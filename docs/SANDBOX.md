# Sandbox and permission policy (layer E): design note

Status: DRAFT for the PM gate. Nothing here is built. Author: the `agent-loop` lane. Task: PM `[TASK]` 2026-11-03 (notes D, E, F
after C and G; "read AntSpawn first, then AgentLife, say what they cover, then survey outside"). Map:
`C:\Projects\AGENT-STACK.md` (row E; seam D|E). Companions: `docs/LOOP-CORE.md`, `docs/HARNESS-ADAPTER.md`, `docs/TOOLS.md`.
Read-only facts carry a file or URL and a ref or date (2026-11-03 UTC unless stated). Refs: antspawn `cc98a2c9`, agentlife
`a5c06136`, codex `f58acd6e`, all `origin/main` fetched 2026-11-03.

## 1. The intended hello-world (ten lines)

What the loop host writes to make every tool call safe. Target API, not final names.

```rust
let policy = Policy::load("policy.toml")?                     // rules: commands, paths, network, secrets by name
    .with_asker(consent::hello());                            // how a person answers `Ask`
let sandbox = Sandbox::open_session(&session_id, Needs { network: Off, workspace: "./" })?;  // ONE AntSpawn job per agent session, or refuse
let guard = Guard::new(policy, sandbox, Ledger::open("audit.jsonl")?);
tools.policy(guard.clone());                                  // D|E seam: Policy::check before every execution
// at run time, for each call:  Allow -> run inside the session's sandbox (calls multiplexed into it)
//                              Ask   -> person answers, answer is recorded, default on no answer = Deny
//                              Deny  -> the model sees a result: "denied: <reason>"; nothing ran
// every decision, including every Deny, is one line in audit.jsonl
```

Properties: **fail closed** (no policy, an error, or no answer means no run); the decision and the isolation are separate
pieces (policy decides, a launcher isolates); the person is reached through the consent channel, not a prompt this layer
invents; the audit line exists whether or not the call ran.

## 2. What E is, and is not

STACK row E: what a tool may touch; allow/deny; audit. Two jobs that must not be merged:

- **E1 policy:** `check(call, ctx) -> Allow | Deny(reason) | Ask(request)` for every call (the D|E seam in `TOOLS.md` 4.3).
- **E2 enforcement:** a boundary the policy cannot be talked out of, for tools that run code or touch the host: process and
  file isolation, network on/off, resource limits.

STACK also says consent "spans E and I": asking is E's mechanism, the person answering through a channel is the human surface
(layer I). Not E: the loop, the tool protocol, session storage, or fleet-level authorisation of lifecycle actions.

## 3. What our own projects already cover (read first, as asked)

### 3.1 AntSpawn (`antspawn` at `cc98a2c9`: `README.md`, `ROADMAP.md`, `docs/DESIGN.md`, `SECURITY.md`)

Covers **E2 for one job**: "Run one job in a clean sandboxed environment: create it, load only what the job declares, run,
collect the declared outputs, destroy it" (`README.md`). Specifically:

