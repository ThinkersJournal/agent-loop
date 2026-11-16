# agent-loop roadmap

State measured at `origin/main` 225e369 (0.1.20, 2026-11-16): `cargo test --all-features` exit 0, every test binary `ok`, 0 failed, 0 ignored.
A "done" row names the command that shows it; an "open" row names what it waits on. "Done" means built and tested against our own fakes
unless the row says otherwise: nothing here has run against a real model provider, a real `spawn` binary or the real AntSpawn account.

Ruled by CireSnave 2026-11-06 (board 373 Q2), verbatim: *"373: B1 and B2.  I'd like the ability to do either one.  Lets build both and make it easy to choose one or the other."* B3 is not built (`docs/SANDBOX.md` 6.2).

| Row | What | Status | Test / command, or blocker |
|---|---|---|---|
| L1 | `Driver` over `Model`, `ToolHost`, `SessionStore` on rig-agent's run state machine; `Run`/`RunConfig` are our types | done | `cargo test --test mock_loop --test run` |
| L2 | Soft stop, inject, resume (`control`) | done | `cargo test --test control` |
| L3 | Typed `Event`s | done | `cargo test --test events` |
| L4 | `Hook`s (tool and model decisions, stop reasons) | done | `cargo test --test hooks` |
| L5 | `ContextPolicy` and the run summary | done | `cargo test --test context --test summary` |
| L6 | Idempotency `Journal` | done | `cargo test --test journal` |
| L7 | Durable `Inbox` (`MemoryInbox`) | done | `cargo test --test inbox` |
| P1 | `PathPolicy`, `CommandPolicy` (`[[path]]` rules with `match`/`not_match` examples checked at load) | done | `cargo test --test pathpolicy --test commandpolicy` |
| P2 | `NetworkPolicy`, `SecretPolicy` (unit tests in `src/networkpolicy.rs`, `src/secretpolicy.rs`) | done | `cargo test --lib` |
| P3 | Allow/Ask decisions that need a human: the consent channel | open | blocked on OverMind (`docs/SANDBOX.md` 6.4: a "tool call" consent kind is a closed-set change); the host must call `check_resume` |
| S1 | `FileStore`/`FileInbox` (flush with `sync_all`) | done, power-cut behaviour not tested | `cargo test --test file` |
| S2 | `BlobStore`/`BlobInbox` over PersistAnt `KeyedBlobs`, hash-checked, torn values fail closed, journal saved beside the snapshot | done | `cargo test --all-features --test blob_store --test blob_inbox` |
| S3 | Retire `FileStore`/`FileInbox` | open | blocked on a PersistAnt `Machine` blob backend (PersistAnt ROADMAP row 33) and a PM ruling (`docs/PERSIST-MIGRATION.md` 7, 8) |
| R1 | Workspace record log and resume fold (`RecordLog`, `ResumeFacts`), PersistAnt storage behind the `persistant` feature | done | `cargo test --all-features --test records` |
| R2 | `write_back_recorded`, `recover_unfinished`, `check_resume`, `RecordedWriteBackTool` | done | `cargo test --all-features --test writeback_recorded --test recorded_tool` |
| R3 | A record that survives a power cut (today: detect a torn record, not survive one; a lost final record is undetectable) | open | blocked on a PersistAnt blob backend that offers it |
| W1 | `ChangeSet` and reviewed write-back (`WriteBackTool`) | done | `cargo test --test changeset --test writeback` |
| W2 | Workspace modes B1 (bind) and B2 (copy-back) as one setting, `Sandbox` trait | done as types and fakes | `cargo test --test workspace` |
| W3 | `AntSpawnSession` client for `spawn session`'s JSON lines (exec, changes, apply, close) and `AntSpawnSandbox` | done against a scripted fake written from AntSpawn's `docs/SESSION-PLAN.md`; **not run against a real `spawn`** | `cargo test --test antspawn` |
| W4 | Run W3 against a real `spawn` and the real account | open | blocked on CireSnave (the account) and AntSpawn |
| W5 | Copy-mode `changes`/`apply` on AntSpawn's CLI protocol, and the byte format of its `set_hash` | open | asked of AntSpawn; reply awaited (a `synapse fetch` for an antspawn message) |
| W6 | Bridge `Staged` into the write-back tool's `ChangeSet`/`Ask` (AntSpawn's `set_hash` differs from ours; `Staged` has no base hashes, so the write-back conflict check stands in) | open | this lane; not started |
| W7 | Protected-path and build-script lists for change sets | open | the PM's proposal (`docs/SANDBOX.md` 5.6), not a ruling; also AntSpawn's unmeasured cases (rename or mkdir in a denied path, junction `.git`, a crash leaving the grant) |
| X1 | Wrap `Model`/`ToolHost`/`ContextPolicy` so rig-core types stop leaking (`docs/WRAP.md`, option C stage 1 only) | stage 1 done; stages 2 and 3 open | stages 2 and 3 wait for a consumer; not built on purpose |
| X2 | Persist the summary across a resume | open, not built on purpose | no consumer yet |
| X3 | Architecture declaration (`[architecture]`, 21 components, 4 layers) | done | `docs/ARCHITECTURE.md`; the portfolio CI job (CodeRipper 0.47.5 ARC rules) |

## README headline claims

| Claim (README) | Command that shows it |
|---|---|
| Drives rig-agent's serialisable, sans-I/O run state machine behind our own `Driver`/`Model`/`ToolHost`/`SessionStore` | `cargo test --test mock_loop --test run` (against a mock model, not a live provider) |
| Durable `Inbox`, typed `Event`s, `Hook`s, `ContextPolicy`, idempotency `Journal` | `cargo test --test inbox --test events --test hooks --test context --test journal` |
| Workspace modes and change-set write-back | `cargo test --test workspace --test changeset --test writeback` |
| Saved sessions are engine-specific | `cargo test --test control` saves and resumes a session (`soft_stop_finishes_the_batch_saves_and_resumes_with_the_queued_message`); the engine-specific format is rig-agent's (`docs/RIG-ASSESSMENT.md`); no test resumes one on another engine, so the "cannot" half is a statement of design, not a test |
| A damaged record is reported as torn, never trusted | `cargo test --all-features --test records` |
| A damaged snapshot fails closed until `BlobStore::clear_torn`; a damaged inbox message is skipped and listed by `BlobInbox::torn` | `cargo test --all-features --test blob_store --test blob_inbox` |
| `BlobStore::save_journal`/`load_journal` keep the journal beside the snapshot | `cargo test --all-features --test blob_store` |
| `FileStore`/`FileInbox` flush with `sync_all` | `cargo test --test file` shows round trips; it does not show a power cut (the README says "not power-cut tested") |
| A record is **not** promised to survive a power cut | no command: it is a limit, row R3 |
| Early and not published (`publish = false`, version 0.x) | `grep -n 'publish\|^version' Cargo.toml` |

— lane: agent-loop
