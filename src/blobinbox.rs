// SPDX-License-Identifier: MIT OR Apache-2.0
//! An [`Inbox`] over any [`KeyedBlobs`] (`docs/PERSIST-MIGRATION.md`, slice 2): the way injected messages are kept durably through
//! PersistAnt without a private disk backend. It follows [`FileInbox`](crate::FileInbox): one value per message
//! (`inbox/<session>/<id>.msg`), an acknowledgement marker (`<id>.ack`), `prune`, and ids that are never reused.
//!
//! Every value is `<sha256 hex>\n<body>`, because on some backends (PersistAnt `fs` writes in place) a value can be visible before
//! it is complete. A message that fails its check is **skipped** by `pending` and listed by [`BlobInbox::torn`], so nothing is
//! dropped silently. A damaged message was never acknowledged to its sender (`append` had not returned), so skipping it loses
//! nothing the sender was told was kept. A damaged **marker** counts as *not* acknowledged: the message is delivered again, which
//! the driver already tolerates (it recognises a message by id).
//!
//! **Durability is the backend's** (`Process` on PersistAnt `fs`); `FileInbox` stays for power-loss care. See the design note.

use std::collections::HashSet;

use crate::file::encode;
use crate::inbox::{Inbox, InboxError, Queued};
use crate::records::{KeyedBlobs, RecordsError, open_value, seal};

fn backend(e: RecordsError) -> InboxError {
    InboxError::Backend(e.to_string())
}

pub struct BlobInbox<B> {
    blobs: B,
}

impl<B: KeyedBlobs> BlobInbox<B> {
    pub fn new(blobs: B) -> Self {
        Self { blobs }
    }

    fn prefix(session: &str) -> Result<String, InboxError> {
        let name = encode(session).ok_or_else(|| InboxError::Backend("empty session id".into()))?;
        Ok(format!("inbox/{name}/"))
    }

    fn key(prefix: &str, id: u64, ext: &str) -> String {
        format!("{prefix}{id:020}.{ext}")
    }

    /// Ids present under `prefix` with the given extension, ascending.
    fn ids(&self, prefix: &str, ext: &str) -> Result<Vec<u64>, InboxError> {
        let suffix = format!(".{ext}");
        let mut out: Vec<u64> = self
            .blobs
            .list(prefix)
            .map_err(backend)?
            .iter()
            .filter_map(|k| {
                k.strip_prefix(prefix)?
                    .strip_suffix(suffix.as_str())?
                    .parse()
                    .ok()
            })
            .collect();
        out.sort_unstable();
        Ok(out)
    }

    fn intact(&self, key: &str) -> Result<bool, InboxError> {
        Ok(self
            .blobs
            .read(key)
            .map_err(backend)?
            .is_some_and(|b| open_value(&b).is_some()))
    }

    /// Ids of acknowledgement markers that pass their hash check.
    fn acked(&self, prefix: &str) -> Result<HashSet<u64>, InboxError> {
        let mut out = HashSet::new();
        for id in self.ids(prefix, "ack")? {
            if self.intact(&Self::key(prefix, id, "ack"))? {
                out.insert(id);
            }
        }
        Ok(out)
    }

    /// Ids of messages that failed their hash check, so a host can report them. Nothing is deleted.
    pub fn torn(&self, session: &str) -> Result<Vec<u64>, InboxError> {
        let prefix = Self::prefix(session)?;
        let mut out = Vec::new();
        for id in self.ids(&prefix, "msg")? {
            if !self.intact(&Self::key(&prefix, id, "msg"))? {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// Delete this session's acknowledged messages and their markers; return how many messages went. Unacknowledged messages are
    /// never touched. The highest-numbered message is kept even when acknowledged: `append` reads it to pick the next id, so
    /// deleting it would let an id be reused. The message goes first: a marker with no message is harmless, a message with no
    /// marker would be delivered again. The backend must support delete.
    pub fn prune(&self, session: &str) -> Result<usize, InboxError> {
        let prefix = Self::prefix(session)?;
        let msgs = self.ids(&prefix, "msg")?;
        let acked = self.acked(&prefix)?;
        let newest = msgs.last().copied();
        let mut removed = 0;
        for id in &msgs {
            if Some(*id) == newest || !acked.contains(id) {
                continue;
            }
            self.blobs
                .delete(&Self::key(&prefix, *id, "msg"))
                .map_err(backend)?;
            self.blobs
                .delete(&Self::key(&prefix, *id, "ack"))
                .map_err(backend)?;
            removed += 1;
        }
        // Markers (of any state) whose message is already gone: an interrupted prune, or an ack for a message pruned since.
        let present: HashSet<u64> = msgs.into_iter().collect();
        for id in self.ids(&prefix, "ack")? {
            if !present.contains(&id) {
                let _ = self.blobs.delete(&Self::key(&prefix, id, "ack"));
            }
        }
        Ok(removed)
    }
}

impl<B: KeyedBlobs> Inbox for BlobInbox<B> {
    fn append(&self, session: &str, text: &str) -> Result<u64, InboxError> {
        let prefix = Self::prefix(session)?;
        // Take the next id (damaged messages count, so an id is never reused); if another appender took it first, try the next.
        for _ in 0..1000 {
            let id = self.ids(&prefix, "msg")?.last().copied().unwrap_or(0) + 1;
            if self
                .blobs
                .create_if_absent(&Self::key(&prefix, id, "msg"), seal(text))
                .map_err(backend)?
            {
                return Ok(id);
            }
        }
        Err(InboxError::Backend(
            "could not take an id after 1000 tries".into(),
        ))
    }

    fn pending(&self, session: &str) -> Result<Vec<Queued>, InboxError> {
        let prefix = Self::prefix(session)?;
        let acked = self.acked(&prefix)?;
        let mut out = Vec::new();
        for id in self.ids(&prefix, "msg")? {
            if acked.contains(&id) {
                continue;
            }
            // A message that is damaged, or was deleted since the listing, is skipped (see `torn`).
            if let Some(text) = self
                .blobs
                .read(&Self::key(&prefix, id, "msg"))
                .map_err(backend)?
                .and_then(|b| open_value(&b))
            {
                out.push(Queued { id, text });
            }
        }
        Ok(out)
    }

    fn ack(&self, session: &str, ids: &[u64]) -> Result<(), InboxError> {
        let prefix = Self::prefix(session)?;
        let known: HashSet<u64> = self.ids(&prefix, "msg")?.into_iter().collect();
        for id in ids.iter().filter(|i| known.contains(i)) {
            let key = Self::key(&prefix, *id, "ack");
            if self
                .blobs
                .create_if_absent(&key, seal(""))
                .map_err(backend)?
            {
                continue;
            }
            // A marker is already there. If it is damaged (a crash mid-write) it acknowledges nothing: replace it.
            if !self.intact(&key)? {
                self.blobs.delete(&key).map_err(backend)?;
                self.blobs
                    .create_if_absent(&key, seal(""))
                    .map_err(backend)?;
            }
        }
        Ok(())
    }
}
