# Rig spike 4: prove or break the unproven items

PM [TASK] 2026-11-05. **This note rules nothing** (no library ruling, no dependency added to agent-loop, no secrets used).
Spike code: `spikes/rig-assess-2/` (throwaway, own workspace, `publish = false`). `rig-agent` + `rig-core` 0.46.0 from crates.io;
the model is rig's own `MockCompletionModel` (feature `test-utils`), except the real-provider example. rustc 1.99.1, Windows 11.
Run: `cargo run` in `spikes/rig-assess-2` (`cargo run -- 2` for one item). Predecessor: `RIG-ASSESSMENT.md` (spike 2).

How to read the verdicts, for CireSnave's "lean Rig, keep the door open": **USE-AS-IS** = works on stock 0.46.0;
**SMALL-PATCH** = a change to Rig that is small enough to upstream (lines stated); **REPLACE-THIS-PART** = we write it.
"Seam" is the one of our traits (`Model`, `ToolHost`, `SessionStore`) that confines the item; where none does, it is a
driver-side policy and says so.

The driver under test is **ours**: `src/driver.rs`, 233 lines, uses only `rig_agent::run` (the sans-I/O state machine) and
`rig-core` (model, streaming, messages). Rig's `Agent`/engine/hook stack is not involved except in items 5 and 6.

## 1. Results

| # | Item | Result | Verdict | Seam |
|---|---|---|---|---|
| 1 | Sans-I/O streaming; cut mid-stream; get `AgentRun` back; persist; resume | **PASS.** | USE-AS-IS | Model (stream), SessionStore (run JSON) |
| 2 | Mid-stream injection | **Works for the model; the saved run loses it** on stock. With `AgentRun::inject_message` (19 added lines) the run keeps it. | **SMALL-PATCH** (upstreamable) | SessionStore (what is persisted); the loop |
| 3 | Several tool calls per turn; crash between tools | **PASS.** Only unanswered calls re-run; answered results kept. | USE-AS-IS | ToolHost |
| 4 | Idempotency journal | **Journal works for crash-after-reply (A); cannot cover crash-before-reply (B)**; B is closed by write-ahead ordering, not by the journal. | Journal: REPLACE (ours, 35 lines); Rig: USE-AS-IS | ToolHost (journal), SessionStore (write-ahead) |
| 5 | Context replacement via `on_completion_call` | **PASS.** Run history stays authoritative; request is a derived copy; resume after trimming gives an identical result. | USE-AS-IS (hook); the pairing-safe trim is ours (~12 lines) | none of the three: driver-side context policy |
| 6 | Hooks: Patch/Deny/Stop, `on_outcome`, usage | **PASS** at 8 of 8 stop points tried; Patch, Deny, outcome rewrite all behave. Hooks exist only in Rig's engine, not in the sans-I/O path. | USE-AS-IS if we use the engine; **REPLACE** if we own the loop (our call-outs) | none: loop hooks |
| 7 | Real provider (OpenRouter; "OverMind's chat endpoint") | **NOT RUN.** Example written and compiled; no OverMind chat server exists to call (see 7). | n/a | Model |

### 1. Sans-I/O streaming (`item1`)

Driven by hand: `prepare_request` (`advertise_tools`), `Model::stream`, `StreamedTurnAssembler::ingest` per item, then `stream.finish()`,
`record_streamed_completion_call`, `assembler.finish`, `streamed_turn`. Control run: `"the answer is 3"`, 4 messages, usage total 4.

- Cut after 2 of 5 stream items (mid tool call): the driver still owns the `AgentRun` (state `AwaitingModel`, turn 1). Persisted 901 bytes.
  `stream.partial()` before the drop held **0** parts: a half-streamed tool call is not in it (only parts that ended count). In item 2, cut after one text
  chunk, it held 1 part (the open text), so a host can keep streamed text but not a half-received call.
- Reloaded into a fresh driver and a **new** model script: finished `"the answer is 3"`, **history JSON identical to the control**, turn counter 2.
  Cut after 4 items: `next_step()` twice re-emitted `CallModel` for turn 1, no turn consumed.
