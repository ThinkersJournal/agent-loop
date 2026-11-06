// SPDX-License-Identifier: MIT OR Apache-2.0
use std::collections::HashMap;

/// Why a store call failed.
#[derive(Debug, PartialEq, Eq)]
pub enum StoreError {
    /// Another writer got there first: the stored sequence is not the one the caller read.
    Conflict { expected: u64, actual: u64 },
    /// The backend failed (message only).
    Backend(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict { expected, actual } => {
                write!(f, "conflict: expected sequence {expected}, stored {actual}")
            }
            Self::Backend(m) => write!(f, "store backend: {m}"),
        }
    }
}
impl std::error::Error for StoreError {}

/// The C|F seam: one resume snapshot per session, written with the sequence the writer read
/// (optimistic: of two writers that read the same sequence, the second is refused).
pub trait SessionStore {
    /// The latest snapshot and its sequence, or `None` for a new session.
    fn load(&self, session: &str) -> Result<Option<(u64, String)>, StoreError>;
    /// Store `snapshot` as sequence `expected + 1`; refused with `Conflict` if the stored sequence is not `expected`
    /// (a new session has sequence 0). Returns the new sequence.
    fn save(&mut self, session: &str, expected: u64, snapshot: &str) -> Result<u64, StoreError>;
}

/// In-memory store, for tests and the hello-world.
#[derive(Default)]
pub struct MemoryStore {
    sessions: HashMap<String, (u64, String)>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SessionStore for MemoryStore {
    fn load(&self, session: &str) -> Result<Option<(u64, String)>, StoreError> {
        Ok(self.sessions.get(session).cloned())
    }
    fn save(&mut self, session: &str, expected: u64, snapshot: &str) -> Result<u64, StoreError> {
        let actual = self.sessions.get(session).map_or(0, |(s, _)| *s);
        if actual != expected {
            return Err(StoreError::Conflict { expected, actual });
        }
        let next = actual + 1;
        self.sessions
            .insert(session.to_string(), (next, snapshot.to_string()));
        Ok(next)
    }
}
