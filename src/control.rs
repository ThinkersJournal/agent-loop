// SPDX-License-Identifier: MIT OR Apache-2.0
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::inbox::{Inbox, InboxError};

#[derive(Default)]
struct State {
    queue: VecDeque<String>,
    stop: bool,
    inbox: Option<(Arc<dyn Inbox>, String)>,
}

/// The verbs a supervisor has over a running [`crate::Driver`]: `inject` and `stop_soft`.
///
/// Cheap to clone and `Send`, so it can be held by another task or by a tool. Both verbs take effect at the next
/// model-call boundary: the current step (a streaming turn, or a whole tool batch) always finishes first, so every open
/// tool call is answered and the saved run is consistent.
///
/// **Durability, stated plainly:** [`inject`](Self::inject) keeps the message in memory until a model-call boundary copies it
/// into the saved run, so a crash before that boundary loses it. [`inject_durable`](Self::inject_durable) writes it to the
/// driver's [`Inbox`] first (see `Driver::with_inbox`), so it survives; the driver delivers it at the next boundary.
#[derive(Clone, Default)]
pub struct RunControl {
    state: Arc<Mutex<State>>,
}

impl RunControl {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a user message for the model; delivered at the next model-call boundary.
    pub fn inject(&self, text: impl Into<String>) {
        self.lock().queue.push_back(text.into());
    }

    /// Like `inject`, but durable: the message is in the inbox when this returns, and the driver delivers it at its next
    /// model-call boundary, also after a restart. Returns the inbox's id for it. Fails if the driver has no inbox.
    pub fn inject_durable(&self, text: &str) -> Result<u64, InboxError> {
        let attached = self.lock().inbox.clone();
        match attached {
            Some((inbox, session)) => inbox.append(&session, text),
            None => Err(InboxError::Backend(
                "no inbox attached to this driver".into(),
            )),
        }
    }

    /// Ask the driver to stop at the next model-call boundary, after saving the run.
    pub fn stop_soft(&self) {
        self.lock().stop = true;
    }

    /// Whether a soft stop has been requested and not yet taken.
    pub fn stop_requested(&self) -> bool {
        self.lock().stop
    }

    pub(crate) fn attach(&self, inbox: Arc<dyn Inbox>, session: String) {
        self.lock().inbox = Some((inbox, session));
    }

    pub(crate) fn drain(&self) -> Vec<String> {
        self.lock().queue.drain(..).collect()
    }

    pub(crate) fn take_stop(&self) -> bool {
        std::mem::take(&mut self.lock().stop)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // A poisoned lock only means another holder panicked mid-push; the queue is still a valid queue.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}
