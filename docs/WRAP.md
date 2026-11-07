# Wrapping rig's types behind our traits (scoping note)

Status: **proposal, nothing built, not decided.** Read at `origin/main` `6f789e4` (0.0.23). This note answers "what does the
public API of `agent-loop` expose from rig today, what would it cost to stop, and what should go first". The decision is the PM's
(it is a breaking API change; pre-1.0 that is a 0.n bump, which the PM allocates).

## 1. Why this is on the list

CireSnave's board 373 Q1 chose rig (`A`) and kept goose-agent as the fallback (`LOOP-CORE.md`, `GOOSE-ASSESSMENT.md`). The slices
so far were built "behind our own `Model`, `ToolHost` and `SessionStore`" so that a fallback would cost one crate, not every
consumer. That promise is only half kept: the traits are ours, but their signatures still name rig types, so a consumer's `impl
Model` or `impl ToolHost` already depends on `rig-core`'s exact version.

## 2. What leaks today (measured: `grep` over `src/` at `6f789e4`)

| Where | Rig type in our public signature | Who is touched |
|---|---|---|
| `driver.rs:29` `Model::capabilities` | `rig_core::completion::ProviderCapabilities` | every `Model` impl |
| `driver.rs:32-33` `Model::stream` | `CompletionRequest` in, `Result<CompletionStream, ProviderError>` out | every `Model` impl, and anything that builds a stream |
| `driver.rs:60` `ToolHost::definitions` | `Vec<Arc<ToolDefinition>>` (also `journal.rs:56`, `writeback.rs:173`) | every `ToolHost` impl, and every tool author |
| `driver.rs:112,124` `Driver::new`, `pub spec` | `rig_agent::run::RunSpec` | whoever constructs a driver |
| `driver.rs:218,264` `Driver::open`, `drive` | `rig_agent::run::AgentRun` (returned and taken by `&mut`) | whoever runs a driver |
| `context.rs:23` `ContextPolicy::shape` | `Arc<rig_core::message::Message>` in and out | every policy author |
| tests | `MockCompletionModel`, `MockStreamEvent` (rig `test-utils`) | our tests, and any consumer test of a `Model` |

Not leaked: the store and inbox (`String` snapshots and ids), events, hooks (they use our `ToolCall` and `ToolOutcome`), the
change-set and workspace types, `RunControl`.

## 3. Findings that shape the options

1. **The saved snapshot is rig's `AgentRun` JSON** (`RUN_FORMAT=5`, `deny_unknown_fields`, `driver.rs` `snapshot`), whatever we
   wrap around it. A session saved by the rig-backed driver cannot be resumed by a goose-backed one. So wrapping the *types* makes a
   fallback cheap for **new sessions**; it does not make saved sessions portable. That would need our own run state, which is a
   rewrite of what `rig-agent`'s `run` module gives us, and is out of scope here.
2. `AgentRun` is already only touched through the driver, so hiding it behind an opaque `Run` is mostly renaming.
3. `Message` is the expensive one: tool calls and results carry ids and call ids, and rig's messages also carry content kinds we do
   not use yet (images, reasoning blocks). **Unmeasured:** whether a conversion to our own message type and back is lossless for the
   cases the loop uses; that needs a round-trip test over rig's own fixtures before committing to it.
4. Nothing in this repo or in the lanes' notes says a consumer has asked for this. The first consumer will be the AntSpawn-backed
   sandbox slice (B1/B2 slice 4) and the agentlife adapter; their needs are the real input, and I have not asked them.

## 4. Options

- **A. Re-export, do not wrap.** `pub use rig_core::...` the types our API names, so a consumer depends on `agent-loop` alone and
  gets the matching rig. Zero breaking change now. Cost: our semver is rig's (a rig minor bump is a breaking bump for us), and a
  fallback still costs every consumer.
- **B. Wrap everything** (`Message`, `Request`, stream chunks, `ToolDef`, `Capabilities`, `Error`, `RunConfig`, `Run`) with a
  `rig` adapter module doing the conversion at the edge (`RigModel<M>` implements our `Model` over a rig model). Rig becomes an
  implementation detail. Cost: the largest breaking change, my estimate is about a thousand lines (not measured), the message and
  stream conversion being the bulk of it, and finding 3 is unmeasured.
- **C. Stage it,** smallest leaks first, each stage its own PR and its own 0.n bump:
  1. `Run` and `RunConfig` as opaque newtypes over `AgentRun` and `RunSpec` (finding 2; no conversion, about 150 lines).
  2. Our own `ToolDef` (name, description, JSON schema) and `Capabilities`, converted in the driver; `ToolHost::definitions` and
     `Model::capabilities` stop naming rig (small, the conversion is a field copy).
  3. Our `ModelError`, and our `Message` plus a `ModelRequest`/chunk-stream pair with a `RigModel` adapter, after the round-trip
     test of finding 3 passes. This is the stage that makes a goose-backed `Model` possible.
  Stop after any stage if the next one has no consumer asking for it.

## 5. Recommendation

**C, and start with stage 1.** It removes `AgentRun` and `RunSpec` from the signatures at near-zero risk, and it is the only stage
whose value does not depend on a consumer's answer. Stages 2 and 3 should wait for AntSpawn and agentlife to say whether they will
implement `Model` or `ToolHost` themselves; if they will not, option A's re-export is enough and stage 3 may never be worth its
cost. Either way, say in the README that the snapshot format is the engine's (finding 1).

## 6. Open questions (for the PM, not decided)

1. Do AntSpawn or agentlife implement `Model`, `ToolHost` or `ContextPolicy` themselves, or only use `Driver`? (Decides whether
   stages 2 and 3 have a customer.)
2. Is "a saved session cannot move between engines" acceptable for the fallback story (finding 1)?
3. Version allocation: each stage is breaking, so 0.n each time, or one allocation for the series.
