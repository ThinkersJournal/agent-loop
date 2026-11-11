// SPDX-License-Identifier: MIT OR Apache-2.0
//! A [`SessionStore`] over any [`KeyedBlobs`] (`docs/PERSIST-MIGRATION.md`, slice 1): the way a session is persisted through
//! PersistAnt without a private disk backend of our own.
//!
//! One value per snapshot, `snap/<session>/<seq>`, stored as `<sha256 hex>\n<snapshot>`. `save(expected)` takes sequence
//! `expected + 1` by creating that key only if absent; "already exists" is a [`StoreError::Conflict`], exactly as the optimistic rule
//! says. The newest two snapshots are kept.
//!
//! **A value can be visible before it is complete** on some backends (PersistAnt `fs` writes in place), so every snapshot is
//! hash-checked: `load` returns the newest **verified** snapshot and falls back to the one before when the newest is damaged.
//!
//! **A damaged snapshot above the newest verified one blocks the next sequence number** (the name is taken). That fails **closed**:
//! `save` returns an error naming it and does not delete anything, because the value might be a second, live writer's. When the host
//! knows no other writer exists it calls [`BlobStore::clear_torn`]. This is the policy recommended in the design note; it is not yet
//! a PM ruling.
//!
//! **Durability is the backend's.** On PersistAnt `fs` that is `Process`, not `Machine`: a power cut can lose the newest snapshot.
//! `FileStore` (which flushes with `sync_all` before publishing) stays for that case; see the note.

use crate::file::encode;
use crate::records::{KeyedBlobs, RecordsError, open_value, seal};
use crate::store::{SessionStore, StoreError};

fn backend(e: RecordsError) -> StoreError {
    StoreError::Backend(e.to_string())
}

/// One snapshot key that exists, and its text if it passed its hash check.
struct Found {
    seq: u64,
    verified: Option<String>,
}

pub struct BlobStore<B> {
    blobs: B,
}

impl<B: KeyedBlobs> BlobStore<B> {
    pub fn new(blobs: B) -> Self {
        Self { blobs }
    }

    fn prefix(session: &str) -> Result<String, StoreError> {
        let name = encode(session).ok_or_else(|| StoreError::Backend("empty session id".into()))?;
        Ok(format!("snap/{name}/"))
    }

    fn key(prefix: &str, seq: u64) -> String {
        format!("{prefix}{seq:020}")
    }

    /// Every snapshot key under `prefix`, ascending, each read and checked.
    fn found(&self, prefix: &str) -> Result<Vec<Found>, StoreError> {
        let mut seqs: Vec<u64> = self
            .blobs
            .list(prefix)
            .map_err(backend)?
            .iter()
            .filter_map(|k| k.strip_prefix(prefix)?.parse().ok())
            .collect();
        seqs.sort_unstable();
        seqs.into_iter()
            .map(|seq| {
                let verified = self
                    .blobs
                    .read(&Self::key(prefix, seq))
                    .map_err(backend)?
                    .and_then(|b| open_value(&b));
                Ok(Found { seq, verified })
            })
            .collect()
    }

    /// The error or conflict for a save of `expected + 1` that cannot proceed, from what is stored.
    fn refuse(expected: u64, found: &[Found]) -> StoreError {
        let newest_verified = found
            .iter()
            .rev()
            .find(|f| f.verified.is_some())
            .map_or(0, |f| f.seq);
        let max = found.last().map_or(0, |f| f.seq);
        if newest_verified != expected {
            return StoreError::Conflict {
                expected,
                actual: newest_verified,
            };
        }
        // The stored sequence is the one expected, yet the name `expected + 1` (or a later one) is taken by an unverifiable value.
        StoreError::Backend(format!(
            "damaged snapshot at sequence {max} blocks the next save (after {expected}); \
             if no other writer is running, call clear_torn"
        ))
    }

    /// Delete every snapshot above the newest verified one, and every damaged journal blob (values a crash left behind). Returns how many.
    ///
    /// **Call it only when no other writer exists**: a damaged-looking value can be a live writer's snapshot still being written,
    /// and deleting it would defeat the rule that a second writer is refused. The backend must support delete.
    pub fn clear_torn(&self, session: &str) -> Result<usize, StoreError> {
        let prefix = Self::prefix(session)?;
        let found = self.found(&prefix)?;
        let newest = found
            .iter()
            .rev()
            .find(|f| f.verified.is_some())
            .map_or(0, |f| f.seq);
        let mut n = 0;
        for f in found.iter().filter(|f| f.seq > newest) {
            self.blobs
                .delete(&Self::key(&prefix, f.seq))
                .map_err(backend)?;
            n += 1;
        }
        // Damaged journal blobs too (same rule: only when no other writer exists).
        let jp = Self::journal_prefix(session)?;
        for seq in self.journal_seqs(&jp)? {
            let key = Self::key(&jp, seq);
            let intact = self
                .blobs
                .read(&key)
                .map_err(backend)?
                .is_some_and(|b| open_value(&b).is_some());
            if !intact {
                self.blobs.delete(&key).map_err(backend)?;
                n += 1;
            }
        }
        Ok(n)
    }

