// SPDX-License-Identifier: MIT OR Apache-2.0
//! Loop hooks (`docs/RIG-SPIKE-4.md` item 6, `docs/HARNESS-ADAPTER.md` 5.4, `docs/TOOLS.md` D|E).
//!
//! rig-agent's hooks belong to its engine, which this crate does not use, so ours are our own, at the same call-outs: before a
//! model call (stop), before a tool runs (allow, patch its arguments, or deny), after a tool runs (rewrite its outcome), and at a
//! stop (the host's wrap-up). A `Policy::check` is a [`Hook`] whose `before_tool` returns [`ToolDecision::Deny`]; the loop only
//! guarantees the check happens before every execution.
//!
//! Hooks are synchronous and infallible by design: a hook that cannot decide must return `Allow`/`Continue` or deny, never
//! panic the run. Every method has a do-nothing default.
//!
//! What a hook changes is what the *tool* and the *model* see for this call; the run's recorded tool-call arguments are the
//! model's own and are not rewritten.

/// What a tool call produced. A tool failure is a result the model sees, not a loop error.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolOutcome {
    pub text: String,
    pub is_error: bool,
}

impl ToolOutcome {
    pub fn ok(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: false,
        }
    }
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
        }
    }
}

/// A tool call about to run (or just run).
#[derive(Clone, Copy, Debug)]
pub struct ToolCall<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub args: &'a serde_json::Value,
}

/// What to do with a tool call before it runs.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolDecision {
    /// Run it as the model asked.
    Allow,
    /// Run it with these arguments instead.
    Patch(serde_json::Value),
    /// Do not run it. The model gets an error result carrying this reason, so it can react.
    Deny(String),
}

/// Whether the loop may make the next model call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelDecision {
    Continue,
    /// Stop at this boundary. The run is saved first, exactly as for a soft stop, and can be resumed.
    Stop(String),
}

/// Why a drive stopped (given to the wrap-up).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// A supervisor asked, through `RunControl::stop_soft`.
    Soft,
    /// A hook asked.
    Hook(String),
}

/// Host-supplied call-outs. All methods default to doing nothing.
pub trait Hook {
    /// Before each model call, after the run was saved. `turn` is the model-call number.
    fn before_model(&mut self, _turn: u64) -> ModelDecision {
        ModelDecision::Continue
    }
    /// Before each tool call runs.
    fn before_tool(&mut self, _call: ToolCall<'_>) -> ToolDecision {
        ToolDecision::Allow
    }
    /// After a tool call ran. `Some` replaces the outcome the model sees; `None` keeps it. Not called for a denied call.
    fn after_tool(&mut self, _call: ToolCall<'_>, _outcome: &ToolOutcome) -> Option<ToolOutcome> {
        None
    }
    /// A soft stop or a hook stop was taken and the run is saved. This is where the host writes its wrap-up (a HANDOFF).
    fn on_stop(&mut self, _why: &StopReason) {}
}
