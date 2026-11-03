# Loop core (layer C): design note

Status: DRAFT for the PM gate. Nothing here is built. Author: the `agent-loop` lane. Task: PM `[TASK]`, ruled by
CireSnave 2026-11-03 (loop core and adapter first; tools, sandbox and session after). Map: `C:\Projects\AGENT-STACK.md`
(row C). The adapter (row G) is the next note and is designed together with this one; the seam is section 5.

Evidence rule used below: every "read-only fact" carries a URL or path and the date it was read (2026-11-03 UTC unless
stated). Stars and pushes move daily; they show a project is alive, not that it is good.

## 1. The intended hello-world (ten lines)

What we want a consumer to write. This is the target API, not a promise of these names.

```rust
let model  = overmind::client("http://localhost:8080")?;          // layer B, OpenAI-shaped
let tools  = Tools::new().mcp_stdio("synapse-mcp")?.native(ReadFile);   // layer D
let agent  = AgentLoop::builder().model(model, "sonnet-class").tools(tools)
                 .session(Session::in_memory()).build();           // layers C, F
let mut run = agent.start("List the TODOs in src/");               // returns a handle, not a future
while let Some(ev) = run.next().await { println!("{ev:?}") }       // Message | ToolCall | Usage | Done
// run.stop_soft().await;  run.inject("also check docs/").await;   // the adapter's verbs, section 5
```

Properties this implies: the model is a plain chat endpoint (the B|C seam); tools arrive from outside (C|D); a run is a
handle you can observe, nudge and stop, which is the property that makes supervision possible (C|G); the loop owns no
network listener, vault or process spawning.

## 2. What the loop core is, and is not

Job (STACK row C): the turn loop, context assembly and compaction, sub-agents. Not in C: the model (A/B), running a tool
(D), allowing it (E), persisting it (F), reporting outward (G). C calls all of those through traits.

What Claude Code does here (the thing we are replacing for non-Claude minds): call the model, run requested tools,
append results, repeat until the model stops asking; compact when the window fills; spawn sub-agents with their own
context; honour interrupts. **Not verified in this note**: Claude Code is closed source, so its internals are not
read here; this paragraph is the public behaviour only. Treat it as a requirements list, not a spec.

## 3. Survey: what exists to ADOPT before anything is built

All rows measured with `gh api repos/<repo>` and the repo's own files on 2026-11-03. Licence = SPDX id GitHub reports,
confirmed against the LICENSE file where a row says so.

