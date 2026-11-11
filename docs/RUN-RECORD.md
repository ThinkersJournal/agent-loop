# A durable run record for workspace facts (seam F, design note)

Status: **proposal, nothing built.** Task from the PM ("build it, design note first"): `docs/SANDBOX.md` 5.6 seam F says the base
manifest and the change set are "session records (`Event`), so a resumed session knows whether write-back happened". The PM's
direction: a durable record a resumed session reads, **stored through PersistAnt** (CireSnave: *"All of our projects should adopt
PersistAnt for persistence"*, `CIRESNAVE-EXPECTATIONS.md` 1069, and the five-step contract at 1100), events stay watch-only, and
this note states the storage need so PersistAnt's lookup picks the backend.

Read for this note: this repo at `origin/main` `7c23b2d` (0.1.2); PersistAnt at `origin/main` `23a5d34` (0.16.19), its `README.md`,
`vocabulary.rs` (the capability ids and their contracts) and `provider.rs` (`ProviderNeeds`, `providers`). crates.io has `persistant`
0.16.16 (`cargo search`). **The need in section 4 was checked by the PersistAnt lane** (message `[FINDING] run-record-providers`,
PersistAnt `main` `23a5d34`, `registry::providers` compiled and run, all backends): that result is reported there, as theirs, and
it changed the first version of this note. I did not run it myself.

## 1. Why events are not enough

`Event` is for watching (`events.rs`: "nothing in the run's saved state depends on a sink"). A resumed drive has no sink history, so
an event cannot tell it "write-back happened". The fact has to be stored, and read at resume.

It also cannot live in the run snapshot. The driver saves a snapshot **before** tools run (write-ahead) and again at the next
boundary. A write-back is a tool call, so a record appended to the run afterwards reaches disk only at the next snapshot:

| Crash point | Real tree | Run snapshot | A resumed session believes |
|---|---|---|---|
| after apply, before the next snapshot | changed | no record | "not written back" (wrong) |

Repeating the proposal is caught by the base-hash check (`Conflict`), so nothing is overwritten, but the person is shown a stale
proposal, and a held path they already approved is asked again. The record has to be durable **around the side effect**, not at the
next boundary, so it is its own log, written by the write-back path itself. (The snapshot is also the engine's format, `RUN_FORMAT`
and `deny_unknown_fields`; a host record in it is engine-specific, which is the README's existing caveat for saved sessions.)

## 2. What is recorded

One append-only list per session, JSON, each record `{ "v": 1, "seq": N, "kind": ..., ... }`:

| `kind` | Fields | Written |
|---|---|---|
| `workspace.opened` | `mode` (`copy-back`/`bind`), `root`, `policy_hash`, `base` (hash of the base manifest, or none under `bind`) | when the sandbox opens |
| `writeback.intent` | `set_hash`, `paths` | **before** `apply` touches the real tree |
| `writeback.done` | `set_hash`, `outcome`, `approved` (held paths the person approved) | after `apply`, denial, refusal or failure |

`outcome` is the existing `LedgerLine` vocabulary: `applied`, `denied`, `empty`, `failed:<why>`, `policy-deny`, `not-copy-back`.
The base manifest itself can be large, so it is stored once as a content-addressed value `manifest/<hash>` and the record carries
the hash. A record is a superset of a `Ledger` line: the ledger is the audit trail for people, the record is what resume reads.

## 3. Write order and failure rules (fail closed)

1. `intent` is appended and acknowledged **before** `apply`. If that append fails, `apply` does not run and the call fails.
2. `done` is appended after. If it fails after a successful apply, the call still reports `Applied` (the tree did change) and also
   carries the record error; it is never reported as a clean success, and never turned into "not applied".
3. `apply` is not atomic across files, so a crash can leave a **partial** write-back. On resume, an `intent` with no `done` is
   *unknown*: the host recomputes, per path, whether the real file equals the change set's new content, the base, or neither.
   All new: record `done{applied, recovered}`. All base: record `done{failed:interrupted}`. Mixed or neither: tell the person, do
   not retry on their behalf.
4. A resumed session reads the list and folds it into `ResumeFacts { mode, base, writebacks, unfinished }`. The driver does not
   need it; the host's workspace layer reads it, then tells the model (the mode is already part of the system context, 5.6).

## 4. The storage need, for PersistAnt

Per PersistAnt's contract (`CIRESNAVE-EXPECTATIONS.md` 1100): agent-loop is a library, so it **names no backend and keeps no
candidate list**. It exposes the need; the host's setup UI passes it to PersistAnt, the operator picks, the host stores the id and
opens it by name. The records are keyed values, one key per record (`records/<session>/<seq, zero-padded>`), plus `manifest/<hash>`:

| Need (capability id) | Why |
|---|---|
| `blob.read`, `blob.write` | read a record, write a record or manifest |
| `blob.list` | read a session's records in order (ascending bytewise, so zero-padded `seq` sorts) |
| `blob.create_exclusive` | take the next `seq` safely when two writers race: of any number of concurrent creates of one key, exactly one succeeds. It does the job `hard_link` does in `FileInbox` today |
| `blob.delete` | pruning old sessions (not needed for correctness) |
| minimum durability: `Process` | a record must survive a crash of the process (the point of §1) |

Speed does not matter (a handful of records per session). Not needed: `blob.append` (one shared log has no atomic sequence number;
per-record keys do), the `kv.*` family, relational.

**What the lookup returned** (PersistAnt lane, `main` `23a5d34`, run for real; relayed, not re-run here):
- The ids are `blob.read blob.write blob.list blob.create_exclusive blob.delete`. **`blob.create_exclusive_atomic` does not exist as
  a verified capability** on any backend, so the first version of this note, which asked for it, asked for something nobody offers.
  It is dropped.
- At `Process` durability the only backend is `fs`. `memory` is volatile; the SQL backends have no blob rows (blob is `fs` only).
- **`Machine` durability is unmet**: `fs` is `Process`, nothing offers `Machine`. So power-loss survival of a record is **not
  available** through PersistAnt today, and this note no longer asks for it. A record can be lost to a power cut after its
  `intent` was acknowledged and the tree changed. Then a resumed session sees no write-back, which is the stale view §1
  describes; the only backstop is the same one: re-proposing hits the base-hash `Conflict`, so nothing is overwritten. §3 point 3's
  recovery cannot help, since it starts from an `intent` that is gone.
- On `fs`, `create_exclusive` is exclusive only when the store is opened **without** `atomic_write_dir` (PersistAnt `store.rs:546`).
  The record store is therefore opened without it. That costs `blob.replace_atomic`, which this design does not use (each record is
  created once and never replaced).

**Consequence for readers: a value may be visible before it is complete.** Without an atomic create, a reader can see a record
while its writer is still writing it, or after a crash tore it. So every stored value is `<sha256 hex of body>\n<JSON body>`, and a
reader **ignores a value whose hash does not match** (a gap in `seq` is harmless). This is safe for the rules in §3: a record
torn by a crash was never acknowledged, so an unacknowledged `intent` means `apply` did not run.

**The guarantee (PM ruling, 2026-11-11: accept the gap now): detect, not survive.** The record store does not promise a record
survives a power cut; it promises a damaged one is **noticed**. A resumed session reads the records, and:
- a value whose hash does not match is a **torn record**: it is reported as torn, never trusted, and never read as "no write-back";
- a gap in `seq` (a record in the middle is missing) is reported the same way;
- **not detectable:** the *last* record lost whole (no later record shows the gap), and a record lost together with every record
  after it. Those look like a session that stopped earlier. For `writeback.intent` that is the stale view of §1, and the base-hash
  `Conflict` is still the backstop.
`ResumeFacts` (§6) carries `torn` and `gaps`, so the host can tell the person. This is revisited when PersistAnt has a `Machine`
durability blob backend (the PM is asking the PersistAnt lane to log that as a gap row).

## 5. A finding that goes beyond this task: `FileStore` and `FileInbox`

The standing rule is that persistence goes through PersistAnt and a project does not keep a private backend. This crate already does:
`FileStore` and `FileInbox` (`file.rs`, 0.0.24) are hand-written disk backends (temp file, `sync_all`, `hard_link`). The need in §4
is the same one they meet (`create_exclusive` for the next sequence or id, `list`, `read`, `Process` durability), plus `blob.delete`
for "keep the newest two snapshots". One difference to check before promising it: `FileStore` publishes a snapshot with `hard_link`,
so a reader never sees a partial snapshot, and `fs` through PersistAnt does not give that (above); the same hash-checked value rule
would have to cover snapshots, and a snapshot is large. So one need set would serve all three seams, and `PersistStore`, `PersistInbox` and `PersistRecords`
could sit over one opened handle.

I have not started this. It replaces shipped code and decides the future of `FileStore`/`FileInbox` (keep as the zero-dependency
reference, or remove once PersistAnt-backed ones exist), which is the PM's call. `MemoryStore`/`MemoryInbox` are test fakes and stay.

## 6. What would be built (each its own PR, none started)

1. `Records` trait (`append` returns the `seq`, `list`) with `MemoryRecords` and a `PersistRecords` adapter over PersistAnt's
   backend-agnostic handle, plus `records_needs()` returning the §4 need for the host to pass on. Non-breaking.
2. `write_back_recorded`: intent before apply, done after, §3 rules, and the unknown-state recovery. The existing `write_back` and
   `WriteBackTool` stay as they are (their `Ledger::record` cannot fail, so it cannot stop an apply); a breaking change that folds
   them together is possible later and would be a 0.n bump.
3. `ResumeFacts` fold and the host-side read. Optionally a watch-only `Event` per record; not needed for resume.

**Built so far:** item 1 (#31) and item 2 (`write_back_recorded`, `recover_unfinished`, with `Recorded` and `Recovery`). Item 3's host-side
reading is not built, and `WriteBackTool` does not call `write_back_recorded` yet (its fields are public, so adding a log there is a
breaking change or a second tool type).

## 7. Open questions (for the PM, not decided)

1. §5: migrate the store and inbox to PersistAnt too, and what happens to `FileStore`/`FileInbox`?
2. Answered by the PersistAnt lane (see §4): `fs` is the one backend, `Process` durability only; depend on crates.io `persistant`
   0.16.16 (0.16.17 to 0.16.19 are not published, and the lane says none touch blob). Is "records can be lost to a power cut" acceptable
   given the §3 recovery, or does it wait for a `Machine`-durable blob backend (a PersistAnt row)? **Ruled by the PM 2026-11-11: accept
   it now, detect not survive (§4 guarantee); revisit when PersistAnt has that backend.**
3. §3 point 3: is "mixed state is shown to the person, never retried" the right rule, or should a policy decide?
4. The `workspace.opened` record under `bind`: there is no base manifest; should it record the ACL grant's identity instead (needs
   AntSpawn A2, not built)?
