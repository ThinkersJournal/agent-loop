// SPDX-License-Identifier: MIT OR Apache-2.0
//! The durable run record for workspace facts (`docs/RUN-RECORD.md`, slice 1): what a resumed session reads to learn whether a
//! write-back happened.
//!
//! One append-only list of records per session, kept as one keyed value per record (`records/<session>/<seq>`). Every stored value
//! is `<sha256 hex of body>\n<JSON body>`, because a backend may show a value before it is complete (PersistAnt's `create_if_absent`
//! on `fs` writes in place). A reader **ignores a value whose hash does not match** and reports it as torn.
//!
//! **The guarantee is detect, not survive** (PM ruling, 2026-11-11). A damaged record is reported ([`Listing::torn`]) and a missing
//! one in the middle shows as a gap ([`Listing::gaps`]); a record is not promised to survive a power cut, and the last record lost
//! whole is not detectable. The base-hash `Conflict` at write-back is the backstop for that case.
//!
//! The storage is the [`KeyedBlobs`] trait, so a backend is the host's choice and this crate names none. With the `persistant` cargo
//! feature, `PersistBlobs` is that trait over PersistAnt's blocking `Store`; the host opens the store (it picks the backend) and
//! `records_needs` says what the store must declare.

use std::collections::BTreeMap;
use std::ops::RangeInclusive;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::file::encode;

#[derive(Debug, PartialEq, Eq)]
pub enum RecordsError {
    /// The backend failed (message only).
    Backend(String),
    /// A record could not be written after trying the next sequence numbers.
    NoSequence,
    /// The session id is empty.
    BadSession,
}

impl std::fmt::Display for RecordsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Backend(m) => write!(f, "records backend: {m}"),
            Self::NoSequence => f.write_str("records: could not take a sequence number"),
            Self::BadSession => f.write_str("records: empty session id"),
        }
    }
}
impl std::error::Error for RecordsError {}

/// The small storage the record log and the session store need: create a key only if absent, list keys under a prefix, read a key,
/// and (optionally) delete one.
pub trait KeyedBlobs: Send + Sync {
    /// Store `bytes` at `key` only if nothing is there. `Ok(false)` means the key already existed and nothing changed.
    fn create_if_absent(&self, key: &str, bytes: Vec<u8>) -> Result<bool, RecordsError>;
    /// Every key that starts with `prefix`, in any order.
    fn list(&self, prefix: &str) -> Result<Vec<String>, RecordsError>;
    /// The value at `key`, or `None` if absent.
    fn read(&self, key: &str) -> Result<Option<Vec<u8>>, RecordsError>;
    /// Delete `key`; deleting an absent key is not an error. Defaulted so a backend written before this method existed still
    /// compiles: it answers "cannot delete", and the callers that need it (`BlobStore::clear_torn`) fail instead of pretending.
    fn delete(&self, _key: &str) -> Result<(), RecordsError> {
        Err(RecordsError::Backend("this backend cannot delete".into()))
    }
}

/// In-memory [`KeyedBlobs`]. Clones share one map, so a test can drop a log and open another over the same blobs.
#[derive(Clone, Default)]
pub struct MemoryBlobs(Arc<Mutex<BTreeMap<String, Vec<u8>>>>);

impl MemoryBlobs {
    pub fn new() -> Self {
        Self::default()
    }
}

impl KeyedBlobs for MemoryBlobs {
    fn create_if_absent(&self, key: &str, bytes: Vec<u8>) -> Result<bool, RecordsError> {
        let mut m = self.0.lock().expect("lock");
        if m.contains_key(key) {
            return Ok(false);
        }
        m.insert(key.to_string(), bytes);
        Ok(true)
    }
    fn list(&self, prefix: &str) -> Result<Vec<String>, RecordsError> {
        let m = self.0.lock().expect("lock");
        Ok(m.keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect())
    }
    fn read(&self, key: &str) -> Result<Option<Vec<u8>>, RecordsError> {
        Ok(self.0.lock().expect("lock").get(key).cloned())
    }
    fn delete(&self, key: &str) -> Result<(), RecordsError> {
        self.0.lock().expect("lock").remove(key);
        Ok(())
    }
}

/// A record that passed its hash check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stored {
    pub seq: u64,
    /// The JSON body (the hash is already checked and removed).
    pub body: String,
}

/// What a session's records hold, with the damage found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    /// Records that passed their hash check, ascending by `seq`.
    pub records: Vec<Stored>,
    /// Sequence numbers whose value failed its hash check (or had no hash): never trusted.
    pub torn: Vec<u64>,
    /// Ranges of sequence numbers with no value at all, below the highest one present (starting from 1).
    pub gaps: Vec<RangeInclusive<u64>>,
}

