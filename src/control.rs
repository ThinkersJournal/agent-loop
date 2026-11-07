// SPDX-License-Identifier: MIT OR Apache-2.0
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct State {
    queue: VecDeque<String>,
    stop: bool,
}

/// The verbs a supervisor has over a running [`crate::Driver`]: `inject` and `stop_soft`.
///
/// Cheap to clone and `Send`, so it can be held by another task or by a tool. Both verbs take effect at the next
/// model-call boundary: the current step (a streaming turn, or a whole tool batch) always finishes first, so every open
/// tool call is answered and the saved run is consistent.
///
/// **Durability, stated plainly:** an injected message is kept in the saved run (as a host record) from the boundary
/// that delivers it, or from the boundary at which a soft stop drains the queue. A message still in this in-memory queue
/// when the process dies is lost; making `inject` write through to the session store is a later slice.
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

    /// Ask the driver to stop at the next model-call boundary, after saving the run.
    pub fn stop_soft(&self) {
        self.lock().stop = true;
    }

    /// Whether a soft stop has been requested and not yet taken.
    pub fn stop_requested(&self) -> bool {
        self.lock().stop
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
