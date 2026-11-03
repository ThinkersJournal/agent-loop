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
| **rig** (0xPlaygrounds/rig; crates `rig-core`, `rig-agent`, `rig-tools`, `rig-harness`) | MIT | Rust | 20+ providers behind one interface; `rig-agent`: agent builder, streaming multi-turn, typed hooks, tool registry, serializable `AgentRun`; optional subagents and checkpoints; `rig-tools` (read, write, edit, search, shell); record/replay of an agent's effects (`rig-cassette`); wasm32 for core and agent runtime; its own coding agent (Bevy-plugin based) | Its own coding agent needs Rust 1.97.1+ (README), so check the MSRV of the library crates before depending. Whether `AgentRun` supports soft stop, inject and resume is **not read**. `rig-core` 0.46.0, crates.io 2026-10-31 | repo `README.md` (crate table), crates.io API, 2026-11-03 |
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

### Reading of the survey (a recommendation, not a ruling)

- **No candidate is shown to satisfy the hello-world as is.** Two Rust candidates are close on different axes:
  `goose-agent` has the right loop shape and documented cancel semantics but an alpha API and its own provider seam;
  `rig` is further along (serializable `AgentRun`, hooks, subagents, 20+ providers, MIT) but I have not read whether a
  run can be stopped softly, injected into, or resumed. **Deciding between them needs that read; do it before any build.**
- **Lean, provisional until the rig read: adopt `goose-agent` behind our own trait, do not fork it, pin the version.** The loop is a pure function
  of a persisted conversation, which gives resume and "soft stop" nearly for free. We write a thin provider that
  speaks OpenAI-shaped chat to OverMind and implements goose's `Provider`. If upstream churn is too high, our trait
  lets us swap in a loop we write ourselves.
- **Do not adopt codex-rs as a dependency.** Use it as the reference for E (sandbox) and the app-server surface for G.
- **TypeScript harnesses are G cases.** We supervise them; we do not embed them.

CireSnave's rules that bear on this: dependencies at their newest versions (a pinned alpha means a bump on every
release), crates.io not git/path deps (goose-agent is on crates.io), and provenance and credit kept for anything we
copy rather than depend on.

## 4. Design

The loop core is a library crate `agent-loop`, no binary, no listener.

1. **Run handle.** `AgentLoop::start` returns a `Run`: an event stream plus control verbs (`inject`, `stop_soft`,
   `stop_hard`, `usage`). Soft stop means finish the current step, leave the conversation consistent, mark every open
   tool call answered. goose-agent already does the last part on cancel.
2. **Steps, not a monolith.** Adopt the "ordered list of steps over a persisted conversation" shape: inference, tool
   execution, compaction, approval-wait and sub-agent spawn are each a step. The run's state is the persisted
   conversation, never loop-local variables. This is what lets a restarted lane resume.
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
without reaching into loop internals. The list is in `docs/HARNESS-ADAPTER.md` (next PR).

## 6. Open questions (for the PM, not decided)

1. Adopt `goose-agent` (alpha) behind our trait, or write the loop? The cost of writing is small if the loop is the
   step machine above; the cost of adopting is tracking alpha releases.
2. Does the first consumer use OverMind's real chat endpoint? I have not read OverMind's current API; the OpenAI-shaped
   claim is from STACK seams, not from OverMind's code.
3. Windows is the primary platform. goose-agent builds for native and wasm; I did not build it here.

## 7. What I did not verify

- Claude Code internals (closed).
- Whether codex-rs crates are published to crates.io.
- That `goose-agent` compiles and runs a loop on our toolchain (Rust 1.88 per the seed `Cargo.toml`): not built.
- `rig-agent` semantics (stop, inject, resume, MSRV of the library crates): README crate table only.
- The 2026-11-03 date is the `pushed_at` timestamp `gh api` returned; the lane's clock said 2026-11-02.