- **Cost of the cut:** the cut attempt is not billed to the run (`completion_calls().len() == 0`, `usage()` all `None`), the rustdoc says abandoned
  streams "must still be drained for usage". You cannot both cut early and count that attempt's tokens; the provider bills it regardless.
- Windows-specific findings: none.

### 2. Mid-stream injection (`item2`, `item2b`, `item2c`)

Stock path (abort the stream, re-issue the same `CallModel` attempt after dropping the old stream, `RequestPatch.history` = old history + old prompt,
the injected message as the request prompt):

- The model's request 2 contained the injected message. rig-core's wire shaping **merges adjacent user messages**: the recorded request is one user
  message holding both the original question and the injection, which is fine for providers.
- **The saved run lacks it**, as the TASK predicted: final `serde_json` of the run, `run.full_history()`, and the `CallModel#2` snapshot all lack the
  text; a run resumed from that snapshot sends `U[Q],A,U` to the model on turn 2, i.e. **the model forgets the injection after a restart**.
- Stock workaround (2c): keep the injection as an `Aside` of kind `host.inject` (`append_entry`, saved with the run), and splice it into each request
  from a hook. Works, including across a restart (resumed turn-2 request still contains it). Cost: ~15 lines of splice plus host-owned position
  bookkeeping; `run.messages()`/`full_history()` (what a session export or UI replay reads) still do not contain it.
- Prototype `AgentRun::inject_message(Message)`, valid in `PreparingRequest` and unrecorded `AwaitingModel`, refused elsewhere:
  `spikes/rig-assess-2/patches/inject_message.patch`, **29-line diff, 19 added lines, one file (`rig-agent/src/run/mod.rs`)**, built against a vendored
  copy via `cargo --config 'patch.crates-io.rig-agent.path="vendor/rig-agent"' run --features inject -- 2`. With it: the saved run, `full_history()`
  and the `CallModel#2` snapshot all contain the injection, and the resumed turn-2 request is `U[Q][INJECTED],A,U`. The `AwaitingModel { recorded: true }`
  refusal is by construction and was not exercised.
- **Injection during a running tool batch (2b):** committed messages are `[U, A(2 calls)]` while the batch runs; results are committed only when all are
  answered. Appending the injection there fails `pairing::validate` with `UnansweredToolCall`. The patched `inject_message` returns a protocol violation
  and the run stays drivable (1 unanswered call still asked for). So the host must queue the message until the batch is answered, then inject.
  Stock has no API for this at all; only the Aside route (2c) applies.

### 3. Several tool calls per turn (`item3`)

Turn of three calls (A, B, C); the driver died after answering two. Effects before the crash `["A","B"]`. The persisted run asks for **only `cc`** on reload;
the second life executed `["C"]` and the final history holds A's, B's and C's results (4 messages). Answers live in the persisted batch; the committed
history (2 messages at the crash) gains the results message only when the batch completes.

### 4. Idempotency journal (`item4`)

| Case | World log | Journal hits |
|---|---|---|
| A1: crash after the reply was saved and the tool ran, before its result was fed back; plain host | `["CHARGE","CHARGE"]` (**double charge**, at-least-once as Rig documents) | n/a |
| A2: same, journal keyed by tool-call id (`Journal<H: ToolHost>`, `done: id -> result`, durable as JSON) | `["CHARGE"]` | 1 |
| B: crash before the reply was saved; model asked again, chose a **new call id** | `["CHARGE-100","CHARGE-100"]` | 0 |

B is shown by forcing a stale snapshot (`CallModel#1`) after the tool had run. It cannot happen if the driver **persists the run when `CallTools` is
emitted and before executing anything** (write-ahead): `drive` snapshots at that point, so the reply is durable before the first side effect. The journal
then only has to cover A. A model that re-chooses after a lost reply never reaches a tool without a saved reply. If we ever persist later than that,
a call-id key is useless and the key would have to be content-derived (tool + canonical args), which can also wrongly dedupe two genuine identical calls.

### 5. Context replacement (`hooks::item5`, `item5_resume`)

`on_completion_call` returns `RequestPatch::new().history(trimmed).extra_context(docs)`; docs come from an external store. 3 tool turns then text.

