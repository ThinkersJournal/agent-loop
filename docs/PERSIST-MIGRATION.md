# Moving `FileStore` and `FileInbox` onto PersistAnt (design note)

Status: **built (slices 1 to 3, 0.1.10 to 0.1.12); rulings recorded in section 7.** PM notice 2026-11-11 ("schedule moving them onto the same need set after the run record is
built, as rows, design first, one PR each"): `FileStore` and `FileInbox` (`file.rs`, 0.0.24) are private disk backends, which
CireSnave's rule forbids (*"All of our projects should adopt PersistAnt for persistence"*, `CIRESNAVE-EXPECTATIONS.md` 1069, and the
exception at 1070: where a project cannot, the lane states the gap as a roadmap row with an owner). The run record is built
(#31 to #34), so this is the next row.

Read for this note: this repo at `origin/main` `2d2acbb` (0.1.8), `store.rs`, `file.rs`, `records.rs`; `persistant` 0.16.16 from the
crates.io registry copy (`store.rs`, `blocking.rs`). Where the PersistAnt lane has run a lookup I say so; where I read code I say
"read", and nothing below was run against a power cut.

## 1. What the two backends promise today

| | `FileStore` | `FileInbox` |
|---|---|---|
| Unit | one snapshot per `seq`, `<session>/<seq>.snap`, newest two kept | one file per message `<id>.msg`, an empty `<id>.ack` once acknowledged |
| New number | `expected + 1`, taken by creating that file; a taken name is `Conflict` | next id = highest `.msg` + 1, retried if taken |
| No partial value visible | temp file, `sync_all`, then `hard_link` to the final name: a reader sees all of the file or none | same |
| Flush | `sync_all` on the temp file before it is published (read in `file.rs`; **power loss not tested**) | same |
| Extras | | `prune` (0.1.2) |

The third row is the one PersistAnt does not give (section 3).

## 2. The same need set, and one abstraction

Both need what the run record needs (`RUN-RECORD.md` 4, checked by the PersistAnt lane): create-if-absent, read, list (with stat),
delete, and `Process` durability. So one opened store serves all three seams. The proposal is **no second adapter layer**: reuse
`KeyedBlobs` (create-if-absent, list, read; `MemoryBlobs` and `PersistBlobs` already implement it) and add one defaulted method,
`delete`, which returns "unsupported" unless an implementer overrides it (so it is not a breaking change to implement the trait).
Then:

- `BlobStore<B: KeyedBlobs>` implements `SessionStore`. `save(expected)` creates `snap/<session>/<expected+1>` with create-if-absent;
  "already exists" is `Conflict`. `load` reads the highest verified snapshot. Old snapshots beyond the newest two are deleted after
  the new one is created.
- `BlobInbox<B: KeyedBlobs>` implements `Inbox` the way `FileInbox` does (messages, ack markers, `prune`), keyed under
  `inbox/<session>/`.

`records.rs` already does the sequence-taking and hash-checking for records; the two new types share those helpers.

## 3. Where PersistAnt is weaker, and what that costs

Read in `persistant` 0.16.16 (`create_if_absent`, `Config::Fs`): on `fs`, create-if-absent is exclusive only when the store is opened
**without** `atomic_write_dir`, and then "the file is created exclusively but written in place, so a reader may see a partly written
value". The PersistAnt lane reports (2026-11-11) that `fs` is `Process` durability and nothing offers `Machine`.

**Torn values (visible-before-complete).** The records fix carries over: every stored value is `<sha256 hex>\n<body>` and a reader
ignores one whose hash does not match. For snapshots `load` returns the highest **verified** one, so a half-written newest snapshot
falls back to the previous one (that is why the newest two are kept, and the old one is deleted only after the new one is
created). For inbox messages `pending` skips an unverified message; `BlobInbox::torn(session)` lists the ids so nothing is dropped
silently. A torn message was never acknowledged to its sender (`append` had not returned), so skipping it loses nothing the sender
was told was kept.

**A torn snapshot blocks the next number.** After a crash mid-write, `snap/<session>/<n+1>` exists but fails verification. The next
`save(expected = n)` would create the same name and be told "already exists", which reads as `Conflict` forever. `hard_link` never
leaves such a name. Options: (a) delete any unverifiable snapshot above the highest verified one automatically; (b) fail closed
with a precise error and give the host `BlobStore::clear_torn(session)` to call once it knows there is no other writer; (c) delete
it if it is older than some time. **Recommendation (b):** (a) can delete another live writer's in-flight value, which breaks the
"second writer is refused" rule `SessionStore` exists for; (c) needs a clock and `modified`, which PersistAnt may report as unknown.
(b) costs a manual step after a crash that tore a snapshot, which is rare, and the error names the file.

**Durability is weaker for snapshots than for records, and that matters more.** Records are evidence, and the PM accepted
"detect, not survive" for them. A snapshot is different: the driver saves a **write-ahead** snapshot before the first tool of a
turn runs, so that after a crash the model's reply is durable and it does not re-choose a call under a new id (`journal.rs` docs).
If a power cut loses that snapshot while the tool ran, a resume loads the older one and the model may choose the call again, **under
a new id, which the idempotency `Journal` cannot recognise**. With `Process` durability that is possible; with `FileStore`'s
`sync_all` it is less likely (not tested). I have not measured how likely either is.

## 4. Recommendation

1. **Build `BlobStore` and `BlobInbox` over `KeyedBlobs`, with PersistAnt `fs` as the first real backend,** as the way the default
   host persists a session. Same crates.io `persistant` 0.16.16, same `persistant` feature.
2. **Keep `FileStore` and `FileInbox`, renamed in the docs as "the power-loss-careful option", until PersistAnt has a `Machine`
   blob backend.** This is the exception the rule allows: a measured gap (the lane's `providers` run: `Machine` unmet), stated as a
   roadmap row with an owner. The row: *"agent-loop's `FileStore`/`FileInbox` exist because no PersistAnt blob backend offers
   `Machine` durability; owner: PersistAnt (log the gap); agent-loop removes them when one does."* The PM already asked the
   PersistAnt lane to log that gap for the run record; it is the same gap.
3. Do not delete anything now. Deleting `FileStore` would trade a stronger snapshot flush for rule compliance without anyone having
   said that trade is acceptable.

## 5. Slices (each its own PR; all three built)

1. `KeyedBlobs::delete` (defaulted) and `BlobStore`, with `MemoryBlobs` and a real `fs` PersistAnt store in the tests, including a
   torn newest snapshot (fallback), a torn claim (the precise error, then `clear_torn`), and two writers (the second gets `Conflict`).
2. `BlobInbox` with `prune` and `torn`, same tests, and the existing `FileInbox` tests ported as a shared suite run against both.
3. README and doc updates: which backend to use when, and the gap row. No code removed.

**Built:** slice 1 (#36, `KeyedBlobs::delete` defaulted, `BlobStore`), slice 2 (#37, `BlobInbox`), slice 3 (this change: README and docs).
Nothing was removed: `FileStore` and `FileInbox` stay.

## 6. Open questions (for the PM, not decided)

1. Is the section 3 durability risk for snapshots acceptable with `Process` durability, or does the default host keep `FileStore`
   until `Machine` exists (my recommendation keeps both and lets the host choose)?
2. Torn-claim policy: (b), explicit `clear_torn`, as recommended?
3. The idempotency `Journal` is persisted "wherever the session is persisted" by the host (`journal.rs`); should `BlobStore` also
   hold it, so the host does not have to find another place? Not in this note's slices.
4. Who logs the roadmap row of section 4.2 (the PM asked the PersistAnt lane for the records gap; this adds snapshots and the inbox)?

## 7. Rulings (PM, 2026-11-11, peer message; not a CireSnave ruling)

1. Build `BlobStore`/`BlobInbox` and **keep `FileStore`/`FileInbox`** as the power-loss-careful default until PersistAnt has a
   `Machine` blob backend (gap logged by the PM as row 33). Question 1 above is answered this way.
2. Torn claim: explicit `BlobStore::clear_torn`, fail closed (question 2). `WriteBackTool` stays as a second tool type.
3. Questions 3 and 4 stay open: `BlobStore` does not hold the `Journal`.

## 8. Which backend to use

| Need | Use |
|---|---|
| Default, PersistAnt-backed, `Process` durability (a crash loses at most the newest value; damage is detected by hash) | `BlobStore` / `BlobInbox` over `PersistBlobs` (feature `persistant`), or any `KeyedBlobs` |
| A power cut must not lose the newest snapshot or message | `FileStore` / `FileInbox` (`sync_all` before publishing; not power-cut tested) |
| Tests, a single process | `MemoryStore` / `MemoryInbox` |

Remove `FileStore`/`FileInbox` only when a PersistAnt blob backend offers `Machine` durability **and** the PM rules it.
