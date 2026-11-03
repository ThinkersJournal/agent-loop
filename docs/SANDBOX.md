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
let sandbox = Sandbox::select(Needs { network: Off, writable: ["./src"] }, Tier::Job)?;  // AntSpawn launcher, or refuse
let guard = Guard::new(policy, sandbox, Ledger::open("audit.jsonl")?);
tools.policy(guard.clone());                                  // D|E seam: Policy::check before every execution
// at run time, for each call:  Allow -> run (inside `sandbox` if the tool executes code)
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
- **The AntSpawn constraints shape the tiers** (section 4.2): the `account` launcher is one-at-a-time and copy-only, and a VM per
  call is too heavy, so isolation cannot be uniform.

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

### 5.2 Enforcement tiers (E2)

| Tier | For | Mechanism | Notes |
|---|---|---|---|
| 0 | Read-only and edit tools on declared workspace roots | Policy only (path rules) | Not a boundary against a bug in our tool code; acceptable only for tools we wrote and tested |
| 1 | `shell` and other code-running tools | AntSpawn `account` launcher: restricted local account, kill-on-close Job Object, firewalled outbound, caps on time and RAM | One job at a time; copy-in/out of the workspace subset; setup is CireSnave's elevated step |
| 2 | Untrusted or networked code | AntSpawn `windows-sandbox` launcher: throwaway VM, declared mounts only | Heavy per launch; GPU unmeasured |

`Sandbox::select(needs, tier)` calls AntSpawn's `providers(needs)` and **refuses with the unmet need named** if nothing certified is
provisioned. It never falls back to a weaker launcher silently (the `naive` launcher is not an isolation tier).

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

## 6. Open questions (for the PM, not decided)

1. **Is a job-per-`shell`-call on the `account` launcher acceptable?** One at a time, copy-in/out, per-job setup unmeasured. If not,
   Tier 1 needs either a long-lived restricted session per run (a change to AntSpawn's one-job model) or a measurement first
   (ROADMAP row 13 leaves `account` unmeasured).
2. **Workspace edits and the copy-only `account` launcher:** keep edits in Tier 0 (policy only) and run only `shell` in Tier 1 against
   a copied workspace, then bring changes back as declared outputs? This is a product decision about what an edit-then-test loop looks like.
3. **A "tool call" consent kind** needs the OverMind lane (`user-request` kinds are a closed set, CONSENT.md open question 1).
4. **Who owns the audit reader** (the PM's pulse, AgentLife status, or a new command)?
5. **Network allowlists** are not provided by any launcher read; requires an AntSpawn change or is out of scope for the first cut.

## 7. What I did not verify

- Claude Code's own permission model (closed source; public docs not read).
- That any AntSpawn launcher is currently provisioned or passing on this machine (ROADMAP: tests runner-only; row 11 held); `spawn` was not run.
- codex `windows-sandbox-rs` mechanism (file names only); `sandboxing` crate (no README).
- The `user-request` crate's current API and published version (AgentLife docs only; they cite 0.11.1 and 0.16).
- That `Policy` with strictest-wins and examples-as-tests covers our real command set; no rule file was written or run.
- Windows Sandbox network behaviour beyond the DESIGN.md text; GPU.