| Candidate | Licence | Lang | Covers | Lacks / risk | Evidence |
|---|---|---|---|---|---|
| **goose-agent** (crate in aaif-goose/goose) | Apache-2.0 | Rust | The loop as an ordered list of `Operation` steps over a persisted conversation; `Inference` step; `ToolOperation` over `rmcp` tools; `CancellationToken` and a defined cancel path (open tool calls answered "interrupted"); `Emitter` event stream (`Message`, `Usage`, `McpNotification`); builds for wasm32, no tokio required | **alpha** (`0.1.0-alpha.13`, crates.io 2026-10-24, 4,929 downloads); depends on `goose-provider-types` and `goose-sdk-types`, so its model seam is goose's, not OpenAI-shaped; sandbox, session store and sub-agents are the host's job | `crates/goose-agent/README.md`, `Cargo.toml` at main; crates.io API |
| **codex-rs** (openai/codex) | Apache-2.0 | Rust | A full agent in ~100 crates: `core`, `tools`, `execpolicy`, `sandboxing`, `linux-sandbox`, `windows-sandbox-rs`, `rollout`, `hooks`, `memories`, `app-server` (JSON-RPC to clients: threads, turns, approvals, interrupts), `agent-identity`, `message-board` | Not a library: a product whose crates are workspace-internal (not checked whether they are published). OpenAI-product-shaped config and auth. Large surface to track | `codex-rs/` directory listing; `codex-rs/app-server/README.md` (read the experimental-features sections only, not the whole protocol) |
| **rig** (0xPlaygrounds/rig; crates `rig-core`, `rig-agent`, `rig-tools`, `rig-harness`) | MIT | Rust | 20+ providers behind one interface; `rig-agent`: agent builder, streaming multi-turn, typed hooks, tool registry, and `AgentRun`, a serializable sans-I/O state machine with a versioned persisted envelope that resumes from any non-terminal state; hooks that can stop the run at six points; `rig-tools` (read, write, edit, search, shell); record/replay (`rig-cassette`); wasm32 for core and agent runtime; MCP via `rig-rmcp` | **No mid-run message injection found** (see "rig-agent read" below). Soft stop is a driver policy, not a method. **MSRV 1.97.1 for the whole workspace** (`Cargo.toml:218`), above the seed's 1.88. Pre-1.0: `rig-agent` 0.46.0, crates.io 2026-10-31, 474,731 downloads, so breaking changes will arrive often. Not compiled here | files at commit `4908f428` (line numbers in "rig-agent read"), crates.io API, 2026-11-03 |
| **rmcp** (modelcontextprotocol/rust-sdk) | LICENSE file: MIT-to-Apache-2.0 transition; new code Apache-2.0, older MIT-only contributions stay MIT (GitHub reports NOASSERTION) | Rust | The official Rust MCP client and server SDK; 3.8.0 on 2026-10-26 | Not a loop. This is D, listed because every loop above sits on it | `LICENSE` first lines; crates.io API |
| **opencode** (anomalyco/opencode) | MIT | TypeScript | Most-starred open coding agent (224k) | TypeScript: cannot be embedded in our Rust crates; usable only as a harness to supervise (a G case) | `gh api` |
| **pi** (earendil-works/pi) | MIT | TypeScript | "unified LLM API, agent loop, TUI, coding agent CLI" | Same: a G case, not a C dependency | `gh api` |
| **OpenHands** | MIT | TypeScript | Coding agent with sandboxed runtime | Same | `gh api` |
| **Agent Client Protocol** (agentclientprotocol/agent-client-protocol) | Apache-2.0 | Rust crate + spec | Editor-to-agent protocol | Facing editors, not fleets; relevant to G (section 5 of the adapter note) | `gh api`, README |

Not surveyed (name only, no claim): LangGraph, Pydantic AI, OpenAI Agents SDK, Claude Agent SDK, Aider, other Rust
crates (`harness-rs-loop`, `swarms-rs`). A web search surfaced `harness-rs-loop` 0.0.1 as MIT OR Apache-2.0; I did not
open it.

### Licence record (PM request 2026-11-03: from the LICENSE file at a commit, not the README)

Root `LICENSE` read via `gh api repos/<repo>/contents/LICENSE?ref=<sha>`, 2026-11-03. This covers the repo-root file
only; per-crate or per-directory licences (a subdirectory with its own terms) were **not** checked, and nothing
should be adopted before they are.

| Repo | Commit read | Root LICENSE text is |
|---|---|---|
| aaif-goose/goose | `cfb67d72` | Apache License 2.0 |
| openai/codex | `f58acd6e` | Apache License 2.0 |
| 0xPlaygrounds/rig | `4908f428` | MIT, "Copyright (c) 2024, Playgrounds Analytics Inc." |
| modelcontextprotocol/rust-sdk | `b166e815` | Transition notice (new code Apache-2.0, older contributions MIT) then Apache-2.0 text; MIT-only contributions remain MIT |
| agentclientprotocol/agent-client-protocol | `971b6cb7` | Apache License 2.0 |
| anomalyco/opencode | `1bf08ae4` | MIT, "Copyright (c) 2025 opencode" |
| earendil-works/pi | `9c9ce854` | MIT, "Copyright (c) 2025 Mario Zechner" |
| OpenHands/OpenHands | `99439d7e` | MIT, "Copyright © 2025 OpenHands contributors" |

These match the SPDX ids in the table above. Not checked: `harness-rs-loop`'s licence (seen only in a search result).

### rig-agent read (PM [TASK] 2026-11-03): stop, inject, resume, MSRV

Read at commit `4908f42820cd587fb2b5274533740da05b2ca282` of 0xPlaygrounds/rig (HEAD on 2026-11-03), files fetched with
`gh api ...contents/<path>?ref=<sha>`. Paths are under `crates/rig-agent/src/` unless stated. Line numbers are of that commit.

