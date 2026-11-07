// SPDX-License-Identifier: MIT OR Apache-2.0
use std::collections::BTreeMap;
use std::sync::Arc;

use rig_core::completion::ToolDefinition;

use crate::driver::{ToolHost, ToolOutcome};

/// Idempotency journal around any [`ToolHost`], keyed by the provider's tool-call id.
///
/// rig-agent is at-least-once: a crash after a tool ran but before its answer was saved re-issues the call.
/// The journal returns the recorded outcome instead of running the tool again. It covers only that case; a crash
/// before the model's reply was saved is closed by the driver's write-ahead snapshot (the reply is durable before
/// the first tool runs), so the model never re-chooses a call under a new id. See `docs/RIG-SPIKE-4.md` item 4.
///
/// Persist it (`to_json`) wherever the session is persisted, and `from_json` it on resume.
pub struct Journal<H> {
    inner: H,
    done: BTreeMap<String, ToolOutcome>,
    hits: usize,
}

impl<H: ToolHost> Journal<H> {
    pub fn new(inner: H) -> Self {
        Self {
            inner,
            done: BTreeMap::new(),
            hits: 0,
        }
    }

    /// Restore a journal saved by [`Journal::to_json`].
    pub fn from_json(json: &str, inner: H) -> Result<Self, serde_json::Error> {
        Ok(Self {
            inner,
            done: serde_json::from_str(json)?,
            hits: 0,
        })
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(&self.done).expect("a map of strings serialises")
    }

    /// Calls answered from the journal instead of the tool.
    pub fn hits(&self) -> usize {
        self.hits
    }

    pub fn inner(&self) -> &H {
        &self.inner
    }
}

impl<H: ToolHost> ToolHost for Journal<H> {
    fn definitions(&self) -> Vec<Arc<ToolDefinition>> {
        self.inner.definitions()
    }

    async fn call(&mut self, id: &str, name: &str, args: &serde_json::Value) -> ToolOutcome {
        if let Some(out) = self.done.get(id) {
            self.hits += 1;
            return out.clone();
        }
        let out = self.inner.call(id, name, args).await;
        self.done.insert(id.to_string(), out.clone());
        out
    }
}
