// SPDX-License-Identifier: MIT OR Apache-2.0
//! The loop's outward channel (`docs/LOOP-CORE.md` 4.4, `docs/HARNESS-ADAPTER.md` 5.2 and 5.4): typed events, and a fold that
//! answers the adapter's `activity` and `cost` verbs from them.
//!
//! The loop never talks to Synapse or any transport. It hands each [`Event`] to an [`EventSink`] and the host does the rest.
//! Events are for watching: nothing in the run's saved state depends on a sink, and a sink cannot fail the run.
//!
//! Unknown is a value: a token count the provider did not report stays `None` in [`Status`], it is never turned into 0.

use std::sync::{Arc, Mutex};

/// One thing that happened in a drive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A drive began. `resumed` is true when it continues a stored run.
    Started {
        resumed: bool,
    },
    /// A model call is about to be made for this turn.
    ModelCall {
        turn: u64,
    },
    /// A model call finished. A counter the provider did not report is `None`.
    Usage {
        input: Option<u64>,
        output: Option<u64>,
    },
    ToolStarted {
        id: String,
        name: String,
    },
    ToolFinished {
        id: String,
        is_error: bool,
    },
    /// A context policy sent fewer messages than the run holds for this call: `dropped` were left out, `kept` were sent
    /// (prompt included). The run's own history is unchanged.
    Compacted {
        dropped: usize,
        kept: usize,
    },
    /// A soft stop was taken; the run is saved and resumable.
    Stopped,
    /// The run reached its final answer.
    Finished,
    /// The drive ended with an error (the text is the error's).
    Error {
        message: String,
    },
}

/// Receives events from the driver. Must not block for long: it runs on the loop's own task.
pub trait EventSink {
    fn emit(&mut self, event: Event);
}

/// A cloneable in-memory sink: keep one clone, give the other to the driver, read `events()` any time.
#[derive(Clone, Default)]
pub struct EventLog(Arc<Mutex<Vec<Event>>>);

impl EventLog {
    pub fn new() -> Self {
        Self::default()
    }
    /// Everything emitted so far, in order.
    pub fn events(&self) -> Vec<Event> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl EventSink for EventLog {
    fn emit(&mut self, event: Event) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).push(event);
    }
}

/// The adapter's `activity` verb, from events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Activity {
    /// Nothing has run yet, or the last drive stopped or finished.
    Idle,
    Busy(Step),
    /// The last drive ended in an error. Not idle: a person should look.
    Failed,
}

/// What a busy loop is doing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Model {
        turn: u64,
    },
    /// Tool calls started and not yet finished: `open` is how many.
    Tools {
        open: usize,
    },
}

/// Activity and cumulative cost, folded from the event stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub activity: Activity,
    /// `None` until a call reports the counter; after that the sum of reported values.
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
    pub model_calls: u64,
    open_tools: usize,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            activity: Activity::Idle,
            tokens_in: None,
            tokens_out: None,
            model_calls: 0,
            open_tools: 0,
        }
    }
}

fn add(total: Option<u64>, n: Option<u64>) -> Option<u64> {
    match (total, n) {
        (t, None) => t,
        (None, Some(n)) => Some(n),
        (Some(t), Some(n)) => Some(t.saturating_add(n)),
    }
}

impl Status {
    pub fn apply(&mut self, e: &Event) {
        match e {
            Event::Started { .. } => {
                self.open_tools = 0;
                self.activity = Activity::Idle;
            }
            Event::ModelCall { turn } => {
                self.model_calls += 1;
                self.activity = Activity::Busy(Step::Model { turn: *turn });
            }
            Event::Usage { input, output } => {
                self.tokens_in = add(self.tokens_in, *input);
                self.tokens_out = add(self.tokens_out, *output);
            }
            Event::ToolStarted { .. } => {
                self.open_tools += 1;
                self.activity = Activity::Busy(Step::Tools {
                    open: self.open_tools,
                });
            }
            Event::ToolFinished { .. } => {
                self.open_tools = self.open_tools.saturating_sub(1);
                self.activity = Activity::Busy(Step::Tools {
                    open: self.open_tools,
                });
            }
            Event::Compacted { .. } => {}
            Event::Stopped | Event::Finished => self.activity = Activity::Idle,
            Event::Error { .. } => self.activity = Activity::Failed,
        }
    }

    pub fn from_events<'a>(events: impl IntoIterator<Item = &'a Event>) -> Self {
        let mut s = Self::default();
        for e in events {
            s.apply(e);
        }
        s
    }
}
