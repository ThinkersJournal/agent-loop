// SPDX-License-Identifier: MIT OR Apache-2.0
//! Disk-backed [`SessionStore`] and [`Inbox`]: what makes "survives a restart" true for files, not only for a dropped driver.
//!
//! Both are plain directories of small files, written with no lock file and no stale-lock failure mode:
//!
//! - Every file is written in full to a temporary name, synced, and then **published with `hard_link`**, which fails if the final
//!   name exists. So a reader never sees a half-written file, and of two writers racing for the same name exactly one wins.
//! - [`FileStore`] keeps one file per snapshot, named by its sequence (`<session>.<seq>.snap`); the latest is the highest
//!   sequence. A second writer that read the same sequence loses the publish and gets `Conflict`, which is the store's
//!   optimistic-sequence rule enforced by the filesystem. The newest two snapshots are kept; older ones are removed.
//! - [`FileInbox`] keeps one file per message (`<id>.msg`) and an empty `<id>.ack` marker once acknowledged. Messages are never
//!   deleted by `append`, `pending` or `ack`, so an id is never reused, also after a restart. [`FileInbox::prune`] removes
//!   acknowledged messages on request, and always keeps the highest id so that rule still holds.
//!
//! Session ids are encoded into file names (see `encode`: lowercase letters, digits, `-` and `_` are kept, every other byte
//! becomes `%XX`, and a fixed prefix keeps it clear of Windows device names), so an id cannot name a path outside the directory
//! and two ids cannot collide, also on a case-insensitive filesystem. Needs a filesystem that supports hard links; if it does not, the first save or append
//! fails with a backend error rather than writing unsafely.

use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::inbox::{Inbox, InboxError, Queued};
use crate::store::{SessionStore, StoreError};

static TMP: AtomicU64 = AtomicU64::new(0);