| Question | Answer | Evidence |
|---|---|---|
| Resume | **Yes.** `AgentRun` derives `Serialize, Deserialize` with `deny_unknown_fields`; the envelope is versioned (`RUN_FORMAT = 5`) and a run of another format is refused by name. `next_step` re-emits the pending step; a persisted run resumes from any non-terminal state **except a pending invalid-tool-call resolution**. A re-issued `CallModel` is the same attempt and costs no turn; the driver must drop the earlier call first | `run/mod.rs:296-370`, `run/mod.rs:829-845`, state enum `run/mod.rs:270-288` |
| Stop | **Yes, by hook or driver, not by a method on the run.** Hooks return `Stop(String)` at run start, model selection, completion call, model-turn finish, and observation; tool dispatch/outcome can return a `Cancelled` error that cancels the run. `cancel_error(reason)` returns an error carrying `canonical_history`, in which unanswered tool calls are closed. Because the run is sans-I/O, a **soft stop at a step boundary is simply the driver not calling `next_step` again and persisting the run** | `agent/hook.rs:439-445, 521-527, 858-865, 890-896, 918-922, 639-649, 788-794`; `run/mod.rs:778-786` |
| Inject | **Not found.** The public methods that touch history are `new` (initial prompt), `with_history`, `with_validated_history`, `append_entry` (host records "excluded from provider requests"), and a rewrite of the initial prompt that is refused once the run has started ("the initial prompt can only be rewritten before the run starts"). No method appends a user message to a started run. A driver would deliver an injected message by finishing or stopping the run and starting the next with `with_history` | `run/mod.rs:393, 440-446, 450-452, 486, 1757`; `pub fn` list of `run/mod.rs` |
| Budget / cost | `max_turns`, cumulative `usage()`; per-turn `Usage` on `ModelTurn`; example `examples/run_budget.rs` exists (not read) | `run/mod.rs:498, 603, 101-120` |
| MSRV | **1.97.1**, workspace-wide, edition 2024; `rig-agent` inherits it | `Cargo.toml:216-218`; `crates/rig-agent/Cargo.toml:5` |
| Published | `rig-agent` 0.46.0 on crates.io (2026-10-31, 474,731 downloads); `rig-core` 0.46.0; `rig-tools` 0.46.0 (2,619 downloads); `rig-harness` 0.46.0 (416) | crates.io API, 2026-11-03 |

Control for the "Inject: not found" absence: the same listing of `pub fn` in `run/mod.rs` does find `append_entry`,
`with_history`, `cancel_error` and `next_step`, so the query sees public methods; it does not prove no path exists in
`agent/` (the higher-level agent builder and runner were only grepped for `cancel|abort|timeout|CancellationToken`).
A `CancellationToken` / `abort` grep in `agent/runner.rs` found no cancel API there either, but that file was not read whole.

### Reading of the survey (a recommendation, not a ruling)

- **No candidate is shown to satisfy the hello-world as is, but the rig read (below) narrows it.** Two Rust
  candidates: `goose-agent` (Apache-2.0, alpha, its own provider seam, documented cancel path) and `rig-agent` (MIT,
  0.46.0, serializable resumable state machine, six stop points, MSRV 1.97.1).
- **Lean (revised after the rig read, replacing the earlier goose-agent lean): build `agent-loop`'s `Run` as a driver
  over rig's `AgentRun`, behind our own `Model`/`ToolHost`/`SessionStore` traits; do not fork it; keep `goose-agent` as
  the fallback.** Reasons, each with its evidence below: the state machine is already serializable and resumable, so
  a restarted lane resumes (the property G and AgentLife need); it owns no I/O, so our `Run` handle, soft stop and
  event stream are driver code we control; its `rig-agent` has about 96 times the downloads of `goose-agent` (474,731 against 4,929) and is a smaller bet than an alpha.
  Costs: MSRV 1.97.1 (CireSnave 2026-10-08, via `C:\Projects\CLAUDE.md` section 9: raising a project's minimum Rust version
  is permitted), pre-1.0 churn, and no mid-run injection (we deliver injected messages at a step boundary).
