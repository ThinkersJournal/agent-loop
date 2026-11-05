# Goose assessment (spike 3): goose-agent 0.1.0-alpha.13 against the three Rig points

PM [TASK] 2026-11-04 (CireSnave: "Let's do the same sort of checks on Goose and see how the two really compare."). **This note
rules nothing.** Spike code: `spikes/goose-assess/` (throwaway, own workspace, `publish = false`, not a dependency of anything).
Same box and toolchain as `RIG-ASSESSMENT.md` (rustc 1.99.1, Windows 11). Run: `cargo run` in `spikes/goose-assess`.

## 0. How Goose was embedded, and what that means

Goose is an agent *application* (v1.58.0, 14 crates, Apache-2.0); its loop is also published as a library layer. I used the
**crates.io** crates, not the PM's snapshot: `goose-agent` and `goose-provider-types` 0.1.0-alpha.13 (published 2026-10-24),
plus `goose-providers` 0.1.0-alpha.13 for the OpenRouter provider and `rmcp` for tool types. The model is a scripted
implementation of goose's own `Provider` trait (no network); tools are a `ToolProvider`; persistence is a `SessionLoader` +
`EffectHandler` that I wrote (store = the `Conversation` as JSON text). `goose-sdk` (uniffi bindings) and the `goose` crate
(the application's assembly, `goose::agents::state_machine`) were **not** used.

**Finding that matters for any dependency decision:** the snapshot (`goose-main` at 1.58.0) and the published
`goose-agent` 0.1.0-alpha.13 carry the *same version string* but not the same code. The snapshot has `tool_approval`,
`unhandled_tool_calls`, `pre_tool_call_hook`, `post_tool_call_hook`, `stop_hook`, `tool_call_state`; the published crate's
`lib.rs` declares only `events`, `inference`, `machine`, `operation`, `skills`, `subagent`, `tool`
(first compile attempt failed with `could not find tool_approval in goose_agent`). So the snapshot README (which §3 of
`RIG-ASSESSMENT.md` was read from) describes things the crate on crates.io does not contain. Everything below is the
**published** crate; the approval and hook layers are **unmeasured**.

Two embedding costs: goose-agent ships no `InferenceEffect` implementation, so the host writes its own effect type
(`Eff`, about 15 lines); and `record_usage` is an effect the host must decide how to persist.

## 1. Rig vs Goose, per point

| Point | Rig (`rig-agent` 0.46.0, mock model) | Goose (`goose-agent` alpha.13, scripted provider) |
|---|---|---|
| **Persist after the model call is emitted, then resume** | **PASS.** `AgentRun` serialised after `CallModel` (566 bytes), reloaded, re-serialised byte-identical; `resume` on a fresh model finished with `"3"`, history JSON-identical to control; `turn()` not consumed. State is a versioned blob (`RUN_FORMAT = 5`), same-rig-version only. | **PASS, by a different mechanism.** There is no run object to persist; state is the `Conversation` (187 bytes before the call). Persisted before the model call, reloaded on a fresh machine and provider: finished, message shape identical to control (`User:text, Assistant:tool_req, User:tool_resp, Assistant:text`). A crash after the reply but before it was persisted left the store at the user message only; the fresh machine called the model again (2 calls total) and matched control. The format is the host's, not goose's. |
| **At-least-once tools** | Documented: a tool that ran but whose result was not fed back runs again. | **Measured, same behaviour.** Crash injected after the tool ran, before its response was persisted: store held `User:text, Assistant:tool_req`; the tool had run once; on a fresh machine it ran **again** (second counter = 1) and the run finished with `"3"`. |
| **Streaming** | **PASS on mock.** 13 ordered items (tool-call start/deltas/end, text start/text/end, usage and commit markers); `resume(persisted).stream()` also finished. High-level runner returns no `AgentRun` after a stop or cut. | **PASS on scripted provider.** The machine emitted 5 `AgentEvent::Message` events, one per provider chunk (`tool_req`, `tool_resp`, `"The "`, `"answer "`, `"is 3"`). Chunks are **persisted as separate assistant messages** (three `Assistant:text` for one reply; control had one) because the control's reply was one chunk. A host wanting one message per reply must merge. No tool-argument deltas: tool calls arrive complete in one chunk (provider-types doc: "complete tool calls"). |
| **Cancel mid-stream** | `on_text_delta` hook returning `Stop` ended with `PromptError::Cancelled`, history in the error; no run handed back. | Cancel after one chunk: machine returned `Ok`, **the partial `"The "` was persisted** as an assistant message (`finalize_cancellation`). Resume with no new user message made **0 model calls**: a trailing assistant text ends the turn, so continuing needs the host to append a user message or drop the partial. |
| **Transport cut mid-stream** | Provider error; resuming the run persisted before the call finished normally. | The cut became a persisted **assistant error message** after the partial text (`Assistant:other(ERROR)`) and the run returned `Ok`. Resume unchanged made **0 model calls** (`trailing_error` makes inference not apply). A retry needs the host to remove the error message first; I did not test that removal. |
| **Real provider** | **NOT DONE.** The PM's run of `spikes/rig-assess/examples/openrouter.rs` failed: `with-secret: OPENROUTER_API_TOKEN not released - no answer before the wait ran out`, exit 2 (`.lane-state/openrouter-run.txt`). No Rig output exists. | **NOT DONE, same reason.** `spikes/goose-assess/examples/openrouter.rs` builds (it uses goose's own `OpenRouterProvider` via `ApiClient::new_with_tls` with a bearer token) and has not been run; I may not take the secret. Nothing here says anything about a real provider. |

## 2. Against the same standards

| Standard | Rig 0.46.0 | Goose alpha.13 |
|---|---|---|
| Crates.io, newest | Both crates from crates.io | Builds from crates.io. **Pre-release `-alpha.13`**; published crate lags the snapshot (§0). The spike asks `rmcp` `3.4.1` (the snapshot's pin) and Cargo resolved 3.8.0. |
| Footprint | 258 locked packages for the spike | **240 locked packages** including the spike itself, with `goose-providers` (`Cargo.lock`); incremental build of the spike after `goose-agent` compiled took 11 to 21 s; I did not time a cold build. |
| Test doubles | Ship in public crates behind `test-utils` | None needed: `Provider` is a plain trait, implemented in about 35 lines. |
| Resume state ownership | Rig's versioned blob; lost on a rig upgrade that bumps it | The host's `Conversation` JSON (goose's `Message` serde schema, which can change, but is not a machine blob) |
| Tool approval, hooks | n/a | In the snapshot only; not in the published crate |

## 3. Not measured

Real provider for either library; the snapshot's approval/hook/stop-hook layers (not published); several tool calls per
turn; the sans-I/O path of Rig and Goose's `subagent`/`skills` modules; token usage (my `record_usage` drops it);
`goose-sdk`, the `goose` crate and the CLI; removing the persisted error message to retry; wall-clock and memory; licences
of the 240 transitive packages against `deny.toml`; any static analysis (no CodeRipper sweep was run on Goose).
Both comparisons used a scripted model that I wrote, so they test each library's machinery, not what a model does with it.
