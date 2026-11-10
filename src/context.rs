// SPDX-License-Identifier: MIT OR Apache-2.0
//! Context policy: which messages a model call actually sends (`docs/LOOP-CORE.md` 4.5, `docs/RIG-SPIKE-4.md` item 5).
//!
//! The run's own history is authoritative and is **never** shortened. Before each model call the driver hands the full
//! transcript (history, injected messages, and the turn's prompt last) to a [`ContextPolicy`]; whatever it returns is only the
//! request for this call. It is derived again on every turn, so it is idempotent and nothing trimmed is ever saved or lost.
//!
//! The driver checks the policy's answer with rig's pairing rules before sending. An answer that breaks them (an orphan tool
//! result, a call left unanswered, or a missing final message) is a drive error and the model is not called: sending a
//! history a provider would reject, or silently sending everything, are both worse than stopping.
//!
//! Not here: summarising the dropped turns. That needs a model call, so it is a driver step of its own, not a policy: see
//! [`Summarise`] and `Driver::set_summariser`.

use std::sync::Arc;

use rig_core::conversation::pairing::safe_start;
use rig_core::message::Message;

/// Decides what a model call sends. `transcript` is everything the run holds for this call, **the prompt being the last
/// message**; the answer must keep that last message.
pub trait ContextPolicy {
    /// `Some(messages)` to send these instead of the whole transcript, `None` to send it all.
    fn shape(&mut self, transcript: &[Arc<Message>]) -> Option<Vec<Arc<Message>>>;
}

/// Keep the first `head` messages (the task) and the last `tail` messages, cutting on a turn boundary.
///
/// The cut never lands between a tool call and its result: the start of the kept tail is moved forward by rig's `safe_start`.
/// `head` should end on a turn boundary too (1, the opening user message, always does); if it does not, the driver's pairing
/// check refuses the result.
#[derive(Clone, Copy, Debug)]
pub struct KeepRecent {
    pub head: usize,
    pub tail: usize,
}

impl ContextPolicy for KeepRecent {
    fn shape(&mut self, transcript: &[Arc<Message>]) -> Option<Vec<Arc<Message>>> {
        let len = transcript.len();
        if len <= self.head + self.tail {
            return None;
        }
        let start = safe_start(transcript, len - self.tail);
        if start <= self.head {
            return None;
        }
        let mut out: Vec<Arc<Message>> = transcript[..self.head].to_vec();
        out.extend_from_slice(&transcript[start..]);
        Some(out)
    }
}

/// Settings for `Driver::set_summariser`: the same cut as [`KeepRecent`], but what falls out is summarised by the model rather
/// than dropped.
#[derive(Clone, Copy, Debug)]
pub struct Summarise {
    /// Messages kept from the start (the task). 1, the opening user message, is the usual value.
    pub head: usize,
    /// Messages kept from the end, moved forward to a turn boundary.
    pub tail: usize,
    /// Refresh the summary only once at least this many messages have newly fallen out of the kept tail (at least 1). A larger
    /// value means fewer summary calls and more unsummarised messages sent between them.
    pub every: usize,
}
