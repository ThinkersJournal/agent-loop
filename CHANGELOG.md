# Changelog

## [0.1.25] — docs: budget governor design note

Adds `docs/BUDGET-GOVERNOR.md`, a design note only (no code): what usage the loop already reports, an in-run token cap that stops through the existing soft-stop path, the fleet cap as the harness adapter's `cost` report plus `SoftStop`, four decisions for the PM, and gap-list rows for the budget governor, the harness adapter and the eval harness. Verified by reading `src/driver.rs`, `src/run.rs`, `src/events.rs` and capant's `docs/AGENT-STACK-GAPS.md` at `origin/main`; nothing was run.

## [0.1.24] — fix: LICENSE-APACHE restored to the canonical Apache-2.0 text

Section 6 now reads "except as required for reasonable and customary use in describing the origin of the Work and reproducing the content of the NOTICE file", as in the canonical text; section 9 heading and the appendix wording also follow it. The file is byte-identical (11358 bytes, git blob d645695) to LICENSE-APACHE in ciresnave/PersistAnt, which the PM named as canonical. The appendix line "Copyright 2026 Eric Evans" returns to the canonical placeholder. Verified by `git diff origin/main -- LICENSE-APACHE` and `git hash-object LICENSE-APACHE`.

## [0.1.23] — write_back_staged_recorded and StagedWriteBackTool, the recorded write-back and the workspace_writeback tool over an AntSpawn session (B1/B2 slice 6)

`write_back_staged_recorded` is `write_back_staged` with the run record kept (`docs/RUN-RECORD.md` 3): a `writeback.intent` naming AntSpawn's `set_hash` and the staged paths is appended before `apply` is sent, and a `writeback.done` after every outcome. It fails closed: if the intent cannot be recorded, `apply` is not sent and the outcome is `Refused`. `StagedWriteBackTool` offers `workspace_writeback` over an `AntSpawnSession` (only in copy-back, not when the policy denies), with or without a record. `write_back_staged` is unchanged in behaviour. Verified with `cargo test --test stagedwriteback_recorded` (7 tests against a scripted fake, not a real `spawn`) and `cargo test --test stagedwriteback`; an unfinished staged intent is not judged by `recover_unfinished`, because AntSpawn holds the working copy.

## [0.1.22] — feat: `write_back_staged`, write-back through an AntSpawn copy-back session (B1/B2 slice 5)

Adds `src/stagedwriteback.rs`: `change_set_from_staged` turns a `Staged` into the `ChangeSet` a person is asked about (holds are this crate's rules plus AntSpawn's own `refused` list; no base hashes, so every `base` is `None`), and `write_back_staged` fetches the changes, asks the `Approver` under AntSpawn's own `set_hash`, and sends `apply` with exactly the approved deletes (`deletes`) and other approved held paths (`protected`). A held path not approved by name sends no `apply`; a bind session and `WritePolicy::Deny` are refused before anything is sent; a conflict or refusal AntSpawn reports comes back as `ApplyError::Conflict` (both hashes `None`) or `ApplyError::Refused`. Ledger lines match `write_back`. `changeset::plain` and `holds_for` become `pub(crate)`. Declares the `stagedwriteback` component (adapter, extensions layer) in `.coderipper.toml`. Verified by 11 tests in `tests/stagedwriteback.rs` against a scripted fake written from AntSpawn's `docs/SESSION-PLAN.md`: **not run against a real `spawn`**, and not wired into `WriteBackTool` or the run record.

## [0.1.21] — docs: `ROADMAP.md` added

Adds `ROADMAP.md`: every row either done, naming the `cargo test` command that shows it, or open, naming its blocker, plus a table of each README headline claim with the command that shows it. Written at 225e369 after `cargo test --all-features` passed with exit 0 (every test binary `ok`, 0 failed, 0 ignored). No code change. Rows cite test files by name; whether a given test exercises the whole claim in its row was not audited test by test.

## [0.1.20] — chore: portfolio workflow pinned to CodeRipper 0.47.5

`.github/workflows/ci.yml` now calls `ciresnave/coderipper/.github/workflows/portfolio.yml` at `c72a6e03479d914de8ea8a73609ba73e93a0de92` (was `33cc3b7e4ab9305b5244092842fc7f6f6b686983`), which installs CodeRipper 0.47.5. The rule-id migration tool (`tools/migrate-rule-ids.py` at coderipper `c72a6e0`) reports "nothing to migrate": `.coderipper.toml` has no `check =` or `blocking =` entries and no workflow passes `--skip-rule`. No code change. The 0.47.5 portfolio job failed on `EFX-001 not run` (no component tagged `pure`), so `.coderipper.toml` now says `[architecture] none = ["pure", "composition"]`: no component is declared pure, and the library has no composition root in `src/`. That replaces the placeholder `[architecture.composition] roots = ["examples"]` (no such directory), which `none = ["composition"]` makes redundant (the 0.43.77 changelog calls an input both listed and declared a configuration error; not tested here).

## [0.1.19] — feat: `AntSpawnSession`, a client for `spawn session`'s JSON lines (exec, changes, apply, close) and `AntSpawnSandbox`

Slice 4 of the B1/B2 workspace modes. `AntSpawnSession<L: Lines>` speaks the lines in AntSpawn's `docs/SESSION-PLAN.md` (at `3c9461b`): `exec` (base64 output decoded), `changes` (the staged set with AntSpawn's own `set_hash`), `apply` (sends only the `deletes`/`protected` paths the caller passes; `Applied::written` is true only with nothing refused or in conflict), `close`. `changes` and `apply` are never sent for a bind session. `ChildLines` runs a real `spawn session` child and closes its input on drop; `AntSpawnSandbox` implements `Sandbox` over it. Adds the `base64` dependency. Verified by 7 tests against a scripted fake whose replies are written from the docs: **not run against the real account or a real `spawn` binary**. Bridging a `Staged` into the write-back tool's `ChangeSet` and `Ask` is not done.

## [0.1.18] — `[architecture]` declaration; `ToolOutcome` moved to `src/hook.rs`

Adds the `[architecture]` table to `.coderipper.toml` (21 components, 4 layers, no waivers) and `docs/ARCHITECTURE.md` with the disposition of the census findings. Breaks the `driver` <-> `hook` cycle (MOD-002) by defining `ToolOutcome` in `src/hook.rs` and re-exporting it from `src/driver.rs`; no public path changes. Repins the portfolio workflow in `.github/workflows/ci.yml` from coderipper 30e012b7 to 33cc3b7 (0.43.49), the first pin that parses `[architecture]`. Declares the volatile namespaces (`rig_agent`, `rig_core`, `persistant`, `opendal`), marks the policy components stable, and sets an `api_budget` so ARC-003 and ARC-006 run. Verified: `cargo test` and coderipper 0.43.20 ARC-001/002 and MOD-002 after the change.

## [0.1.17] — changelog fragments adopted

Pull requests no longer edit `CHANGELOG.md`, a `Cargo.toml` version or `Cargo.lock`: each adds one `changelog.d/<topic>.md` fragment (format in `changelog.d/README.md`) and the gate stamps the version it allocates with `tools/stamp-version.py`. Adds the portfolio fragment-stamp kit unchanged (`tools/stamp-version.py`, `tools/check-fragment.py`) and the `Changelog fragment` workflow (`.github/workflows/changelog-fragment.yml`) that fails a PR with no fragment or with two. Verified with `python tools/stamp-version.py 0.1.16 --dry-run` on this branch.

## [0.0.0] - baseline created by the stamp kit

No earlier entries were written.
