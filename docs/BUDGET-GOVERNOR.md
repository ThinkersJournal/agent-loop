# Budget governor (layer H, enforced at C and G): design note

Status: DRAFT for the PM gate. **Note only, no code, nothing built, nothing always-on.** Author: the `agent-loop` lane.
Task: pm-ants `[TASK]` 2026-11-21 ("budget-governor-design-note"), citing a PM ruling that the governor goes to agent-loop,
which owns the loop core and the harness adapter where a budget is enforced. I did not see that ruling itself; it reached me
as a peer message. Gap source: `capant` `docs/AGENT-STACK-GAPS.md` (capant#26, read at its `origin/main` 2026-11-21).
Companions: `docs/LOOP-CORE.md`, `docs/HARNESS-ADAPTER.md`, `docs/ARCHITECTURE.md`.

## 1. What exists today (read from this repo's `main`, 1a8c892)

| Fact | Where |
|---|---|
| A run takes `max_turns`; the driver opens `AgentRun::new(prompt).max_turns(n)` | `src/run.rs:35`, `src/driver.rs:319-328` |
| Every model response emits `Event::Usage { input, output }` (tokens only; no money, no currency) | `src/driver.rs:235, 457`, `src/events.rs:24` |
| The summarising call also emits its `Usage`, so compaction spend is visible | `src/driver.rs:157, 235` |
| A soft stop finishes the batch, saves and resumes | `cargo test --test control` (`soft_stop_finishes_the_batch_saves_and_resumes_with_the_queued_message`, per `ROADMAP.md`) |
| rig-agent gives `max_turns`, cumulative `usage()`, per-turn `Usage` | `docs/LOOP-CORE.md` (rig read at `4908f428`) |
| ACP `usage_update` carries `{used, size, cost?: {amount, currency}}`, cost cumulative and optional | `docs/HARNESS-ADAPTER.md` section on ACP |
| Claude Code's cost source is **UNVERIFIED** | `docs/HARNESS-ADAPTER.md` open question 2 |

Nothing enforces a spend limit. `max_turns` is the only cap, and it counts turns, not tokens or money. Not measured here:
whether any of this runs against a real provider (all runs are against a mock; see `ROADMAP.md`).

## 2. What "budget governor" has to mean

capant's row says no provider has it; the stack names it in layer H. Two different jobs share the name:

1. **In-run cap (layer C):** stop one run that is spending too much. Needs usage per turn, which we already have.
2. **Fleet cap (layer H):** stop or throttle a role across runs and across harnesses. Needs a cost report from every
   harness (layer G `cost`) and a way to issue a `SoftStop`. This is AgentLife's side of the C|G|H seam.

I propose that agent-loop carries job 1 and the *harness side* of job 2 (the `cost` report and honouring `SoftStop`), and
that the fleet policy itself (who gets how much, per day or week) stays with layer H. The cost-routing statement in
`C:\Projects\CLAUDE.md` section 9 is the standing reason a fleet cap is wanted; it names no mechanism, so none is implied.

## 3. Proposed shape (a sketch, not an API)

- A `BudgetPolicy` trait given the cumulative usage after each model response and returning `Continue`, `Warn(reason)` or
  `Stop(reason)`. A stop uses the existing soft-stop path (finish the batch, save, resume later), never a kill.
- Limits are on tokens in, tokens out and an optional money figure. Token fields are `Option`: a provider that reports none
  gives `None`, which is unknown, not 0 (same rule as `Cost::Unknown`). Cache read/write counts are not modelled (`Event::Usage`
  carries `input` and `output` only); a cache-aware cap is a later decision, not part of this slice.
- **Overshoot:** the check runs after a model response, so a run can exceed a cap by one turn plus that turn's tool batch
  (a soft stop finishes the batch). A cap is a ceiling with that slack, not a hard limit.
- **Resume:** usage is not carried across a resume today. `Event::Usage` is emitted per response and no saved state holds a
  cumulative total (grep of `src/` at 1a8c892: only `driver.rs` and `events.rs` mention usage). A cap that spans a
  resume needs the host to keep the running total and pass it into the policy, or a deliberate addition to saved state;
  neither exists. Until then an in-run cap is per process, not per logical run. Money needs a price table per model; we have none, and
  prices change, so money is **off by default** and only the provider-reported cost (ACP `cost`, when present) counts.
- The policy is a value the host passes in. No background task, no timer, no polling: it is evaluated at turn boundaries that
  already happen. This keeps it from creating standing work.
- **Fleet cap is opt-in and reads `THROTTLE.md`.** Per `C:\Projects\CLAUDE.md` section 9, cost is a routing constraint, not a
  stop-work order, and whether we are throttled is read from `THROTTLE.md` (default FULL SPEED). So no fleet cap is on unless
  `THROTTLE.md` says so or a host passes a policy; nothing here defaults to stopping a lane.
- The fleet face is the G `cost` report (already designed: cumulative, ACP `Cost` shape, omitted means unknown).
  A supervisor that sees a breach sends `SoftStop`. An adapter that cannot report cost declares `Cost::Unknown`, and the
  governor must treat unknown as unknown, not zero.

## 4. Decisions needed (none taken here)

| # | Question | Who decides | My recommendation |
|---|---|---|---|
| 1 | Is the in-run cap (job 1) the first slice? | PM | Yes: smallest, testable against a mock model |
| 2 | Is money in scope, and where does the price table live? | CireSnave via PM | Not in the first slice |
| 3 | Where does cost come from for Claude Code lanes? | whoever owns the lane wrapper | Unowned; blocks the fleet cap for Claude backends |
| 4 | Does the fleet policy live in AgentLife? | PM | Yes; it already owns the layer-H supervision seam |

## 5. Rows for the gap list (agent-loop's side)

| Gap | State | Blocker | Smallest next step |
|---|---|---|---|
| budget-governor, in-run cap | designed (this note), not built | decision 1 | `BudgetPolicy` plus a mock-model test that a token cap stops a run and the run resumes |
| budget-governor, fleet cap | not designed past section 3 | decisions 3 and 4; opt-in via `THROTTLE.md`. (Synapse rooms are now built, so the earlier "no subscribe/topic" blocker, `HARNESS-ADAPTER.md` open question 1, is stale; not re-read here) | none until 3 and 4 are answered |
| harness-adapter (G) | designed in `docs/HARNESS-ADAPTER.md`, nothing built | PM gate on the note; Synapse transport; codex app-server and goose-sdk method lists unread | wrapper for our own `Driver` only, after the gate |
| eval-harness | not started, not designed | CireSnave 2026-11-01 (quoted by capant): "we're not ready to even start that yet"; what Verify scores is undefined | none; do not start |

## 6. Standing-work check

This note adds no loop, timer, listener or lane task. The governor, if built, runs inside turns that already happen. Any
fleet-level watching would be AgentLife's, not ours, and is not proposed here.

— lane: agent-loop
