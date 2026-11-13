# Changelog

## [0.1.19] — feat: `AntSpawnSession`, a client for `spawn session`'s JSON lines (exec, changes, apply, close) and `AntSpawnSandbox`

Slice 4 of the B1/B2 workspace modes. `AntSpawnSession<L: Lines>` speaks the lines in AntSpawn's `docs/SESSION-PLAN.md` (at `3c9461b`): `exec` (base64 output decoded), `changes` (the staged set with AntSpawn's own `set_hash`), `apply` (sends only the `deletes`/`protected` paths the caller passes; `Applied::written` is true only with nothing refused or in conflict), `close`. `changes` and `apply` are never sent for a bind session. `ChildLines` runs a real `spawn session` child and closes its input on drop; `AntSpawnSandbox` implements `Sandbox` over it. Adds the `base64` dependency. Verified by 7 tests against a scripted fake whose replies are written from the docs: **not run against the real account or a real `spawn` binary**. Bridging a `Staged` into the write-back tool's `ChangeSet` and `Ask` is not done.

## [0.1.18] — `[architecture]` declaration; `ToolOutcome` moved to `src/hook.rs`

Adds the `[architecture]` table to `.coderipper.toml` (21 components, 4 layers, no waivers) and `docs/ARCHITECTURE.md` with the disposition of the census findings. Breaks the `driver` <-> `hook` cycle (MOD-002) by defining `ToolOutcome` in `src/hook.rs` and re-exporting it from `src/driver.rs`; no public path changes. Repins the portfolio workflow in `.github/workflows/ci.yml` from coderipper 30e012b7 to 33cc3b7 (0.43.49), the first pin that parses `[architecture]`. Declares the volatile namespaces (`rig_agent`, `rig_core`, `persistant`, `opendal`), marks the policy components stable, and sets an `api_budget` so ARC-003 and ARC-006 run. Verified: `cargo test` and coderipper 0.43.20 ARC-001/002 and MOD-002 after the change.

## [0.1.17] — changelog fragments adopted

Pull requests no longer edit `CHANGELOG.md`, a `Cargo.toml` version or `Cargo.lock`: each adds one `changelog.d/<topic>.md` fragment (format in `changelog.d/README.md`) and the gate stamps the version it allocates with `tools/stamp-version.py`. Adds the portfolio fragment-stamp kit unchanged (`tools/stamp-version.py`, `tools/check-fragment.py`) and the `Changelog fragment` workflow (`.github/workflows/changelog-fragment.yml`) that fails a PR with no fragment or with two. Verified with `python tools/stamp-version.py 0.1.16 --dry-run` on this branch.

## [0.0.0] - baseline created by the stamp kit

No earlier entries were written.