- Turn 4: history 6 → 2 (+ prompt = 3 messages sent), 1 retrieved document. `pairing::validate` on the kept messages: `Ok`. The document text arrives inside the
  user message (the recorded request's `documents` is empty and its text is in `chat_history`): wire shaping folds it in.
- **A cut between a tool call and its result is not stopped by Rig.** The naive trim (keep the first message and the assistant call of turn 2, prompt is
  the result of turn 3) fails `validate` (`OrphanToolResult`), yet the mock received `["U","A(tool_call)","U(tool_result)"]` and the run finished. The mock does
  not model a real provider's 400; we did **not** test a real provider. Rule: cut on turn boundaries and call `pairing::validate` before sending.
- **Authoritative copy: the run history.** After the run it holds all 8 messages, untrimmed; snapshots carry the untrimmed history (contains the first
  tool result). The trimmed request is derived on every turn from the run, so trimming is idempotent and nothing trimmed is ever saved. The store is a retrieval
  copy: lose it and the run is still complete; lose the run and the store cannot rebuild the transcript.
- Resume after trimming (sans-I/O driver, snapshot at `CallModel#3`, i.e. before the trim ever ran): finished identically, **final history JSON equal** to the
  uninterrupted run, turn-4 request again 3 messages.

### 6. Hooks (`hooks::item6`; Rig's `AgentHook` on the streaming runner, mock reporting usage)

- Event order (tool turn): `run_start`, `completion_call`, `model_select`, `dispatch completion`, `text_delta`…, `tool_call_delta`, `outcome completion`,
  `model_turn_finished`, `dispatch ToolCall`, `outcome tool_call`, then the next turn. The completion outcome fires **before** `model_turn_finished`.
- **Stop works at all 8 points tried**: `run_start`, `model_select`, `completion_call`, `text_delta`, `tool_call_delta`, `model_turn_finished`, tool `dispatch`
  (Deny with a `Cancelled` report), tool `outcome`. Each ends the run with `the run was cancelled: <reason>`; the error carries `chat_history` (1 message before
  any tool, 3 after the tool call: prompt, call, closing result) and **not** an `AgentRun` (the spike-2 gap, unchanged). The tool had already run at
  `outcome` (observed), had not at `dispatch`. The task said six points; I could not tell which six, so I tried every hook with a stop path.
  `on_reasoning_delta` and `on_invalid_tool_call` were not exercised.
- **Patch:** `on_dispatch` patched the tool args `x:1,y:2` → `10,20`; the history holds `30`, not `3`. **Deny** with a non-`Cancelled` report: the model sees a
  skipped result (history contains `denied by policy`) and the run continues; with `Cancelled` it ends the run. **`on_outcome`** `rewrite_tool_output` → `REDACTED`
  in history, raw result absent.
- **Usage with a usage-reporting mock:** the hook saw per turn `100/20/120` then `130/5/135` (input/output/total); the final response summed to `230/25/255`.
- **Boundary:** these hooks belong to the engine (`rig-agent/src/agent`, about 9.3k non-test lines of the 13.4k in rig-agent). The sans-I/O driver of items 1-5
  has none; call-outs there are ours (`patch` and `splice` closures in `driver.rs`).

### 7. Real provider

- `examples/real_provider.rs` (101 lines, compiles, exits 2 with `OPENROUTER_API_TOKEN not set`): runs the **same hand-rolled driver** on a real model for
  leg A (streamed tool run, usage from the run), B (cut after 3 items, persist, resume on a new connection), C (abort after 2 items, re-issue with a message patched in).
  Key from the environment, never printed.
  `RP_PROVIDER=compat RP_BASE_URL=… RP_KEY_ENV=… RP_MODEL=…` selects any OpenAI-shaped `/chat/completions` endpoint
  (`OpenAIConfig::new(key).with_base_url(url).with_route(Route::Chat)`); default is OpenRouter. **Not run: no output exists.** Its output belongs in
  `RIG-ASSESSMENT.md` section 4 when the PM runs it under `with-secret`.
- **OverMind has no chat endpoint to call.** At `OverMind` origin/main `2920f08`: a `git grep` over `src`, `*.md`, `pyproject.toml` for `http.server|HTTPServer|fastapi|uvicorn|aiohttp|flask|starlette|axum|serve_forever`
  returned **0 lines**; the same tree answers a positive control (`chat/completions` at `src/overmind/providers.py:743`). OverMind is an OpenAI-compatible **client**
  (`providers.py`: Groq, Google, OpenRouter, NVIDIA, Cloudflare, Ollama at `127.0.0.1:11434/v1`, Mistral, Hugging Face router). The "OpenAI-shaped claim" is
  therefore true only of the endpoints OverMind calls, and `LIGHTBULB-OPENAI-COMPAT-CHECKLIST.md` is a checklist for a server that does not exist in that repo.
  Two facts that bear on Rig: OverMind sends only `model, messages, temperature, max_tokens, tools` and **never `stream`** (checklist 1.4: "zero references to streaming"),
  so nothing OverMind does shows those endpoints stream; Rig's `Route::Chat` streams with `stream: true` and `stream_options.include_usage`. Whether a given OverMind provider honours that is unverified.
  *Which server did the PM mean (Lightbulb? a local OverMind gateway)?* — if one exists, give me its base URL and the example runs unchanged.

## 2. What forking Rig would take

| Crate (0.46.0) | Files | Lines total / excluding tests | MSRV | Role |
|---|---|---|---|---|
| `rig-core` | 444 | 141,478 / 78,111 | 1.97.1 | model, streaming, messages, pairing, providers, mocks |
| `rig-agent` | 74 | 36,937 / 13,448 | 1.97.1 | `run` (sans-I/O) 4,123 + engine, hooks, tool server, builder |
| `rig-memory` | 31 | 9,463 / 6,733 | 1.97.1 | `rig-agent` depends on it |
| `rig-http`, `rig-reqwest`, `rig-tungstenite`, `rig-derive` | 33 | 5,568 / 3,675 | 1.97.1 | transport and derive |

All MIT, all MSRV 1.97.1 (ours is 1.88; raising is permitted, CLAUDE.md s9). Locked graph of the spike: 258 packages.

- **Constraint, not a decision:** CLAUDE.md s9 says cross-repo dependencies are crates.io versions, never git or path. A fork of Rig we depend on therefore means
  publishing our own crate under a name we own, or folding the code in as a module of ours.
- **`RUN_FORMAT = 5` lock:** the envelope is written by `rig-agent` and refused by any other format. Staying on `rig-agent` from crates.io means a persisted run does not survive an upgrade
  that bumps it. If we own the `run` module, the number is ours and the migration question becomes ours.
- **Smallest fork (SMALL-PATCH route):** vendor `rig-agent` only (36.9k lines, 13.4k excluding tests), keep `rig-core` from crates.io. Patch size so far: 19 lines.
- **Replace the engine, keep Rig's state machine (verified):** items 1-4 use only `rig_agent::run`. The engine, hook stack, tool server, builder and `rig-memory` (about 9.3k + 6.7k
  non-test lines) are not needed for them.
