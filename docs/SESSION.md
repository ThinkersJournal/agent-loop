# Session and memory (layer F): design note

Status: DRAFT for the PM gate. Nothing here is built. Author: the `agent-loop` lane. Task: PM `[TASK]` 2026-11-03 (notes D, E, F
after C and G). Map: `C:\Projects\AGENT-STACK.md` (row F; seam C|F). Companions: `docs/LOOP-CORE.md`, `docs/HARNESS-ADAPTER.md`,
`docs/TOOLS.md`, `docs/SANDBOX.md`. Read-only facts carry a file or URL and a ref or date (2026-11-03 UTC unless stated). Refs:
rig `4908f428`, goose `cfb67d72`, codex `f58acd6e`, PersistAnt `32ccc430`, all fetched 2026-11-03.

## 1. The intended hello-world (ten lines)

What the loop host writes to make a run survive a restart. Target API, not final names.

```rust
let store = SessionStore::open(Dir::new("sessions/"), Needs { durability: Durability::Process })?;   // refused if the backend cannot
let mut s = store.open_or_create("kiss")?;                  // one session per role; returns the log as it stands
let run = match s.snapshot()? { Some(snap) => AgentRun::resume(snap)?,      // C: sans-I/O state machine, serialised
                                None       => AgentRun::new(&prompt) };
for step in run.drive(&model, &tools, &policy) {            // C, D, E do their jobs
    s.append(step.record())?;                               // optimistic append: refused if another writer got there first
    if step.is_boundary() { s.snapshot_now(&run)?; }        // a durable resume point at every step boundary
}
s.write_handoff(wrap_up(&run))?;                            // the human-readable state, rewritten not appended
```

Properties: **append-only log plus a resume snapshot**; **two writers cannot both win** (the double-launch failure AgentLife already
paid for); the store declares what durability it gives and is refused if it cannot give what was asked; the model-written summary
for the next session is a separate, small, rewritten document.

## 2. What F is, and is not

STACK row F: transcript, resume, memory store. The PM's 2026-10-31 reading adds that C and F are tightly coupled: the loop decides
what fills the context window (compaction), and the session store is what it reads and writes; a durable key-value layer underneath is
separable (STACK suggests PersistAnt fits).

F is four things, which this note keeps apart:

1. **Transcript:** the durable, append-only record of what happened (messages, tool calls and results, usage, events).
2. **Resume point:** enough state to continue a run after a restart (the serialised loop state, not a replay).
3. **Compaction state:** summaries that replace old history in the model's context while the transcript stays whole.
4. **Memory:** facts that outlive a session (the fleet's `HANDOFF.md`, and file-based memory).

Not F: the loop (C), tool protocol (D), policy (E), or fleet-level supervision (H).

## 3. What exists today

### 3.1 In our fleet (requirements, not designs to copy)

| Item | What it does | Evidence |
|---|---|---|
| Claude Code transcript | One JSON-lines file per session, `~/.claude/projects/<project>/<session-id>.jsonl`. This session's file on this machine: 3,131,487 bytes, 1,317 lines, **20 record types** (counted by `type`: `user`, `assistant`, `attachment`, `system`, `api-request`, `queue-operation`, `file-history-snapshot`, `permission-mode`, `pr-link`, `custom-title`, ...). Records link by `parentUuid` and carry `isSidechain`, so the log is a tree, not a list. **`queue-operation` records exist (keys include `operation`, `queuePriority`); that they persist queued user messages is inferred from the names, not read** | read 2026-11-03 from the live file, record **types and key names only** (content not read). The on-disk format is undocumented and not a stable interface |
| Claude Code memory | A directory of one-fact files with frontmatter and a `MEMORY.md` index loaded each session | `~/.claude/projects/<project>/memory/`, described in this session's own instructions |
| HANDOFF.md | Current state for the next session; **a rewrite, not an append**; ceiling 8 KB for a lane, 20 KB for the PM; `WAIT:` lines are parsed and `assert-idle` refuses a malformed one | `C:\Projects\CLAUDE.md` section 9; `lane-restart --help` |
| Lane state and ledger | `<role>.json` (hook-written activity), `<role>.launch.json` (the way back), `restart.log` (append-only JSON lines) | agentlife `docs/SUPERVISION-INPUTS.md` 1.1 at `a5c06136` |
| Failure already paid for | A role got a **second session** while the first was stuck, and state files outlive processes. A store must make "two writers" impossible or visible | `SUPERVISION-INPUTS.md` section 2, failures 2 and 3 |
| Atomic write rule | The spec is written complete (temp file, `sync_all`, rename, read back, compare) **before** the kill | `SUPERVISION-INPUTS.md` section 3 (`write_atomic`) |

### 3.2 Candidates to ADOPT

Method as before: `gh api`, crates.io API, files at named commits. Licence = root LICENSE text at the commit (rig, goose and codex
rows were read for `LOOP-CORE.md`; PersistAnt's two licence files were read at `32ccc430`).

| Candidate | Licence | Covers | Lacks / risk | Evidence |
|---|---|---|---|---|
| **rig-memory** (rig) | MIT (`4908f428`) | Sessions saved as JSON-lines logs, format `rig-session/4`, **resumable, rewritable, and forkable up to a turn boundary**; recent-messages window and compaction; `DirStore` (feature `fs`), `SqliteSessionStore`, `PostgresSessionStore`; any `AsyncStore`. **A store appends only if the log is still as long as the writer read it, checked and appended in one step: of two saves that read one log, the second is refused.** A session in another format is refused untouched; a migration chain keeps a copy under `rig/kept/`. A conformance suite (`conformance::check_store`) every store runs. Builds for wasm32 by default. 0.46.0, crates.io 2026-10-31, 41,110 downloads | Tied to rig-core's message types; pre-1.0 (MSRV 1.97.1, which is permitted to take, see `LOOP-CORE.md`). Windows `DirStore` behaviour not run. Durability class not stated in the README | `crates/rig-memory/README.md`, `Cargo.toml`; crates.io API |
| **goose-context-management** (goose) | Apache-2.0 (`cfb67d72`) | Compaction: `summarize(model, messages)` to one summary message; trait-based `compact` over caller-owned conversation types; `TokenEstimator`; `DEFAULT_COMPACTION_THRESHOLD = 0.8`; structured summary including **which files the conversation touched** | Alpha (0.1.0-alpha.13, 1,380 downloads); tied to goose provider types; a prompt-and-policy library, not a store | `crates/goose-context-management/README.md`, `Cargo.toml` |
| **codex thread-store / rollout** (openai/codex) | Apache-2.0 (`f58acd6e`) | "Storage-neutral thread persistence interfaces": `ThreadId` as the only durable handle, local rollout files or RPC behind it; separate modules for attachments, sections, projects, and a **durable queue of user submissions** (`queue_store.rs`, `MAX_QUEUE_ITEMS`) | Workspace-internal as far as checked (other `codex-*` crates are not on crates.io; these two names were not queried). A reference for the interface | `codex-rs/thread-store/src/lib.rs`, directory listing |
| **PersistAnt** (ours, `ciresnave/PersistAnt`) | MIT OR Apache-2.0 (`LICENSE-MIT` © 2026 Eric Evans, `LICENSE-APACHE` at `32ccc430`) | A thin layer over Apache OpenDAL for blobs and records by key. **A program states the guarantees it needs; a backend that cannot give them is refused by name.** Atomic replace on `fs` and `memory`; typed records with `SCHEMA` versions and `upgrade`; in-memory, capability-masking and fault-injecting fakes; a blocking facade; backend ranking by evidence. 0.16.8, crates.io 2026-11-01, 7,434 downloads, MSRV 1.94 | **Machine-level (power-loss) durability is UNVERIFIED and stays declared**: verified only for guest power loss on ext4 under Hyper-V (ROADMAP). No log or append abstraction read; kinds are kv, blob, relational | `README.md`, `ROADMAP.md`, `Cargo.toml` |
| **file-based memory + HANDOFF** (the fleet's own convention) | n/a | Already works for lanes today and is what a restarted lane reads | Convention, not a library; no atomicity, no schema | `CLAUDE.md`; this session's memory instructions |

Not surveyed (no claim): LangGraph checkpointers, Letta/MemGPT-style memory, vector stores, OpenHands' event store.

### Reading of the survey (recommendation, not ruling)

- **Our `SessionStore` is a trait** (the C|F seam from `LOOP-CORE.md`), small: `open_or_create`, `append(record)` with an
  **expected length** (optimistic append), `snapshot`/`write_snapshot`, `fork(at_boundary)`, `list`.
- **If C adopts rig-agent, adopt rig-memory's log format and DirStore** rather than inventing a log: it already has the three
  properties we need (resume, fork at a turn boundary, refuse the second writer) and a conformance suite to prove a new store.
  If C does not adopt rig-agent, take the *rules* (append only if length unchanged; refuse other formats untouched; keep a copy
  on migration) and write the store ourselves.
- **Put durability where it is declared: PersistAnt.** Use it for anything that must survive and be read back (snapshots,
  the durable inject queue, memory records) so a backend that cannot give the stated durability is refused at open. Do not
  claim `Machine` durability on Windows: PersistAnt itself says it is unverified.
- **Compaction: reuse the idea, not the crate yet.** goose-context-management is alpha and tied to goose types; the part worth
  taking is the structured summary that records which files were touched and the 0.8-of-window default trigger. The summariser
  prompt is ours to own (it is policy).

## 4. Design

### 4.1 Records and the log

One session per `(role, session id)`: an append-only log of typed, schema-versioned records, each with a monotonically increasing
sequence number: `Message`, `ToolCall`, `ToolResult`, `Usage`, `Event` (from `HARNESS-ADAPTER.md`), `Queued` (a message
injected for the next step boundary), `Compaction{replaces: range, summary}`, `Decision` (from E's audit line, by reference).
**Compaction never deletes**: it appends a marker, and the model's context is built from the log by skipping what a compaction
replaced. The transcript stays whole for audit and replay.

### 4.2 Resume

A snapshot is the serialised loop state (rig: `AgentRun`, envelope `RUN_FORMAT = 5`; format refused by name if different) plus the
sequence number it corresponds to. Resume = load the latest snapshot, replay log records after its sequence number into the run's
pending queue. A snapshot is written at every step boundary C defines; writes use the atomic rule (complete, then rename, read
back). **A run is never resumed from a log without a snapshot written by the same format**; an unreadable one stops and reports
(`unknown` is unsafe, `HARNESS-ADAPTER.md` section 3).

### 4.3 One writer per session

Every append names the length it read (`expected_len`). A mismatch returns `Conflict`; the loop host stops and reports, it does not
retry blindly. This is the store-level form of AgentLife's double-launch guard and gives G a `Conflict` event to report.

### 4.4 Inject survives a restart

`Run::inject` (LOOP-CORE 4.1) appends a `Queued` record before returning; delivery happens at the next step boundary and writes a
matching `Delivered`. A message injected while the harness was stopping is therefore not lost. (Evidence this matters: codex has a durable
`queue_store` (`codex-rs/thread-store/src/queue_store.rs`, `MAX_QUEUE_ITEMS`); Claude Code's transcript has `queue-operation`
records, whose contents I did not read.)

### 4.5 Handoff and memory

- **HANDOFF.md stays** as the human-readable current-state document: written by the host's wrap-up hook (E asks the person if policy
  says so), **rewritten not appended**, size-checked against the fleet ceiling, `WAIT:` lines preserved. It is a derived view, not the
  source of truth; the log and snapshot are.
- **Memory** (facts across sessions) stays file-based with the existing frontmatter-plus-index convention so Claude Code lanes and
  agent-loop lanes can share it. A memory write is a tool (D) and passes policy (E).

### 4.6 Sensitive content

Transcripts hold tool output and may hold secrets. The store lives in a per-user directory with user-only ACLs; E's secret rules apply
(names only; values never written); a redaction step runs before append. No transcript is ever sent to a model provider other than the
one the run uses, and none is published.

### 4.7 Seams

- **C|F:** `SessionStore` (4 verbs above). C owns what a record means; F owns ordering, atomicity and refusal.
- **F|G:** session id and `Conflict` surface as G events; `resume` emits `started{resumed: true}`.
- **F|E:** policy decisions are logged by reference; F stores, E decides. **The session id keys the sandbox session**: the PM ruled
  (2026-11-03, `SANDBOX.md` 5.2) that E runs one sandbox job per agent session, so the same id names the log, the snapshot and the
  sandbox workspace. A `Sandbox` attach/close is a record in the log (`Event`), so a resume knows whether it must re-attach or
  recreate (open question 5).
- **F|H:** AgentLife never reads a session; it sees only adapter output (`HARNESS-ADAPTER.md`).

## 5. Open questions (for the PM, not decided)

1. **Durability class for transcripts:** `Process` (survives a process kill) is verifiable today; `Machine` (power loss) is not
   verified on Windows by PersistAnt. Is `Process` acceptable for lanes? Recommend yes, stated plainly.
2. **Where sessions live** (one directory per machine like `.lane-state`, or per project) and **retention** (how long, how large).
3. **Does C adopt rig-agent?** Decides rig-memory vs our own store; F follows C's spike.
4. **Shared memory with Claude Code lanes:** is the existing file convention the contract, or does agent-loop get its own?
5. **Resume versus the sandbox session.** After a harness restart, is the session's sandbox workspace re-attached (its files and
   running processes survived) or recreated from the log and the last write-back? It depends on AntSpawn ask A1 (long-lived job,
   `SANDBOX.md` 5.5): AntSpawn today destroys the environment at the end of a job (`antspawn` `docs/DESIGN.md:28-33` at `cc98a2c9`), so
   a restart would lose uncommitted workspace state. Recommend: treat the workspace as recreatable and the log plus write-back as the
   truth, until A1 says otherwise.
6. **PersistAnt has no log/append abstraction that I read:** an append-with-expected-length primitive may be a PersistAnt feature
   request rather than something we build in this repo.

## 6. What I did not verify

- Claude Code's transcript semantics beyond record type and key names (content not read; format undocumented).
- That `rig-memory` `DirStore` works on Windows, its fsync behaviour, or that its refusal of the second writer holds across processes
  (README statement only).
- `rig-session/4` record layout; `codex` rollout format; whether `codex-rollout` / `codex-thread-store` are on crates.io.
- PersistAnt: whether an append primitive exists (README and ROADMAP head only); its Windows durability.
- goose-context-management: summary quality, token-estimator accuracy; nothing was run.
- Memory write policy and redaction design are proposals; no code was written.