- **This is still a recommendation.** Neither crate has been compiled here; the next gate is a throwaway spike that
  drives one `AgentRun` against a fake `Model` and round-trips it through serde, before any dependency is added.
- **Do not adopt codex-rs as a dependency.** Use it as the reference for E (sandbox) and the app-server surface for G.
- **TypeScript harnesses are G cases.** We supervise them; we do not embed them.

CireSnave's rules that bear on this: dependencies at their newest versions (a pinned alpha means a bump on every
release; rig-agent and goose-agent are both on crates.io), crates.io not git/path deps, and provenance and credit kept for anything we
copy rather than depend on.

## 4. Design

The loop core is a library crate `agent-loop`, no binary, no listener.

1. **Run handle.** `AgentLoop::start` returns a `Run`: an event stream plus control verbs (`inject`, `stop_soft`,
   `stop_hard`, `usage`). Soft stop means finish the current step, leave the conversation consistent, mark every open
   tool call answered. Both candidates do the last part on cancel (rig: `AgentRun::cancel_error` closes unanswered
   calls, `run/mod.rs:778-786`; goose-agent: README "Cancellation"). With rig, soft stop = stop calling `next_step`
   and persist; `inject` = queue the message and deliver it at the next step boundary or the next run, because no
   mid-run append exists (see "rig-agent read").
2. **Steps, not a monolith.** The run's state is a serializable state machine (rig: `AgentRun`; goose: a persisted
   conversation walked by an ordered list of steps), never loop-local variables. Inference, tool execution,
   compaction, approval-wait and sub-agent spawn are each a step. This is what lets a restarted lane resume.
3. **Three seams as traits:** `Model` (chat plus tool schemas, B|C), `ToolHost` (tool call in, result out, C|D),
   `SessionStore` (C|F). In-memory implementations ship in the crate so the hello-world runs.
4. **Events are the only outward channel.** The loop emits typed events; G (next note) maps them to identity, activity,
   cost. The loop never talks to Synapse itself.
5. **Compaction is a step with a policy trait**, because C and F are coupled (STACK: the context window belongs to A,
   the loop decides what fills it). Default: summarise oldest turns via the same model.
6. **Sub-agents are runs started by a tool,** each with its own conversation and budget, reported to the parent as
   events.

## 5. Seam to the adapter (G)

The adapter note defines the outward verbs. C's obligation: every one must be answerable from `Run` and its events
without reaching into loop internals. The list is in `docs/HARNESS-ADAPTER.md` (merged, PR #2).

## 6. Open questions (for the PM, not decided)

1. Adopt `rig-agent`'s `AgentRun` behind our traits (current lean), keep `goose-agent` as the fallback, or write the
   loop? Writing the state machine ourselves is small only if we match rig's resume guarantees; adopting means
   MSRV 1.97.1 and tracking a pre-1.0 line. Needs the spike (section 3) before it is a decision.
2. Does the first consumer use OverMind's real chat endpoint? I have not read OverMind's current API; the OpenAI-shaped
   claim is from STACK seams, not from OverMind's code.
3. Windows is the primary platform. Neither crate was built here; goose-agent documents native and wasm, rig-agent
   documents wasm32-unknown-unknown (`crates/rig-agent/README.md:239`) and not WASI.

## 7. What I did not verify

- Claude Code internals (closed).
- Whether codex-rs crates are published to crates.io.
- That `goose-agent` compiles and runs a loop on our toolchain (Rust 1.88 per the seed `Cargo.toml`): not built.
- `rig-agent`: read `run/mod.rs` (module doc, state, envelope, `next_step` doc, public method list), `agent/hook.rs`
  action enums, and `Cargo.toml`; **not read whole**: `agent/runner.rs`, `agent/engine.rs`, `run/streamed.rs`,
  `examples/run_budget.rs`. No `AgentRun` was serialized or driven; resume is from its documentation and types, not a run.
- That `goose-agent` supports mid-run injection (it appends via `AppendMessage` effects between passes by its README,
  not verified).
- The 2026-11-03 date is the `pushed_at` timestamp `gh api` returned; the lane's clock said 2026-11-02.
