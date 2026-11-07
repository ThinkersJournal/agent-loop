// SPDX-License-Identifier: MIT OR Apache-2.0
//! A durable inbox for injected messages (`docs/LOOP-CORE.md` 4.1, `docs/SESSION.md` `Queued`).
//!
//! `RunControl::inject` keeps a message in memory, so a crash loses it. [`RunControl::inject_durable`](crate::RunControl)
//! writes the message to an [`Inbox`] first and only then returns. At each model-call boundary the driver copies pending
//! inbox messages into the run, saves the run, and **only then** acknowledges them. A crash between the save and the
//! acknowledgement redelivers the message, and the driver recognises it by id (already in the run) and does not add it twice:
//! delivery is at-least-once from the inbox and exactly-once into the run.
//!
//! The inbox is separate from the [`SessionStore`](crate::SessionStore) on purpose: the store's optimistic sequence admits one
//! writer, and a supervisor appending from another task must not race the driver's snapshots.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A message and the id the inbox gave it. Ids are unique per session and never reused, also across restarts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Queued {
    pub id: u64,
    pub text: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum InboxError {
    /// The backend failed (message only).
    Backend(String),
}

impl std::fmt::Display for InboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Backend(m) => write!(f, "inbox backend: {m}"),
        }
    }
}
impl std::error::Error for InboxError {}

/// Durable, ordered messages for a session. Methods take `&self`: a supervisor and the driver hold the same inbox.
pub trait Inbox: Send + Sync {
    /// Store a message and return its id. Durable when this returns.
    fn append(&self, session: &str, text: &str) -> Result<u64, InboxError>;
    /// Messages not yet acknowledged, oldest first.
    fn pending(&self, session: &str) -> Result<Vec<Queued>, InboxError>;
    /// Forget these ids. Unknown ids are ignored.
    fn ack(&self, session: &str, ids: &[u64]) -> Result<(), InboxError>;
}

#[derive(Default)]
struct Slot {
    next: u64,
    pending: Vec<Queued>,
}

/// In-memory inbox. Clones share one inbox, so a test can drop the driver ("crash") and open another over the same one.
#[derive(Clone, Default)]
pub struct MemoryInbox(Arc<Mutex<HashMap<String, Slot>>>);

impl MemoryInbox {
    pub fn new() -> Self {
        Self::default()
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Slot>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Inbox for MemoryInbox {
    fn append(&self, session: &str, text: &str) -> Result<u64, InboxError> {
        let mut all = self.lock();
        let b = all.entry(session.to_string()).or_default();
        b.next += 1;
        let id = b.next;
        b.pending.push(Queued {
            id,
            text: text.to_string(),
        });
        Ok(id)
    }
    fn pending(&self, session: &str) -> Result<Vec<Queued>, InboxError> {
        Ok(self
            .lock()
            .get(session)
            .map(|b| b.pending.clone())
            .unwrap_or_default())
    }
    fn ack(&self, session: &str, ids: &[u64]) -> Result<(), InboxError> {
        if let Some(b) = self.lock().get_mut(session) {
            b.pending.retain(|q| !ids.contains(&q.id));
        }
        Ok(())
    }
}