    fn journal_prefix(session: &str) -> Result<String, StoreError> {
        let name = encode(session).ok_or_else(|| StoreError::Backend("empty session id".into()))?;
        Ok(format!("journal/{name}/"))
    }

    /// Keep the idempotency [`Journal`](crate::Journal)'s JSON (`Journal::to_json`) for the snapshot `seq` that `save` just returned:
    /// one hash-checked blob per sequence number, `journal/<session>/<seq>`, created only if absent. Storing the same text again is
    /// fine (a retry); different text for a taken `seq`, or a damaged blob in the way, is an error and nothing is overwritten.
    /// The newest two are kept. Call it after `save`; on resume call [`load_journal`](Self::load_journal) beside `load`.
    pub fn save_journal(&self, session: &str, seq: u64, json: &str) -> Result<(), StoreError> {
        let prefix = Self::journal_prefix(session)?;
        let key = Self::key(&prefix, seq);
        if !self
            .blobs
            .create_if_absent(&key, seal(json))
            .map_err(backend)?
        {
            let same = self
                .blobs
                .read(&key)
                .map_err(backend)?
                .and_then(|b| open_value(&b));
            return match same {
                Some(t) if t == json => Ok(()),
                Some(_) => Err(StoreError::Backend(format!(
                    "journal for sequence {seq} is already stored with different content"
                ))),
                None => Err(StoreError::Backend(format!(
                    "damaged journal at sequence {seq}; if no other writer is running, call clear_torn"
                ))),
            };
        }
        // Keep the newest two. A failed delete only leaves an old blob behind.
        let mut seqs = self.journal_seqs(&prefix)?;
        seqs.retain(|s| *s != seq);
        seqs.sort_unstable();
        for old in seqs.into_iter().filter(|s| *s + 1 < seq) {
            let _ = self.blobs.delete(&Self::key(&prefix, old));
        }
        Ok(())
    }

    /// The newest verified journal JSON and the snapshot sequence it belongs to, or `None` if there is none. A damaged newer blob is
    /// skipped. **Compare the sequence with `load`'s**: a journal older than the snapshot missed the calls made since.
    pub fn load_journal(&self, session: &str) -> Result<Option<(u64, String)>, StoreError> {
        let prefix = Self::journal_prefix(session)?;
        let mut seqs = self.journal_seqs(&prefix)?;
        seqs.sort_unstable();
        for seq in seqs.into_iter().rev() {
            if let Some(text) = self
                .blobs
                .read(&Self::key(&prefix, seq))
                .map_err(backend)?
                .and_then(|b| open_value(&b))
            {
                return Ok(Some((seq, text)));
            }
        }
        Ok(None)
    }

    fn journal_seqs(&self, prefix: &str) -> Result<Vec<u64>, StoreError> {
        Ok(self
            .blobs
            .list(prefix)
            .map_err(backend)?
            .iter()
            .filter_map(|k| k.strip_prefix(prefix)?.parse().ok())
            .collect())
    }

    /// Sequence numbers of snapshots that failed their hash check, so a host can report them.
    pub fn torn(&self, session: &str) -> Result<Vec<u64>, StoreError> {
        let prefix = Self::prefix(session)?;
        Ok(self
            .found(&prefix)?
            .into_iter()
            .filter(|f| f.verified.is_none())
            .map(|f| f.seq)
            .collect())
    }
}

impl<B: KeyedBlobs> SessionStore for BlobStore<B> {
    fn load(&self, session: &str) -> Result<Option<(u64, String)>, StoreError> {
        let prefix = Self::prefix(session)?;
        let found = self.found(&prefix)?;
        if found.is_empty() {
            return Ok(None);
        }
        // The newest verified snapshot; a damaged newer one is skipped (see `torn`).
        match found
            .into_iter()
            .rev()
            .find_map(|f| f.verified.map(|text| (f.seq, text)))
        {
            Some(hit) => Ok(Some(hit)),
            None => Err(StoreError::Backend(
                "every stored snapshot of this session is damaged".into(),
            )),
        }
    }

    fn save(&mut self, session: &str, expected: u64, snapshot: &str) -> Result<u64, StoreError> {
        let prefix = Self::prefix(session)?;
        let found = self.found(&prefix)?;
        let max = found.last().map_or(0, |f| f.seq);
        if max != expected {
            return Err(Self::refuse(expected, &found));
        }
        // The stored newest key is the one expected; it must also verify (a damaged newest means the caller's `expected` came
        // from somewhere else).
        if expected > 0 && found.last().is_some_and(|f| f.verified.is_none()) {
            return Err(Self::refuse(expected, &found));
        }
        let next = expected + 1;
        if !self
            .blobs
            .create_if_absent(&Self::key(&prefix, next), seal(snapshot))
            .map_err(backend)?
        {
            // Lost a race for the name: say what is there now.
            return Err(Self::refuse(expected, &self.found(&prefix)?));
        }
        // Keep the newest two. A failed delete only leaves an old snapshot behind.
        for f in found.iter().filter(|f| f.seq + 1 < next) {
            let _ = self.blobs.delete(&Self::key(&prefix, f.seq));
        }
        Ok(next)
    }
}
