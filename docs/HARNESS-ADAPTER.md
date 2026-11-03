# Harness adapter (layer G): design note

Status: DRAFT for the PM gate. Nothing here is built. Author: the `agent-loop` lane. Task: PM `[TASK]`, ruled by
CireSnave 2026-11-03. Map: `C:\Projects\AGENT-STACK.md` (row G; seams C|G and G|H). Companion: `docs/LOOP-CORE.md`
(merged, PR #1). Read-only facts carry a file or URL and the ref or date; facts about AgentLife and Synapse are from
`origin/main` of each repo as fetched 2026-11-03 (agentlife `a5c06136`, synapse `ffe9964d`).

## 1. The intended hello-world (ten lines)

What the author of ANY harness (ours, Claude Code via a shim, an ACP agent, an OverMind task) writes to become
supervisable. Target API, not final names.

```rust
let a = Adapter::connect(Identity::role("kiss"), Synapse::local()?)?;   // claims the role; identity is the role
a.declare(Capabilities { soft_stop: true, inbox: true, cost: Cost::Reported, activity: Activity::Events });
a.activity(Busy { step: "tool:read_file" });                           // typed, not a log line
a.cost(Usage { tokens_in, tokens_out, money: Some(Money::usd("0.012")) });
while let Some(cmd) = a.next_command().await {                          // from the supervisor, via Synapse
    match cmd { Command::SoftStop(req) => { wrap_up(); a.stopped_clean(req) }   // or a.declined(req, "why")
                Command::Inbox(msg)    => deliver_to_loop(msg) } }
```

What AgentLife (layer H) sees, and all it sees: identity, capabilities, activity, cost, inbox depth, and the answer to a
soft-stop request. It never sees a harness's internals (STACK seam C|G, G|H).

## 2. What G is, and is not

G is the one outward face every harness shows. Six jobs, from the task: **identity, inbox, activity, soft-stop, cost,
events**, all carried through Synapse.

Not G: starting a process, killing it, restarting it, the roster, or deciding policy. That is lifecycle (AgentLife, layer
H). CireSnave 2026-10-31, as recorded in `agentlife` `docs/SUPERVISION-INPUTS.md` (read at `a5c06136`): AgentLife is
*"a tool for controlling the lifecycle of an agent. It can start agents, stop agents, soft-stop agents, and restart
agents. Those functions are common to almost any agent and not specific to Claude Code."* The adapter is how a harness
**cooperates** with that; AgentLife must still work when the adapter is absent or lying (section 5).

OverMind is a model gateway (layer B) and runs no tools. It is a harness only in the sense that its runner executes
one-shot tasks; section 4 treats that as the minimum-capability case.

## 3. What exists today (the Claude-only version of G)

All from `agentlife` `docs/SUPERVISION-INPUTS.md` and `docs/GRACEFUL-STOP.md` at `a5c06136`, plus Synapse docs at
`ffe9964d`. These are requirements the adapter must be able to reproduce, not designs to copy.

| Job | Claude Code today | Mechanism / evidence |
|---|---|---|
| identity | role id (`valid_identifier`); Synapse `role@account`, restart supersedes the old session by epoch | `SUPERVISION-INPUTS` 1.1; `synapse docs/ROLE_LIVENESS_DESIGN.md` |
| activity | `busy`, `subagents_running`, `no_background_shells`, one idle verdict `LaneState::is_idle` ("never asserted" is not idle) | written by Claude Code hooks into `<role>.json` (`SUPERVISION-INPUTS` 1.2) |
| soft-stop | A `[TASK]` message asks the lane to write HANDOFF, run `assert-idle` last, and stop; supervisor waits up to 600 s for evidence newer than the request; unknown counts as unsafe; a lane may decline | `GRACEFUL-STOP.md` steps 4-5 |
| hard stop | process kill, with pid AND start time re-checked | `GRACEFUL-STOP.md` step 6 |
| inbox | Synapse mailbox per role; `GET /v1/list` carries `pending: {queued, leased, oldest_enqueued_at}` or `null` (= unknown); poll no faster than every 10 s | `SYNAPSE-REQUIREMENTS.md` "Answers received" |
| cost | not found in these documents. **UNVERIFIED** where Claude Code lanes report spend today | n/a |
| events | none. Synapse has no subscribe/topic command (STACK row "Events in Synapse", PM reading); stores counters, not events (`SUPERVISION-INPUTS` section 5) | |

Failures already paid for that the adapter design must not repeat (`SUPERVISION-INPUTS` section 2):

- **Liveness is the supervisor's, never the harness's.** A state file outlives its process; a pid is reused. Liveness =
  process table, pid plus start time (failure 3). An adapter can say "busy"; it cannot say "alive".
- **A silent decline is unreadable (failure 12).** The hook that writes the record can skip without telling anyone.
  Every adapter outcome, including "I will not", must be a message the supervisor can read.
- **Unknown must never read as safe** (graceful stop step 5; Synapse `pending: null`). Every field below has an explicit
  `unknown` value and the supervisor treats it as unsafe.
- **Idle is defined in one place** and "never asserted" is not idle (PM ruling 2026-10-14, `is_idle`).

## 4. Survey: what exists to ADOPT before anything is built

Same method as the loop note: facts from `gh api` and the repos' own files, 2026-11-03; licences are the root `LICENSE`
file text at the named commit (see `docs/LOOP-CORE.md` for the licence table of the other repos).

| Candidate | Licence | Covers (G jobs) | Lacks | Evidence |
|---|---|---|---|---|
| **Agent Client Protocol** (agentclientprotocol/agent-client-protocol; schema v1; Rust crates `agent-client-protocol`, `agent-client-protocol-schema`) | Apache-2.0 (`971b6cb7`) | Agent methods: `initialize`, `session/new`, `session/load`, `session/prompt`, `session/cancel`, `session/list`, `session/resume`, `session/close`, `session/set_mode`. Client-side: `session/request_permission`, `session/update`. Update kinds include `tool_call`, `tool_call_update`, `plan`, `usage_update`, `compaction_update`. `UsageUpdate` = `{used, size, cost?: {amount, currency}}`, cost cumulative and optional | Its README says it standardizes editor-to-agent communication (transport not read here); it is not a fleet-supervision protocol: no role identity, no inbox, no soft-stop-with-handoff. `session/cancel` cancels a turn; it is not "wrap up and stop" | `schema/v1/meta.json`, `schema/v1/schema.json` (`UsageUpdate`, `Cost`), 2026-11-03 |
| **codex app-server** (openai/codex `codex-rs/app-server`, `app-server-protocol`) | Apache-2.0 (`f58acd6e`) | JSON-RPC server with threads, turns, approvals, `thread/list`, shutdown admission errors; has `agent-identity` and `message-board` crates alongside | Product-specific; I read only the experimental-feature sections of its README, not the method list. Whether it exposes a soft stop or cost is **not read** | `codex-rs/` listing; `app-server/README.md` |
| **goose-sdk / goose ACP** (aaif-goose/goose: `goose-sdk`, `goose-acp-macros`, `goose-sdk-types`) | Apache-2.0 (`cfb67d72`) | Crate names suggest an ACP-based SDK; goose-agent has an `Emitter` of typed events (`Message`, `Usage`, `McpNotification`) and a defined cancel path | Not opened beyond names and the goose-agent README | `crates/` listing; `crates/goose-agent/README.md` |
| **Synapse** (ours) | MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE` both present at `ffe9964d`) | Identity (`role@account`, epoch takeover), durable mailbox per role, `list` with presence and `pending`, send/fetch/ack, MCP tools `ack fetch list send set_summary whoami` | No events/topics yet; polling only (10 s floor); not remote | `docs/ROLE_LIVENESS_DESIGN.md`, `docs/SYNAPSE-REQUIREMENTS.md`; tool list in this session |
| **MCP** (rmcp 3.8.0, crates.io 2026-10-26) | Apache-2.0 / MIT transition (`b166e815`) | Tool transport, notifications, a `server` feature | It is layer D; not a supervision protocol | crates.io API; root LICENSE |

Not surveyed (no claim): OpenTelemetry GenAI semantic conventions for activity/cost naming; any fleet-supervision
protocol outside coding agents. Both are worth one read before a build.

### Reading of the survey (recommendation, not ruling)

- **Adopt ACP as the wire for harnesses that already speak it**, for the jobs it covers: prompt/cancel (soft-turn
  control), `session/update` (activity), `usage_update` with `cost` (cost). Do not invent a second vocabulary for these.
- **ACP does not cover identity, inbox, wrap-up-then-stop, or fleet events.** Those are ours and ride Synapse. So G =
  ACP-shaped activity and cost + a small Synapse-carried control vocabulary (section 5).
- **Define G ourselves, as STACK leans.** The part that is ours is small: roles, capability declaration, soft-stop
  handshake, and the rule that every outcome is readable.

## 5. Design

### 5.1 Two planes

- **Control plane (Synapse, role to role):** identity, capabilities, soft-stop request and answer, inbox, status heartbeat.
  Reuses Synapse because identity, epoch takeover and durable mail already exist there and a stopped harness must still be
  addressable.
- **Data plane (ACP where the harness has it, else our native events):** `session/update`-shaped activity and
  `usage_update`-shaped cost. The adapter folds these into control-plane messages for the supervisor; the supervisor never
  speaks ACP to a harness.

### 5.2 The six verbs

| Verb | Direction | Contents | Unknown value |
|---|---|---|---|
| `identity` | harness to H, at claim | role, harness kind, adapter and harness versions, model name, capabilities | missing field is `unknown`, not defaulted |
| `inbox` | H reads, harness acks | depth `{queued, leased, oldest}` from Synapse `list`; delivery into the loop is the adapter's job | `pending: null` = unknown, never wake/stop on it |
| `activity` | harness to H | typed state: `idle`, `busy{step}`, `waiting{on}`, `subagents{n}`, with `since` timestamp | no report for N seconds = unknown, **not** idle |
| `soft_stop` | H to harness, answer back | request `{id, reason, deadline}`; answer is exactly one of `stopped_clean{evidence}`, `declined{why}`, `cannot{why}` | no answer by the deadline = left running, blockers reported (as GRACEFUL-STOP does today) |
| `cost` | harness to H | cumulative `{tokens_in, tokens_out, money?, currency?}` per session, ACP `Cost` shape | omitted = unknown (ACP marks cost optional) |
| `events` | harness to H | typed lifecycle events: `started`, `compacted`, `subagent_started`, `error`, `stopped` | |

Rules inherited from section 3: (1) the supervisor decides liveness from its own process view, adapter "alive" claims
are evidence only; (2) every outcome including refusal is a message; (3) `unknown` is a value, and unsafe; (4) idle has
one definition, derived in AgentLife from `activity`, not stored by the harness.

### 5.3 Capability declaration, so minimal harnesses are first-class

An adapter declares what it can do; AgentLife degrades by capability rather than by harness name.

- **Full** (our loop, a Claude Code shim): all six.
- **ACP agent:** activity + cost + turn-cancel from the protocol; identity/inbox/soft-stop from a thin wrapper that owns
  the Synapse role.
- **One-shot task (OverMind runner):** `identity` + `events(started, stopped)` only. Per `SUPERVISION-INPUTS` section 5
  (from OverMind's code, not measured live) such tasks have no heartbeat and no hooks, so activity is derived by the
  supervisor as time since the last event and a missing `soft_stop` capability means only hard stop applies.
- **Claude Code today:** an adapter that reads the existing hook-written `<role>.json` and reports it. This is the
  migration path AgentLife's own note describes (Claude parts become one backend module). The adapter does not change the
  state directory contract (`SUPERVISION-INPUTS` section 4 do-not-break list).

### 5.4 How `agent-loop` (layer C) satisfies G

Section 5 of `LOOP-CORE.md` promised that every verb is answerable from `Run` and its events. Mapping:

| Verb | From the loop core |
|---|---|
| identity | builder config |
| inbox | `Run::inject` (delivery); Synapse supplies the mailbox |
| activity | the step in progress, from the event stream |
| soft_stop | `Run::stop_soft`: finish the step, answer open tool calls, run the wrap-up hook, emit `stopped` |
| cost | `Usage` events, accumulated |
| events | the event stream itself |

The wrap-up (HANDOFF-style) is a **hook the host supplies**, not something the loop knows about.

## 6. Open questions (for the PM, not decided)

1. **Events need a transport.** Synapse has no subscribe/topic today (STACK). Until it does, activity and cost reach
   AgentLife by (a) the adapter sending point-to-point messages to an `agentlife` role, or (b) AgentLife polling. (a)
   costs mailbox traffic per activity change; (b) has the 10 s polling floor. Recommend (a) with coalescing, and revisit
   when Synapse events land.
2. **Where does cost for Claude Code lanes come from today?** Not found in the documents read. Needs an owner's answer
   before "cost" is a real requirement for the Claude backend.
3. **Does ACP's `session/cancel` plus `session/prompt` suffice for a soft stop,** or do we add a `wrap_up` extension
   through ACP's `_meta`? ACP says implementations must not assume `_meta` values, so an extension is optional by design.
4. **Remote harnesses:** the process-table liveness rule has no meaning for a process on another machine, and Synapse is
   loopback-only today (`SUPERVISION-INPUTS` section 5). Out of scope here; stated so it is not assumed solved.
5. **Naming:** AgentLife binds an agent to a Synapse role by the agent's `name` (`SYNAPSE-REQUIREMENTS.md`, naming
   agreement). This note assumes the same.

## 7. What I did not verify

- Claude Code's own cost reporting (above) and any internals (closed source).
- codex app-server method list, soft-stop and cost support; goose-sdk beyond crate names.
- Synapse's current command surface beyond the documents cited and the MCP tool list; the Synapse README on `main` is
  marketing text and was not used.
- That any of this compiles or has been prototyped. No code beyond the seed.
- AgentLife and Synapse refs are those fetched at the start of this note; either may have moved since.