- **Replace `rig-agent` entirely, keep only `rig-core` (verified to build and run):** `extract-run-only.sh` copies `rig-agent/src/run` (4,123 lines excluding tests), removes the 7 `mod tests;`
  lines and the one `PromptError::Memory` variant (the only reach into `rig-memory`), adds `http`, and builds against **`rig-core` alone** (cold build 1 m 37 s). `run-only-proof/` then runs the
  spike's unchanged driver on it: cut mid-stream and resume, and crash between tools with only the unanswered call re-running. The `run` module's own unit tests were dropped, not run.
- **What the escape hatch costs us:** hooks (item 6) and the tool server are gone with the engine, so hooks, dispatch Patch/Deny and `on_outcome` become our code. Streaming
  assembly, pairing validation, the model/provider layer and all message types would still come from `rig-core` (78k non-test lines) and stay on its release train.

## 3. What was not measured

Real providers (items 7, 5's orphan-result behaviour). `on_reasoning_delta`, `on_invalid_tool_call`, `Repair`/`Retry`. The `AwaitingModel { recorded: true }` refusal of the patch. The
`run` module's unit tests after extraction. Memory use and wall-clock. `rig-memory` as a `SessionStore` (not touched; the TASK did not ask). The patch was applied to a copy of 0.46.0 only;
whether upstream would accept it is unasked.