| Capability | What exists | Evidence |
|---|---|---|
| Job spec | Strict TOML: `[job]` cmd/cwd/env, `[[mount]]` host/guest with modes `ro`, `rw`, `scratch`, `[limits]`, `[needs]` (network/gpu/tools), `[[output]]` | `docs/DESIGN.md` items 1-2 |
| Launchers | `naive`, `account` (job runs as local account `antspawn-run`, kill-on-close Job Object, outbound firewalled, `runner.toml` caps `time_s` and `ram_mb`), `windows-sandbox` (throwaway VM via `wsb.exe`, network/vGPU/clipboard off, only declared folders mapped). Never elevate, never create accounts | `DESIGN.md` items 3, 7 and the `windows-sandbox` section |
| "Tested, never asserted" | A capability is off until its row in `matrix/` says `yes`; every test has a known-bad control | `README.md`; ROADMAP rows 3-5 done |
| Result contract | `spawn run` exit codes (124 timeout, 137 killed, 125 refused, 126 spawn-side failure) and an authoritative `spawn-result.json`; last 64 KiB of stdout/stderr | `DESIGN.md` "`spawn run` result contract"; ROADMAP row 22 done (#49) |
| Refusal when unmet | `providers(needs)` / `open_named` refuse with the unmet need named | `DESIGN.md` item 4 |

**What it does not cover (E needs these), each read from the same files:**

- **Per-call decisions.** There is no allow/deny/ask for a command; AntSpawn runs whatever the spec says, and its authority is
  "who may write the spool" (ACL) plus `runner.toml` caps (`DESIGN.md` item 7).
- **Concurrency.** `account`: "One job at a time... `concurrent` is never claimed" (`DESIGN.md` item 7); ROADMAP row 25 notes two
  suites at once serialise on the one runner account (24-25 s vs 4.5-10 s).
- **In-place workspace edits.** `account`: "Copy, not bind... `rw` mounts are refused"; inputs are copied in and declared outputs
  copied out (`DESIGN.md` item 7). A coding agent that edits the repo directly cannot run its `shell` there without a copy-in/out
  step. `windows-sandbox` does allow declared `rw` mounts (`DESIGN.md` windows-sandbox section).
- **Network beyond off/on.** `network = "on"` is refused for `account`; no allowlist (`DESIGN.md` item 7). Not read for `windows-sandbox`
  beyond "network off" and the 260 ruling that allows it for the runner case.
- **Secrets.** "`secrets` are names only; this version implements none and refuses a spec that lists one" (`DESIGN.md` item 1).
- **CPU/GPU limits.** `cpu`, `gpu` refused for `account` (`DESIGN.md` item 7). GPU in Windows Sandbox unmeasured (ROADMAP row 18).
- **Latency.** Golden images do not exist for Windows Sandbox (fresh image every launch); per-job setup cost for `account` is
  unmeasured (ROADMAP row 13). A VM per tool call is therefore a poor fit; a job per call to the `account` launcher is unmeasured.
- **Provisioning.** The account launcher needs one-time elevated setup by CireSnave (`scripts/account-setup.ps1`, `DESIGN.md` item 7);
  ROADMAP rows 11-12 (self-hosted runner, non-admin `schtasks /Run`) are held. Its `account` and `windows-sandbox` tests are
  runner-only, "UNVERIFIED in CI" (`DESIGN.md` item 6).
- **Callers.** The adapter has no caller outside tests yet (ROADMAP row 14, true not noise).

### 3.2 AgentLife (`agentlife` at `a5c06136`: `README.md`, `docs/CONSENT.md`, `docs/LAUNCH-ON-MESSAGE-THREAT.md`)

Covers **authorisation and consent for lifecycle actions, not tool calls**:

- "**No permission escalation.** A lane is restarted in the mode it last ran in; anything more needs a person's approval. Requests
  to start agents that arrive from another agent are untrusted input" (`README.md`).
- A **consent contract** (`consent.rs`: `submit`, `begin_answer`, `resolve`, `withdraw`) over OverMind's `user-request` crate with
  Windows Hello: a request holds no privilege and has no timeout; a changed bound hash voids it unasked; `Cancelled` closes it;
  `TimedOut`/`Unavailable` leave it pending; **fails closed off Windows**; an approval can only come from the channel (`docs/CONSENT.md`,
  and failure 10 in `SUPERVISION-INPUTS.md`). `user-request`'s `KindId` was a closed set compiled in (`Secret`, `LaneDialogBypass`) when
  CONSENT.md was written; a new kind needs the OverMind lane (CONSENT.md open question 1). Current published version of the crate:
  0.16 per `SUPERVISION-INPUTS.md` failure 10 (not re-checked).
- A bounded-authority pattern worth reusing: a caller with a narrow identity can do only what its gate allows, every call (and every
  refusal) is **ledgered**, and the "if forged, at worst it can..." analysis is written down (`LAUNCH-ON-MESSAGE-THREAT.md`).
- The ledger (`restart.log`, JSON lines: time, requester, role, outcome, acted) (`SUPERVISION-INPUTS.md` section 1.1).

It does **not** decide, check or record what a tool may do inside a run. That is the gap E fills.

## 4. Survey: what exists outside to ADOPT before anything is built

Method as in the earlier notes (`gh api`, crates.io API, files at named commits). Licence = root LICENSE text at the commit.

| Candidate | Licence | Covers | Lacks / risk | Evidence |
|---|---|---|---|---|
| **codex-execpolicy** (openai/codex `codex-rs/execpolicy`) | Apache-2.0 (`f58acd6e`) | A policy engine for commands: `prefix_rule(pattern, decision = allow \| prompt \| forbidden, justification, match, not_match)` in Starlark; `match`/`not_match` examples are validated at load time as unit tests; strictest decision wins; `host_executable` pins which absolute paths may resolve a basename. Maps directly to `Allow/Ask/Deny` | Not on crates.io (`codex-execpolicy` not found; control: `landlock` found). "Preview... API may have breaking changes". Prefix rules only: no path, network or secret rules | `codex-rs/execpolicy/README.md` |
| **codex sandbox crates** (`linux-sandbox`, `windows-sandbox-rs`, `sandboxing`, `process-hardening`) | Apache-2.0 | Linux: bubblewrap (`--ro-bind / /`, writable roots layered, protected subpaths), seccomp, `PR_SET_NO_NEW_PRIVS`, network syscalls blocked under restricted policy, `io_uring` blocked. Windows: a large crate whose file names show ACLs, tokens, WFP (Windows Filtering Platform), DPAPI and a provisioning service | Not on crates.io (`codex-linux-sandbox`, `codex-windows-sandbox` not found). Windows mechanism not read, only file names. Needs installed setup | `codex-rs/linux-sandbox/README.md`; directory listing of `windows-sandbox-rs/src` |
| **landlock** (landlock-lsm/rust-landlock) | MIT or Apache-2.0 (`6a541374`, both files present) | Linux self-sandboxing of a process's filesystem rights; 0.4.7, 20,096,385 downloads | **Linux only**: not our primary platform | crates.io API; `README.md`; LICENSE-MIT, LICENSE-APACHE |
| **rig-tools** `Workspace` / overlay / checkpoint (rig) | MIT (`4908f428`) | In-process file-tool abstraction: tools act on a `Workspace` (`Local` or remote); the crate lists checkpoints and an overlay workspace; `edit` writes only if the file still holds the text it was made from | Not a security boundary (in-process); crate docs for overlay/checkpoint were not read | `crates/rig-tools/README.md` |
| **OS primitives** (Windows Job Object, restricted token/AppContainer, WFP; Linux bubblewrap/seccomp) | n/a | The mechanisms the above build on; AntSpawn already uses a kill-on-close Job Object and a local account | Building directly on them repeats work AntSpawn and codex have done | `antspawn DESIGN.md` item 7 |

Not surveyed (no claim): Claude Code's permission model (closed; rules and modes are documented publicly but not read here), Docker and
gVisor-class runtimes, OpenHands' runtime, WASM sandboxes (Wasmtime), Firecracker. Docker is allowed on this box under the lane rules
(`AGENT-ONBOARDING.md` section 2) but is Linux-container based and was not assessed.

### Reading of the survey (recommendation, not ruling)

- **E2: use AntSpawn.** It is ours, MIT OR Apache-2.0, already certifies launchers with negative controls, and runs on Windows,
  our platform. Do not adopt codex's sandbox crates (unpublished, installer-heavy); use them as the reference for what a Windows
  sandbox needs (ACLs, tokens, WFP) and for the Linux path later (bubblewrap + seccomp; `landlock` is the published crate).
- **E1: write our own thin `Policy`, take codex-execpolicy's rule model.** Three-valued decision, strictest-wins, justification
  strings, and **examples as load-time tests** are the parts worth taking (provenance: reimplement from the README semantics, credit
  it; do not copy code without keeping its licence and notices). Add the rule kinds it lacks: paths (workspace roots, protected
  subpaths), network, secrets by name.
- **Ask: use the consent channel (`user-request` via AgentLife's `Consent` trait shape).** It exists, is published, fails closed
  and is bound to a subject hash. A new request kind for "tool call" is a cross-lane change (OverMind).
- **The AntSpawn constraints shape the tiers** (section 5.2): today's launchers are one job at a time, copy-only (`account`) or a VM
  per launch (`windows-sandbox`). PM ruling 2026-11-03: one job per tool call cannot be the only mode, so the default tier is
  **session-scoped**, and what AntSpawn must gain for it is listed as asks in section 5.5.

## 5. Design

### 5.1 Policy (E1)

- Rule file (TOML, strict). Rule kinds: `command` (prefix pattern as codex), `path` (root, mode `ro | rw | deny`, protected
  subpaths), `network` (`off | on | allow = [hosts]`, enforced only where a launcher can, else refused), `secret` (names only, as AntSpawn).
  Each rule has `decision` and `justification`; **`match`/`not_match` examples are checked when the file loads**.
- Strictest decision across matching rules wins; **no match means `Ask`, not `Allow`** (deny-by-default for anything that executes
  code or writes; read-only tools inside declared roots may default to `Allow`).
- **A policy file in a repository is untrusted until a person approves it** (same rule as project MCP config, `TOOLS.md` 4.4).
  Changing policy at run time is a tool call that is itself `Ask`.
- Pure and deterministic: `check` takes the call and context and does no I/O except reading the loaded rules, so it is testable
  with the same negative-control style AntSpawn uses.

### 5.2 Enforcement tiers (E2), session-scoped by ruling

PM ruling 2026-11-03 (`[FIX]` on this note): a sandbox job per tool call is **not acceptable as the only mode** (latency per call,
and copy-only breaks in-place edits). The design is **one sandbox job per agent session**: a persistent restricted workspace;
tool calls are multiplexed inside it through the `ToolHost`, each one passing `Policy::check` first; job-per-call stays only as the
high-risk tier.

| Tier | For | Mechanism | Notes |
|---|---|---|---|
| S (default) | All tools of a session: `read`, `edit`, `write`, `search`, `shell`, MCP stdio servers the session starts | **One long-lived AntSpawn job per session**: a restricted workspace (the project tree or a working copy) plus a small tool executor inside it. The harness's `ToolHost` sends each allowed call in over a pipe; the job's account and Job Object, firewall and caps are the boundary, `Policy::check` is the per-call decision | **Needs AntSpawn changes (section 5.5, asks A1-A5). Until they exist this tier has no implementation.** In-place editing needs write-back or bind (ask A2) |
| J (high risk) | Untrusted code, anything needing network | **Job per call**: AntSpawn `account` for restricted-but-offline code, `windows-sandbox` (throwaway VM) for untrusted or networked code | Exists today, with its limits: one job at a time, copy in/out, VM launch cost (section 3.1). Chosen per call by policy (`command` rules naming `tier = "J"`), never by default |

There is no policy-only tier for tools that execute code. File tools (`read`, `search`) may run host-side with path rules only
**if** the session workspace is the project tree itself; that is a deliberate downgrade to be ruled on (section 6), not a default.

`Sandbox::open_session(id, needs)` calls AntSpawn's `providers(needs)` and **refuses with the unmet need named** if no certified,
provisioned launcher offers a session. It never falls back to a weaker launcher or to job-per-call silently (the `naive` launcher
is not an isolation tier).

### 5.3 Audit

One JSON line per decision: time, run id, role, tool, arguments digest (not the secrets), matched rules, decision, who answered an
`Ask` (channel and subject hash), and for allowed calls the launcher id and the AntSpawn result status. Denials are recorded; a
decision that could not be recorded is a `Deny`. Same format family as agentlife's `restart.log` so one reader can read both.

### 5.4 Seams

- **D|E:** `Policy::check` before every execution; the host runs a call only on `Allow`, or `Ask` answered approved.
- **E|I:** `Ask(request)` goes to the consent channel; a request holds no privilege, is bound to the call's subject hash, and a
  changed call voids it unasked (AgentLife's contract).
- **E|G:** `Ask` pending is an `activity` state `waiting{on: "approval"}`; a soft stop while an `Ask` is open cancels the request
  (`withdraw`) (`HARNESS-ADAPTER.md` 5.2).
- **E|F:** audit lines are session-adjacent but not session data; they are not compacted away.

### 5.5 Asks to AntSpawn (for the PM to route; this lane does not change AntSpawn)

What Tier S needs that AntSpawn does not provide today. Evidence is `antspawn` `origin/main` `cc98a2c9`, `docs/DESIGN.md` (D),
`CHANGELOG.md` (C), `matrix/launchers.tsv` (M), `ROADMAP.md` (R), read 2026-11-03. Each ask is a request for the AntSpawn lane to
size and schedule, not a design for it.

| # | Ask | Today (evidence) |
|---|---|---|
| A1 | **Long-lived job / session.** Open a session environment, run many `exec`s in it, close it. State persists between execs inside the session; the session is destroyed at close or crash | A job is create, run, collect, destroy: the launcher spools, runs, polls `exit.code`, deletes its dirs, and `serve` wipes the work dir before `exit.code` appears (D:28-33). The Job Object is kill-on-close per `exec` (D:30). `windows-sandbox` is one throwaway VM per run, `wsb stop` ends it (D:44-52) |
| A2 | **In-place workspace with write-back or bind.** The agent edits files in its project and the changes reach the real tree, under policy, without per-call copy-in/out | `account`: "Copy, not bind... `rw` mounts are refused"; `C:\Projects` is ACL-denied to the account; outputs are copied out only for declared `[[output]]`s at the end (D:23-26). `windows-sandbox` can map declared `rw` folders (D:44-52) but is a VM per launch. Needs a design choice: bind with an ACL grant, or a session working copy with an explicit write-back step that applies a reviewed change set |
| A3 | **Concurrency of N sessions.** Many agent sessions at once on one machine, isolated from each other | "One job at a time. The task ignores a second start while running" (D:37); `concurrent` is `no` for `naive`, `account` and `windows-sandbox` in the matrix, "no probe for this capability yet" (M); `windows-sandbox` runs one VM at a time, `CO_E_APPSINGLEUSE` (C:124); two suites at once serialise on the one runner account, 24-25 s vs 4.5-10 s (R row 25). Needs a probe for `concurrent`, and isolation **between** sessions (a session must not read another's workspace) |
| A4 | **Cancel and kill at two grains.** Cancel one `exec` without ending the session; kill the whole session and everything in it | Only the whole job: kill-on-close Job Object (D:30), exit codes 124 time limit, 137 killed (D:92). No per-exec cancel. The loop's soft stop (`HARNESS-ADAPTER.md` 5.2) needs both |
| A5 | **Audit stream.** Per-exec records as they happen: start, argument digest, exit, duration, bytes in/out, resource use; streamed output tail, not only at the end | Results exist only after the job: `spawn-result.json` plus the last 64 KiB of stdout/stderr, `account` streams only with a redeployed runner (D:92-99). `windows-sandbox` returns one combined stream (D:97). Nothing is emitted during the run |
| A6 | **New certified capabilities with negative controls**: `clean-slate-next-session` and `no-read-other-session` (and a probe for `concurrent`) | `clean-slate-next-run` is certified `yes` for `account` and `windows-sandbox` by a `write+exists` probe (M; C:252-254). A persistent session deliberately keeps files between execs, so that capability's meaning does not carry over; AntSpawn's own rule is that a capability is off until a row says `yes` (README) |
| A7 | **Per-exec time limit and per-session RAM cap** | `runner.toml` caps `time_s` and `ram_mb` per job; `cpu`, `gpu`, `network = "on"` refused (D:34-36) |

Not asked for the first cut: network allowlists (stay `network.off` in Tier S; networked work goes to Tier J, D:36), GPU, secrets
(AntSpawn refuses a spec that lists one, D:5; E's secret rules cover names only). Provisioning for N sessions will need CireSnave's
elevated setup as `account` does today (D:19-22); that is the AntSpawn lane's to scope.

**AntSpawn's sizing of A1-A3 (answered; do not re-ask).** `antspawn` `docs/SESSION-OPTIONS.md` at PR #67 head `657639969ec8` (open when read, state CLEAN; read through `gh api contents?ref=<sha>`, so re-check once it merges). Sizes are the AntSpawn lane's estimates in PR-sized slices, not measurements. Candidates are `account` and `windows-sandbox`; `naive` isolates nothing.

| Ask | `account` | `windows-sandbox` | Needs a ruling |
|---|---|---|---|
| A1 session | feasible, 4 slices (way 2: a session host on a named pipe, DACL to the launcher SID only; way 1, a spool per exec, cannot stream and pins the runner) | feasible in principle, 3 slices; repeated `wsb exec` into one VM **unmeasured**; open costs about 10-25 s on the loaded host | no |
| A2 in-place | B2 session working copy plus reviewed write-back, 4 slices; B1 bind by ACL grant, 1 slice plus a ruling (ACL ordering **not measured**) | B3 `rw` mapped folder, needs a probe, no review step | **yes, CireSnave** |
| A3 N sessions | pool of N accounts, 4 slices plus one elevated paste per account; isolation between sessions **not yet run live** | **no**: one VM at a time (`CO_E_APPSINGLEUSE`, measured) | no, but N and the accounts are CireSnave's one-time setup |

AntSpawn's order: A1 on `account` first, then A3, with A2 decided in parallel as a question. `windows-sandbox` stays Tier J. Side effects it names: a session can write the account's real profile by absolute path and that survives into the next session, so a profile wipe at close is needed; the pool built from the runner setup script would have network on unless the firewall rule is folded in; `ram_mb` is per job and `cpu` is refused, so one session can starve others (A7). A6's two capabilities each need a probe with a known-bad control.

**Consequence for sequencing:** Tier S cannot be built until A1-A3 exist; A4-A7 can follow. Until then the only runnable E2 is
Tier J, which the PM has ruled is not enough on its own. A1-A3 are now sized (above); A2 is ruled (6.2, design in 5.6); what remains is the PM's go to AntSpawn on A1 first.

### 5.6 Workspace modes B1 and B2 (design addendum, 2026-11-06)

Ruling: section 6 item 2. Everything marked "proposal" is the PM's proposal to this lane (2026-11-06), not a ruling by CireSnave.
Nothing here is built; AntSpawn was not run or changed. Facts about AntSpawn come from section 3.1 and the sizing table in 5.5 (read at
PR #67 head `657639969ec8`, not re-read on `main`).

**One setting.** The harness passes the session's workspace mode as one value when it opens the session:
`Sandbox::open_session(id, Needs { workspace: Workspace { root, mode } })` with `mode = Bind | CopyBack`. `CopyBack` is B2, `Bind` is B1.
The same value is what the policy file's `[workspace] mode = "copy-back" | "bind"` sets, and what the person sees named at the approval
prompt. There is no third value and no "auto".

| | B2 `CopyBack` (default, proposal) | B1 `Bind` (opt-in) |
|---|---|---|
| What the sandbox account touches | A **working copy** of the project inside the session | The **real project folder**, through an ACL grant to the sandbox account |
| How edits reach the real tree | A reviewed **change set** applied by a write-back step | Immediately, in place |
| Review point | Write-back (a person or policy approves the change set) | Per call, by `Policy::check` only; there is no later review |
| Boundary kept | Yes: the account never has rights on the real tree | **Widened**: the account can write what the grant covers, so a bad call is real damage |
| AntSpawn cost (its estimate, 5.5) | A1 plus 4 slices | A1 plus 1 slice and a ruling; **ACL ordering (does a deny entry beat the grant) not measured** |

**Choosing B1 (proposal).** Default is B2. B1 is used only when a policy setting selects it, per project or per session, **and a person
has approved that policy file**. A policy file in a repository is untrusted until approved (5.1), so a cloned repo cannot switch itself to
B1. The approval names the mode and the root it applies to, is bound to the file's hash (a changed file voids it, as in AgentLife's
consent contract), and is remembered per project outside the repo. If the setting is absent, malformed or unapproved, the mode is B2; if
B2 cannot be provided, the open **fails** with the unmet need named, it never falls back to B1.

**Protected paths (proposal).** `.git`, `.github`, `.claude`, `CLAUDE.md` and hook files are protected in both modes, because a write to
them changes what runs later outside the sandbox. Under B2 a change-set entry that touches one is **refused at write-back** (not merely
flagged) unless the person approves that entry by name. Under B1 they are `path deny` entries in the policy and also **deny ACEs** on the
real tree. **The ACE half is measured once, by the AntSpawn lane** (`antspawn` `docs/MEASUREMENTS.md`, "2026-11-06: W0", `main` `cdcd1c8`, one
sample per case, read 2026-11-06): an explicit grant on the tree beats the inherited `C:\Projects` deny, and an explicit deny on a
subpath (`.git`) beats the grant: create, append and delete were refused and the file was still present on the host. **The deny must use
exact rights** (`WriteData, AppendData, WriteExtendedAttributes, WriteAttributes, Delete, DeleteSubdirectoriesAndFiles` via `Set-Acl`):
icacls `W` includes SYNCHRONIZE and made `.git` unreadable, so `git status` inside the session would fail. **Not measured:** rename or
create-directory inside the denied path, `.git` replaced by a file or junction, files the account creates, grant cost on a large tree,
and a grant surviving a crashed launcher. Until those are measured, B1's protection of these paths is a one-sample result, and the
policy `path deny` entries stay as the second layer.

**B2 write-back, concretely.**
1. At open, the project (minus what policy excludes) is copied into the session workspace and a **base manifest** (path, size, content
   hash) is recorded; nothing under the real tree is touched.
2. During the session all tools act on the copy. The ledger (5.3) records every call as before.
3. At a boundary the loop chooses (end of run, on request, or at a policy-set interval) the harness computes the **change set**: files
   added, modified, deleted, relative to the base manifest.
4. **Deletes are refused unless a person approves them per file (proposal).** An approved delete is one ledger line naming the path.
5. **Build scripts such as `build.rs`, `package.json` scripts, `Makefile` and CI files need explicit approval, not a ban
   (proposal).** They are listed in the change set as "executable on a later step" so the approval names what it enables.
6. Write-back **applies only if the real file still matches the base manifest** (hash) for each touched path; a mismatch is a
   `Conflict` shown to the person, never an overwrite. This is the same "append only if unchanged" rule as `SessionStore` (`SESSION.md` 4.3).
7. The apply step runs in the harness, as the user, not inside the sandbox, so the sandbox account never needs rights on the real tree.
   Apply is a tool call that is itself `Ask` by default (5.1); the answer and the change-set hash are recorded.
8. A session that ends without write-back leaves the real tree untouched; the working copy is destroyed with the session, and the log
   plus the change set are what survive (`SESSION.md` open question 5, resolved the same way: the workspace is recreatable).

**What the model sees.** Under B2 the agent is told its edits are staged until written back, so "done" means "proposed"; a tool
`workspace.writeback` exists for it to request the boundary. Under B1 no such tool exists. The mode is part of the system context, so a
model cannot assume the other one.

**Seams.** D|E: `Policy::check` is unchanged; mode adds a `path` rule family (`deny` entries applying to both modes) and the approval
binding above. F: the base manifest and the change set are session records (`Event`), so a resumed session knows whether write-back
happened. G: a pending write-back approval is `activity` `waiting{on: "approval"}` (5.4).

**Asks to AntSpawn for the PM to route (the mode is one setting the harness passes).** In order:
- **A1 long-lived session**, first, unchanged from 5.5; both modes sit on it.
- **A2 for both modes.** The launcher takes `workspace = { root, mode = "bind" | "copy-back" }`. `copy-back`: AntSpawn provides the
  isolated working copy and **returns the change set** (it does not apply it; the harness applies it as the user). `bind`: AntSpawn
  grants the account rights on `root` for the session and **removes the grant at close and on crash**, refusing to open if it cannot
  prove the removal path. It records which mode it ran in, in `spawn-result.json`.
- **A3 pool of sessions**, unchanged. Isolation between sessions applies to both modes: under `bind` two sessions on one root are
  refused unless the person approves it.
- **A8 (ACL ordering): answered, partly.** AntSpawn measured it (W0, above), one sample. What remains is the unmeasured list above
  (rename, mkdir, junction, crash leaving the grant), which AntSpawn's session plan journals; a capability `bind-deny-beats-grant`
  stays off until a row says `yes` with a known-bad control, per AntSpawn's own rule.

**Slices, in this lane's order (each its own PR, each `[READY]`):**
1. Mode types, the `[workspace]` policy setting, the approval binding (hash-bound, per project), and fail-closed selection; tested with a
   fake `Sandbox`. No AntSpawn needed.
2. The B2 change-set engine against a fake working copy: manifest, diff, protected-path refusal, delete refusal, hash-match apply,
   conflict. This is pure file logic and runs on `cargo test`.
3. B2 write-back as a tool call through `Policy` and the ledger, with the approval question surfaced as an `Ask`.
4. The real `Sandbox` backend for both modes, **blocked on AntSpawn A1/A2** (and on A8's unmeasured cases for a firm B1 protected-path claim).

**What this addendum did not verify.** The W0 ACL measurement was read, not re-run here. No AntSpawn code, launcher or session was run. Whether copying a large
project tree at open is fast enough was not measured (AntSpawn's own copy-in time is unmeasured, 5.5). That a fixed list finds the
`build.rs`-class files in real repos was not checked. The protected-path list is the PM's proposal and may need more entries (for
example `.cargo/config.toml`, which can name a build runner).

## 6. Open questions (for the PM, not decided)

1. **RULED (PM `[FIX]` 2026-11-03): job-per-call is not acceptable as the only mode; the design is session-scoped** (section 5.2).
   This replaces the earlier open question about job-per-`shell`-call and the earlier idea of keeping edits policy-only and running only
   `shell` in a copied workspace, both withdrawn.
2. **RULED (CireSnave 2026-11-06, board 373 Q2, archived in `CIRESNAVE-DECIDED-ARCHIVE.md`), verbatim: *"373: B1 and B2.  I'd like the ability to do either one.  Lets build both and make it easy to choose one or the other."*** Build B1 (bind) and B2 (working copy with reviewed write-back); B3 is not built. The design is section 5.6. The PM's defaults and protections there are the PM's proposals, not his rulings.
3. **May file-only tools run host-side under path rules when the session workspace is the project tree?** A downgrade for speed; not assumed.
4. **A "tool call" consent kind** needs the OverMind lane (`user-request` kinds are a closed set, CONSENT.md open question 1).
5. **Who owns the audit reader** (the PM's pulse, AgentLife status, or a new command)?
6. **Network allowlists** are not provided by any launcher read; out of scope for the first cut (Tier S has no network).

## 7. What I did not verify

- Claude Code's own permission model (closed source; public docs not read).
- That any AntSpawn launcher is currently provisioned or passing on this machine (ROADMAP: tests runner-only; row 11 held); `spawn` was not run.
- codex `windows-sandbox-rs` mechanism (file names only); `sandboxing` crate (no README).
- The `user-request` crate's current API and published version (AgentLife docs only; they cite 0.11.1 and 0.16).
- That `Policy` with strictest-wins and examples-as-tests covers our real command set; no rule file was written or run.
- Windows Sandbox network behaviour beyond the DESIGN.md text; GPU.
- Whether Tier S is feasible on the `account` launcher at all (a long-lived Job Object per session, a per-session workdir under one account, or a per-session account): the asks state needs, not a mechanism; AntSpawn was not run or modified.
- Line references (D:, C:, M, R) are to `cc98a2c9` and move with AntSpawn; re-read before acting on them.

Added 2026-11-03: AntSpawn's sizing is read from its PR #67 head, not from `main`, and none of its unmeasured items (ACL ordering, repeated `wsb exec`, live isolation controls, copy-in time) were checked here.
