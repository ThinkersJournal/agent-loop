# Rig assessment (spike 2): rig-agent 0.46.0 against our needs and standards

PM [TASK] 2026-11-04: inspect Rig and judge it against our standards before any library ruling. **This note rules
nothing.** Spike code: `spikes/rig-assess/` (throwaway, own workspace, `publish = false`, not a dependency of anything).
Built with rustc 1.99.1 on Windows 11; `rig-agent` and `rig-core` 0.46.0 from crates.io, models are rig's own
`MockCompletionModel` (feature `test-utils`). Run: `cargo run` in `spikes/rig-assess`.

## 1. The three points

| Point | Result | What was measured |
|---|---|---|
| Persist after `CallModel` emitted | **PASS** | `AgentRun::new(..)` then `next_step()` gave `CallModel`; serialised (566 bytes), reloaded, re-serialised byte-identical. `agent.resume(reloaded)` against a fresh model finished with text `"3"` and a message history JSON-identical to an unpersisted control (4 messages). A second `next_step()` on a reloaded run re-emitted `CallModel` and `turn()` stayed 1 (no turn consumed). |
| Streaming | **PASS on the mock, with a gap** | `agent.prompt(..).stream()` over a scripted stream gave 13 items in order: tool-call start, two argument deltas, tool-call end, `CompletionCall`, `ToolCall`, `ToolExecutionCommitted`, `ToolResult`, text start/text/end, `CompletionCall`, `FinalResponse`. `agent.resume(persisted).stream()` also finished with `"3"`. |
| Real provider | **NOT DONE** | No provider credential was available to this lane, and no local model server answered on `localhost:11434`. I did not look for a key in the secret vault (the attempt was refused as credential exploration; I stopped). Needs a key or a local endpoint from the PM or CireSnave. Nothing here says anything about a real provider. |

Failure legs, same mock: a hook returning `Stop` from `on_text_delta` ended the stream with `PromptError::Cancelled`, whose
`chat_history` field holds the history (its docs say every tool call is answered). A stream cut by a transport error
(`connection reset` after one text chunk) ended with a provider error; resuming the run persisted **before** the call
then finished normally.

**The gap:** the high-level runner hands back no `AgentRun`. After a stop or a cut the host has the error's history, not
the run. A host that wants to persist at step boundaries mid-stream must either drive `AgentRun` itself (the sans-I/O
path, which needs `prepare_request`, `StreamedTurnAssembler`, `record_streamed_completion_call` and `streamed_turn`: not
built here) or persist the run before each model call as above and discard the partial stream. I found no accessor on the
stream or runner (grep of `agent/runner.rs`, `agent/streaming.rs` for `AgentRun`: only `Resume(AgentRun)` as input); that is
absence from a grep, not a proof.

**Not measured:** a run persisted mid tool batch via the runner (the serde spike covered the sans-I/O path); several tool
calls per turn; pending invalid-tool-call resolution (documented non-resumable); the sans-I/O streaming path; hooks other
than the one stop; real token usage (the mock reports none, so `usage` printed all `None`); wall-clock or memory.

Spike 4 (`RIG-SPIKE-4.md`) measured the sans-I/O streaming path, several tool calls per turn, hooks other than the one stop, and usage with a mock that reports it. One real provider (OpenRouter free tier, `nvidia/nemotron-3.5-lightning:free`) now ran the three legs with real tokens (`RIG-SPIKE-4.md` section 7); the default Gemma model returned 429 and no local server was up.

## 2. Against our standards

| Standard (source) | Finding | Evidence |
|---|---|---|
| Newest dependencies; MSRV may rise (CLAUDE.md s9) | Fine: 0.46.0 is current as of 2026-11-03 (crates.io); MSRV 1.97.1 is above our 1.88 and permitted to raise | LOOP-CORE s3 |
| Crates.io, not git or path (s9) | Fine: both crates build from crates.io | `Cargo.toml` of the spike |
| Test doubles ship in the public crates | **Concern.** `MockCompletionModel`, `MockAddTool` and the conformance suites live in `rig-core::test_utils` / `rig-agent::test_utils` behind feature `test-utils`; usable, but a feature of a published crate we would not want in a production build, and the way to script a model is a mock with no stable-API promise | `rig-core` `lib.rs:77-79` |
| Pre-1.0 churn | **Concern.** 0.46.0; `RUN_FORMAT = 5` and a refusal of other formats mean a persisted run does not survive a rig upgrade that bumps it. `resume` docs: "The run must have been suspended by the same rig version." A lane restart across an upgrade loses in-flight runs unless we migrate | `agent/completion.rs` `resume` doc; LOOP-CORE s3 |
| Footprint | Spike resolves **258 locked packages** (159 unique in `cargo tree -e normal`); rig's own are `rig-agent`, `rig-core`, `rig-derive`, `rig-http`, `rig-memory`. Compile of the spike from cold took over two minutes on this box | `Cargo.lock`, `cargo tree` |
| Two crates needed | `rig-agent` alone is not enough: the driver needs `rig-core` types directly | Spike 1 (LOOP-CORE s7) |
| At-least-once tool execution on resume | **Behaviour to design around.** `resume` docs: a tool that ran but whose result was not fed to the run runs again. Tools with side effects (E, D) need idempotence or a journal | `agent/completion.rs` `resume` doc |
| Mid-run injection | Absent (LOOP-CORE s3); unchanged | LOOP-CORE s3 |
| Static analysis | CodeRipper sweep of `rig-core` (`C:\Projects\.lane-state\rig-out\crates_rig-core.jsonl`): 193 lines parsed by me, of which 191 are `reachability`/`absence` findings (163 `info`, 28 `medium`) and 2 are run-summary lines. The PM's note says 209 findings and all 38 sampled were public-API constants with 0 real defects; **my count differs from 209 and I did not re-triage any finding**, so I neither confirm nor dispute that. Recall is unmeasured | the file |

What I cannot judge from this spike: code quality beyond the surface I touched, soundness of `unsafe`, security of the HTTP
layer, provider-specific behaviour, licence of transitive dependencies (the 258 were not checked against `deny.toml`).

## 3. Goose, same three points

Measured in `GOOSE-ASSESSMENT.md` (spike 3, published `goose-agent` alpha.13). That note replaces the README-only reading
that stood here: the README describes the snapshot at 1.58.0, whose approval and hook layers are not in the crate on crates.io.

## 4. Real-provider leg

`spikes/rig-assess/examples/openrouter.rs` (OpenRouter, free-tier model, key from `OPENROUTER_API_TOKEN`, never printed) is written and compiles; it has **not been run against a provider**. The PM's run (`.lane-state/openrouter-run.txt`) ended `with-secret: OPENROUTER_API_TOKEN not released - no answer before the wait ran out`, exit 2, so there is no output to record. Its output goes in this section when a run succeeds.

Originally needed:

A provider key via the approved `with-secret` route (name and lane access set by the PM or CireSnave), or a local endpoint.
Then: `rig-core` provider client, one tool call, streamed, persisted before the call, killed mid-stream, resumed.