/// Append-only records for a session.
pub trait Records: Send + Sync {
    /// Store `body` (JSON text) as the session's next record and return its sequence number (from 1). Durable as far as the backend is.
    fn append(&self, session: &str, body: &str) -> Result<u64, RecordsError>;
    /// Every record, with torn values and gaps reported, never skipped silently.
    fn list(&self, session: &str) -> Result<Listing, RecordsError>;

    /// Append a typed record.
    fn append_record(&self, session: &str, rec: &WorkspaceRecord) -> Result<u64, RecordsError> {
        let body = serde_json::to_string(&Envelope {
            v: FORMAT,
            rec: rec.clone(),
        })
        .map_err(|e| RecordsError::Backend(e.to_string()))?;
        self.append(session, &body)
    }

    /// What a resumed session needs to know, folded from the records.
    fn resume_facts(&self, session: &str) -> Result<ResumeFacts, RecordsError> {
        Ok(ResumeFacts::fold(&self.list(session)?))
    }
}

const FORMAT: u32 = 1;

#[derive(Serialize, Deserialize)]
struct Envelope {
    v: u32,
    #[serde(flatten)]
    rec: WorkspaceRecord,
}

/// The facts a session records about its workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum WorkspaceRecord {
    /// The sandbox opened. `base` is the hash of the base manifest (copy-back), or `None` (bind).
    #[serde(rename = "workspace.opened")]
    Opened {
        /// `copy-back` or `bind` (`WorkspaceMode::as_str`).
        mode: String,
        root: String,
        policy_hash: String,
        base: Option<String>,
    },
    /// A write-back is about to touch the real tree. Appended and acknowledged **before** it does.
    #[serde(rename = "writeback.intent")]
    Intent {
        set_hash: String,
        paths: Vec<String>,
    },
    /// A write-back ended. `outcome` uses the `LedgerLine` words (`applied`, `denied`, `empty`, `failed:<why>`, ...).
    #[serde(rename = "writeback.done")]
    Done {
        set_hash: String,
        outcome: String,
        approved: Vec<String>,
    },
}

/// What a resumed session learns from its records.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResumeFacts {
    /// The latest `workspace.opened`, if any.
    pub opened: Option<WorkspaceRecord>,
    /// Every `writeback.done`, oldest first.
    pub done: Vec<WorkspaceRecord>,
    /// The last `writeback.intent` with no later `writeback.done` for the same set: the state is **unknown**, and the host
    /// recomputes it from the tree (`docs/RUN-RECORD.md` 3).
    pub unfinished: Option<WorkspaceRecord>,
    /// Hash-checked records this version could not read (a newer format or an unknown kind): reported, never skipped.
    pub unknown: Vec<u64>,
    pub torn: Vec<u64>,
    pub gaps: Vec<RangeInclusive<u64>>,
}

impl ResumeFacts {
    /// No damage found. It says nothing about a record lost whole at the end, which cannot be detected.
    pub fn is_intact(&self) -> bool {
        self.torn.is_empty() && self.gaps.is_empty() && self.unknown.is_empty()
    }

    pub fn fold(l: &Listing) -> Self {
        let mut f = ResumeFacts {
            torn: l.torn.clone(),
            gaps: l.gaps.clone(),
            ..Self::default()
        };
        for r in &l.records {
            let rec = match serde_json::from_str::<Envelope>(&r.body) {
                Ok(e) if e.v == FORMAT => e.rec,
                _ => {
                    f.unknown.push(r.seq);
                    continue;
                }
            };
            match &rec {
                WorkspaceRecord::Opened { .. } => f.opened = Some(rec),
                WorkspaceRecord::Intent { .. } => f.unfinished = Some(rec),
                WorkspaceRecord::Done { set_hash, .. } => {
                    if let Some(WorkspaceRecord::Intent { set_hash: s, .. }) = &f.unfinished
                        && s == set_hash
                    {
                        f.unfinished = None;
                    }
                    f.done.push(rec);
                }
            }
        }
        f
    }
}

