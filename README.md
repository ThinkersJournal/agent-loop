# agent-loop

An agent loop core in Rust. Early and not published (`publish = false`, version 0.x).

It drives rig-agent's serialisable, sans-I/O run state machine behind our own seams: a `Driver` over a `Model`, a `ToolHost`
and a `SessionStore`, with a durable `Inbox`, typed `Event`s, `Hook`s, a `ContextPolicy`, an idempotency `Journal`, and the
workspace modes and change-set write-back that `docs/SANDBOX.md` describes. The run handle (`Run`) and its settings
(`RunConfig`) are our own types.

**Saved sessions are engine-specific.** What a `SessionStore` holds is rig-agent's serialised run, in its own format version, so
a session saved by this driver cannot be resumed by a driver built on another engine. A fallback to goose-agent would cover new
sessions only. The traits `Model`, `ToolHost` and `ContextPolicy` still name rig-core types; `docs/WRAP.md` says what wrapping
them would cost and why that waits for a consumer.

**Workspace records** (`docs/RUN-RECORD.md`): the log and its resume fold exist (`RecordLog`, `ResumeFacts`; PersistAnt storage behind the
`persistant` feature), and `write_back_recorded` / `recover_unfinished` write and settle them, `check_resume` reads them at resume, and `RecordedWriteBackTool` is the write-back tool that keeps them (`WriteBackTool` does not). The guarantee is *detect, not survive*: a
record is stored through PersistAnt with a hash, so a record damaged by a crash or power cut is reported as torn, never trusted, but
a record is not promised to survive a power cut (no PersistAnt blob backend offers that yet). A lost final record cannot be detected.

**Sessions through PersistAnt:** `BlobStore` is a `SessionStore` over any `KeyedBlobs` (PersistAnt `fs` with the `persistant` feature),
hash-checked, with a fail-closed rule for a damaged snapshot. It has `Process` durability at best; `FileStore` stays for the
power-loss-careful case (`docs/PERSIST-MIGRATION.md`).

Licensed under either of the Apache License, Version 2.0 (`LICENSE-APACHE`) or the MIT licence (`LICENSE-MIT`), at your
option.