/// `session` as a file-name-safe string. Empty is not a session.
///
/// Only lowercase ASCII letters, digits, `-` and `_` are kept; every other byte, **uppercase letters included**, becomes `%XX`,
/// so two ids never share a name on a case-insensitive filesystem. The result starts with `s_`, so it is never a Windows device
/// name (`con`, `nul`, `aux`, ...), which Windows reads as a device whatever the extension.
fn encode(session: &str) -> Option<String> {
    if session.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(session.len() + 2);
    out.push_str("s_");
    for b in session.bytes() {
        if b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    Some(out)
}

/// A unique temporary name next to `dir`'s files. Not matched by any reader's pattern.
fn tmp_in(dir: &Path) -> PathBuf {
    dir.join(format!(
        "tmp.{}.{}",
        std::process::id(),
        TMP.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Write `bytes` to `final_path` completely or not at all, failing with `AlreadyExists` if it exists.
fn publish(dir: &Path, final_path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = tmp_in(dir);
    let result = (|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::hard_link(&tmp, final_path)
    })();
    let _ = fs::remove_file(&tmp);
    result
}

/// Disk-backed [`SessionStore`] in one directory.
pub struct FileStore {
    dir: PathBuf,
}

impl FileStore {
    /// Use `dir`, creating it if needed.
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let dir = dir.into();
        fs::create_dir_all(&dir)
            .map_err(|e| StoreError::Backend(format!("{}: {e}", dir.display())))?;
        Ok(Self { dir })
    }

    fn snap(&self, name: &str, seq: u64) -> PathBuf {
        self.dir.join(format!("{name}.{seq:020}.snap"))
    }

    /// Sequence numbers present for `name`, ascending.
    fn seqs(&self, name: &str) -> Result<Vec<u64>, StoreError> {
        let prefix = format!("{name}.");
        let mut out = Vec::new();
        let entries = fs::read_dir(&self.dir).map_err(|e| StoreError::Backend(e.to_string()))?;
        for entry in entries {
            let entry = entry.map_err(|e| StoreError::Backend(e.to_string()))?;
            let file = entry.file_name();
            let Some(file) = file.to_str() else { continue };
            if let Some(mid) = file
                .strip_prefix(&prefix)
                .and_then(|r| r.strip_suffix(".snap"))
                && let Ok(n) = mid.parse::<u64>()
            {
                out.push(n);
            }
        }
        out.sort_unstable();
        Ok(out)
    }
}

fn name_of(session: &str) -> Result<String, StoreError> {
    encode(session).ok_or_else(|| StoreError::Backend("empty session id".into()))
}

impl SessionStore for FileStore {
    fn load(&self, session: &str) -> Result<Option<(u64, String)>, StoreError> {
        let name = name_of(session)?;
        let Some(&seq) = self.seqs(&name)?.last() else {
            return Ok(None);
        };
        let text = fs::read_to_string(self.snap(&name, seq))
            .map_err(|e| StoreError::Backend(format!("read snapshot {seq}: {e}")))?;
        Ok(Some((seq, text)))
    }

    fn save(&mut self, session: &str, expected: u64, snapshot: &str) -> Result<u64, StoreError> {
        let name = name_of(session)?;
        let actual = self.seqs(&name)?.last().copied().unwrap_or(0);
        if actual != expected {
            return Err(StoreError::Conflict { expected, actual });
        }
        let next = expected + 1;
        match publish(&self.dir, &self.snap(&name, next), snapshot.as_bytes()) {
            Ok(()) => {
                // Keep the newest two. A failed removal only leaves an old file behind.
                for old in self.seqs(&name)?.into_iter().filter(|s| *s + 1 < next) {
                    let _ = fs::remove_file(self.snap(&name, old));
                }
                Ok(next)
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                let actual = self.seqs(&name)?.last().copied().unwrap_or(next);
                Err(StoreError::Conflict { expected, actual })
            }
            Err(e) => Err(StoreError::Backend(format!("save snapshot {next}: {e}"))),
        }
    }
}

/// Disk-backed [`Inbox`]: one subdirectory per session under `dir`.
pub struct FileInbox {
    dir: PathBuf,
}

impl FileInbox {
    /// Use `dir`, creating it if needed.
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self, InboxError> {
        let dir = dir.into();
        fs::create_dir_all(&dir)
            .map_err(|e| InboxError::Backend(format!("{}: {e}", dir.display())))?;
        Ok(Self { dir })
    }

    fn session_dir(&self, session: &str) -> Result<PathBuf, InboxError> {
        let name = encode(session).ok_or_else(|| InboxError::Backend("empty session id".into()))?;
        Ok(self.dir.join(name))
    }

    /// Delete this session's acknowledged messages and their markers; return how many messages went. Unacknowledged messages
    /// are never touched. The highest-numbered message is kept even when acknowledged: it is what `append` reads to pick the
    /// next id, so deleting it would let an id be reused. A crash part-way leaves at worst an orphan marker, which is ignored.
    pub fn prune(&self, session: &str) -> Result<usize, InboxError> {
        let sdir = self.session_dir(session)?;
        let msgs = Self::ids(&sdir, ".msg")?;
        let acked: std::collections::HashSet<u64> = Self::ids(&sdir, ".ack")?.into_iter().collect();
        let newest = msgs.last().copied();
        let mut removed = 0;
        for id in msgs {
            if Some(id) == newest || !acked.contains(&id) {
                continue;
            }
            // The message first: a marker with no message is harmless, a message with no marker would be delivered again.
            fs::remove_file(sdir.join(format!("{id:020}.msg"))).map_err(io)?;
            fs::remove_file(sdir.join(format!("{id:020}.ack"))).map_err(io)?;
            removed += 1;
        }
        // Markers whose message is already gone (an interrupted prune, or an ack for a message pruned since).
        for id in acked {
            if !sdir.join(format!("{id:020}.msg")).exists() {
                let _ = fs::remove_file(sdir.join(format!("{id:020}.ack")));
            }
        }
        Ok(removed)
    }

    /// Message ids present in `sdir` with the given extension, ascending.
    fn ids(sdir: &Path, ext: &str) -> Result<Vec<u64>, InboxError> {
        let entries = match fs::read_dir(sdir) {
            Ok(e) => e,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(InboxError::Backend(e.to_string())),
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| InboxError::Backend(e.to_string()))?;
            let file = entry.file_name();
            if let Some(file) = file.to_str()
                && let Some(stem) = file.strip_suffix(ext)
                && let Ok(n) = stem.parse::<u64>()
            {
                out.push(n);
            }
        }
        out.sort_unstable();
        Ok(out)
    }
}

fn io(e: std::io::Error) -> InboxError {
    InboxError::Backend(e.to_string())
}

impl Inbox for FileInbox {
    fn append(&self, session: &str, text: &str) -> Result<u64, InboxError> {
        let sdir = self.session_dir(session)?;
        fs::create_dir_all(&sdir).map_err(io)?;
        // Take the next id; if another appender took it first, publish fails and we try the one after.
        for _ in 0..1000 {
            let id = Self::ids(&sdir, ".msg")?.last().copied().unwrap_or(0) + 1;
            match publish(&sdir, &sdir.join(format!("{id:020}.msg")), text.as_bytes()) {
                Ok(()) => return Ok(id),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(io(e)),
            }
        }
        Err(InboxError::Backend(
            "could not take an id after 1000 tries".into(),
        ))
    }

    fn pending(&self, session: &str) -> Result<Vec<Queued>, InboxError> {
        let sdir = self.session_dir(session)?;
        let acked: std::collections::HashSet<u64> = Self::ids(&sdir, ".ack")?.into_iter().collect();
        let mut out = Vec::new();
        for id in Self::ids(&sdir, ".msg")? {
            if !acked.contains(&id) {
                let text = fs::read_to_string(sdir.join(format!("{id:020}.msg"))).map_err(io)?;
                out.push(Queued { id, text });
            }
        }
        Ok(out)
    }

    fn ack(&self, session: &str, ids: &[u64]) -> Result<(), InboxError> {
        let sdir = self.session_dir(session)?;
        let known: std::collections::HashSet<u64> = Self::ids(&sdir, ".msg")?.into_iter().collect();
        for id in ids.iter().filter(|i| known.contains(i)) {
            fs::File::create(sdir.join(format!("{id:020}.ack"))).map_err(io)?;
        }
        Ok(())
    }
}
