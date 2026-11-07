// SPDX-License-Identifier: MIT OR Apache-2.0
//! The run handle and its configuration, as our own types (`docs/WRAP.md`, stage 1).
//!
//! [`Run`] and [`RunConfig`] wrap rig-agent's `AgentRun` and `RunSpec` so neither appears in this crate's public API. A consumer
//! creates a [`RunConfig`], gets a [`Run`] from `Driver::open`, and hands it back to `Driver::drive`; it never names a rig type.
//!
//! **A saved run is the engine's.** What `SessionStore` holds is rig-agent's serialised run (its own format version), so a session
//! saved by this driver cannot be resumed by a driver built on another engine, whatever types wrap it. See `docs/WRAP.md`.

use rig_agent::run::{AgentRun, RunSpec};

/// How a run behaves. Only the settings a consumer has needed so far are here; the rest of rig's `RunSpec` (tool choice,
/// structured output, provider tools, static context, retry limits) stays at rig's defaults until someone asks for it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunConfig {
    preamble: Option<String>,
    max_turns: Option<usize>,
    max_tokens: Option<u64>,
    temperature: Option<f64>,
    additional_params: Option<serde_json::Value>,
}

impl RunConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// The system prompt sent with every model call.
    pub fn preamble(mut self, text: impl Into<String>) -> Self {
        self.preamble = Some(text.into());
        self
    }

    /// The model-call budget for the run (without it, rig's default of one call applies).
    pub fn max_turns(mut self, n: usize) -> Self {
        self.max_turns = Some(n);
        self
    }

    /// The most tokens the model may generate per call.
    pub fn max_tokens(mut self, n: u64) -> Self {
        self.max_tokens = Some(n);
        self
    }

    pub fn temperature(mut self, t: f64) -> Self {
        self.temperature = Some(t);
        self
    }

    /// Provider-specific request parameters, merged into every request.
    pub fn additional_params(mut self, params: serde_json::Value) -> Self {
        self.additional_params = Some(params);
        self
    }

    pub(crate) fn to_spec(&self) -> RunSpec {
        let mut s = RunSpec::new();
        s.preamble = self.preamble.clone();
        s.max_turns = self.max_turns;
        s.max_tokens = self.max_tokens;
        s.temperature = self.temperature;
        s.additional_params = self.additional_params.clone();
        s
    }
}

/// One session's run: opaque, created by `Driver::open` and advanced by `Driver::drive`.
pub struct Run(pub(crate) AgentRun);

impl Run {
    /// The number of messages in the run's own history (never shortened by a [`ContextPolicy`](crate::ContextPolicy)).
    pub fn history_len(&self) -> usize {
        self.0.full_history().len()
    }

    /// The run's own history as JSON text, for inspection and comparison.
    pub fn history_json(&self) -> serde_json::Result<String> {
        serde_json::to_string(&self.0.full_history())
    }
}