fn digest(body: &str) -> String {
    Sha256::digest(body.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub(crate) fn seal(body: &str) -> Vec<u8> {
    format!("{}\n{body}", digest(body)).into_bytes()
}

/// The body of a stored value, or `None` if the hash line is missing or does not match.
pub(crate) fn open_value(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let (hash, body) = text.split_once('\n')?;
    (hash == digest(body)).then(|| body.to_string())
}

/// A [`Records`] over any [`KeyedBlobs`].
pub struct RecordLog<B> {
    blobs: B,
}

impl<B: KeyedBlobs> RecordLog<B> {
    pub fn new(blobs: B) -> Self {
        Self { blobs }
    }

    fn prefix(session: &str) -> Result<String, RecordsError> {
        let name = encode(session).ok_or(RecordsError::BadSession)?;
        Ok(format!("records/{name}/"))
    }

    fn seq_of(prefix: &str, key: &str) -> Option<u64> {
        key.strip_prefix(prefix)?.parse().ok()
    }
}

impl<B: KeyedBlobs> Records for RecordLog<B> {
    fn append(&self, session: &str, body: &str) -> Result<u64, RecordsError> {
        let prefix = Self::prefix(session)?;
        for _ in 0..1000 {
            let next = self
                .blobs
                .list(&prefix)?
                .iter()
                .filter_map(|k| Self::seq_of(&prefix, k))
                .max()
                .unwrap_or(0)
                + 1;
            let key = format!("{prefix}{next:020}");
            if self.blobs.create_if_absent(&key, seal(body))? {
                return Ok(next);
            }
        }
        Err(RecordsError::NoSequence)
    }

    fn list(&self, session: &str) -> Result<Listing, RecordsError> {
        let prefix = Self::prefix(session)?;
        let mut seqs: Vec<(u64, String)> = self
            .blobs
            .list(&prefix)?
            .into_iter()
            .filter_map(|k| Self::seq_of(&prefix, &k).map(|n| (n, k)))
            .collect();
        seqs.sort_unstable();
        let mut out = Listing::default();
        let mut expect = 1u64;
        for (seq, key) in seqs {
            if seq > expect {
                out.gaps.push(expect..=seq - 1);
            }
            expect = seq + 1;
            match self.blobs.read(&key)? {
                Some(bytes) => match open_value(&bytes) {
                    Some(body) => out.records.push(Stored { seq, body }),
                    None => out.torn.push(seq),
                },
                // Listed, then gone: nobody deletes records, so treat it as damage, not as nothing.
                None => out.torn.push(seq),
            }
        }
        Ok(out)
    }
}

/// A [`Records`] in memory, for tests and for hosts that do not need to survive a restart.
pub type MemoryRecords = RecordLog<MemoryBlobs>;

#[cfg(feature = "persistant")]
mod persist {
    use std::num::NonZeroUsize;
    use std::sync::Arc;

    use persistant::blocking::Store;
    use persistant::{Error, Need, Needs};

    use super::{KeyedBlobs, RecordsError};

    /// What a PersistAnt store must declare to hold records: read, write, create-if-absent, delete (pruning), and list with stat.
    ///
    /// Not here: `AtomicReplace`. Each record is created once and never replaced. On `fs`, open the store **without**
    /// `atomic_write_dir`, which `create_if_absent` needs to be exclusive (PersistAnt 0.16.16); the hash check covers the partly
    /// written value that allows.
    pub fn records_needs() -> Needs {
        Needs::new()
            .with(Need::Read)
            .with(Need::Write)
            .with(Need::CreateIfAbsent)
            .with(Need::Delete)
            .with(Need::Stat)
            .with(Need::List)
    }

    fn backend(e: Error) -> RecordsError {
        RecordsError::Backend(e.to_string())
    }

    /// [`KeyedBlobs`] over a PersistAnt blocking [`Store`] the host opened. This crate names no backend: the host chooses it.
    #[derive(Clone)]
    pub struct PersistBlobs(Arc<Store>);

    impl PersistBlobs {
        /// `store` must have been opened with (at least) [`records_needs`], or calls fail with `NotDeclared`.
        pub fn new(store: Store) -> Self {
            Self(Arc::new(store))
        }
    }

    impl KeyedBlobs for PersistBlobs {
        fn create_if_absent(&self, key: &str, bytes: Vec<u8>) -> Result<bool, RecordsError> {
            match self.0.create_if_absent(key, bytes) {
                Ok(()) => Ok(true),
                Err(Error::AlreadyExists(_)) => Ok(false),
                Err(e) => Err(backend(e)),
            }
        }

        fn list(&self, prefix: &str) -> Result<Vec<String>, RecordsError> {
            let limit = NonZeroUsize::new(1000).expect("non-zero");
            let mut keys = Vec::new();
            let mut after: Option<String> = None;
            loop {
                let page = self
                    .0
                    .list(prefix, after.as_deref(), limit)
                    .map_err(backend)?;
                keys.extend(page.entries.into_iter().map(|e| e.key));
                match page.next {
                    Some(n) => after = Some(n),
                    None => return Ok(keys),
                }
            }
        }

        fn read(&self, key: &str) -> Result<Option<Vec<u8>>, RecordsError> {
            match self.0.read(key) {
                Ok(b) => Ok(Some(b)),
                Err(Error::NotFound(_)) => Ok(None),
                Err(e) => Err(backend(e)),
            }
        }

        fn delete(&self, key: &str) -> Result<(), RecordsError> {
            self.0.delete(key).map_err(backend)
        }
    }
}

#[cfg(feature = "persistant")]
pub use persist::{PersistBlobs, records_needs};
