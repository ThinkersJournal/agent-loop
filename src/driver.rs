// SPDX-License-Identifier: MIT OR Apache-2.0
use std::future::Future;
use std::sync::Arc;

use futures::StreamExt;
use rig_agent::run::{
    AgentRun, AgentRunStep, PendingToolCall, RunSpec, StreamedTurnAssembler, StreamedTurnEvent,
    prepare_request,
};
use rig_core::completion::{CompletionRequest, ProviderCapabilities, ToolDefinition};
use rig_core::error::ProviderError;
use rig_core::streaming::CompletionStream;
use rig_core::tool::{ToolErrorKind, ToolExecutionError, ToolOutput, ToolResult};

use crate::store::{SessionStore, StoreError};

/// The B|C seam: open a streamed completion for a request.
pub trait Model {
    fn capabilities(&self) -> ProviderCapabilities;
    fn stream(
        &self,
        request: CompletionRequest,
    ) -> impl Future<Output = Result<CompletionStream, ProviderError>>;
}

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

/// The C|D seam: the tools offered to the model, and running one call.
pub trait ToolHost {
    fn definitions(&self) -> Vec<Arc<ToolDefinition>>;
    /// Run one call. `id` is the provider's tool-call id (the key an idempotency journal will use).
    fn call(
        &mut self,
        id: &str,
        name: &str,
        args: &serde_json::Value,
    ) -> impl Future<Output = ToolOutcome>;
}

#[derive(Debug)]
pub enum DriverError {
    Store(StoreError),
    /// The stored snapshot could not be read as a run (corrupt, or another `RUN_FORMAT`): stop, do not guess.
    Snapshot(String),
    /// A rig-agent or model error, as text.
    Run(String),
}

impl std::fmt::Display for DriverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(e) => write!(f, "{e}"),
            Self::Snapshot(m) => write!(f, "unreadable snapshot: {m}"),
            Self::Run(m) => write!(f, "{m}"),
        }
    }
}
impl std::error::Error for DriverError {}
impl From<StoreError> for DriverError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

/// How a drive ended.
#[derive(Debug, PartialEq, Eq)]
pub enum End {
    Done(String),
}

fn rerr(what: &str, e: &dyn std::fmt::Display) -> DriverError {
    DriverError::Run(format!("{what}: {e}"))
}

/// Runs one session over our three traits.
pub struct Driver<M, T, S> {
    pub model: M,
    pub tools: T,
    pub store: S,
    pub spec: RunSpec,
    session: String,
    seq: u64,
}

impl<M: Model, T: ToolHost, S: SessionStore> Driver<M, T, S> {
    pub fn new(session: impl Into<String>, model: M, tools: T, store: S, spec: RunSpec) -> Self {
        Self {
            model,
            tools,
            store,
            spec,
            session: session.into(),
            seq: 0,
        }
    }

    /// Resume the stored run, or start one from `prompt`. An unreadable snapshot is an error, never a fresh start.
    pub fn open(&mut self, prompt: &str, max_turns: usize) -> Result<AgentRun, DriverError> {
        match self.store.load(&self.session)? {
            Some((seq, json)) => {
                self.seq = seq;
                serde_json::from_str(&json).map_err(|e| DriverError::Snapshot(e.to_string()))
            }
            None => Ok(AgentRun::new(prompt).max_turns(max_turns)),
        }
    }

    /// Persist the run. Called at every step boundary; before any tool executes (write-ahead).
    fn snapshot(&mut self, run: &AgentRun) -> Result<(), DriverError> {
        let json = serde_json::to_string(run).map_err(|e| DriverError::Snapshot(e.to_string()))?;
        self.seq = self.store.save(&self.session, self.seq, &json)?;
        Ok(())
    }

    /// Drive `run` to `Done`, snapshotting at each boundary.
    pub async fn drive(&mut self, run: &mut AgentRun) -> Result<End, DriverError> {
        loop {
            match run.next_step().map_err(|e| rerr("next_step", &e))? {
                AgentRunStep::Done(resp) => {
                    self.snapshot(run)?;
                    return Ok(End::Done(resp.output().clone()));
                }
                AgentRunStep::CallModel {
                    prompt,
                    history,
                    turn,
                } => {
                    self.snapshot(run)?;
                    let caps = self.model.capabilities();
                    let prepared = prepare_request(
                        &self.spec,
                        &caps,
                        &history,
                        self.tools.definitions(),
                        run.output_tool_name(),
                        None,
                    )
                    .map_err(|e| rerr("prepare", &e))?;
                    run.advertise_tools(turn, prepared.tools.clone());
                    let policy = prepared.policy.clone();
                    let request = prepared.apply(CompletionRequest::new(prompt));
                    let mut stream = self
                        .model
                        .stream(request)
                        .await
                        .map_err(|e| rerr("stream open", &e))?;
                    let mut assembler = StreamedTurnAssembler::new(policy);
                    while let Some(item) = stream.next().await {
                        let item = item.map_err(|e| rerr("stream", &e))?;
                        if let StreamedTurnEvent::InvalidToolCall(_) =
                            assembler.ingest(&item).map_err(|e| rerr("ingest", &e))?
                        {
                            return Err(DriverError::Run(
                                "invalid tool call (not handled in slice 1)".into(),
                            ));
                        }
                    }
                    let response = stream.finish().await.map_err(|e| rerr("finish", &e))?;
                    run.record_streamed_completion_call(
                        response.usage,
                        response.identity(),
                        response.finish_reason(),
                        response.raw.clone(),
                    )
                    .map_err(|e| rerr("record", &e))?;
                    let turn_result = assembler.finish(&response);
                    run.streamed_turn(turn_result)
                        .map_err(|e| rerr("streamed_turn", &e))?;
                }
                AgentRunStep::CallTools { calls } => {
                    // Write-ahead: the model's reply is durable before the first side effect.
                    self.snapshot(run)?;
                    for call in calls {
                        match call {
                            PendingToolCall::Execute(c) => {
                                let out = self
                                    .tools
                                    .call(&c.id().to_string(), c.name().as_str(), &c.arguments())
                                    .await;
                                let result = if out.is_error {
                                    ToolResult::failed(ToolExecutionError::new(
                                        ToolErrorKind::Other,
                                        out.text,
                                    ))
                                } else {
                                    ToolResult::success(ToolOutput::text(out.text))
                                };
                                run.answer(c.answer(result))
                                    .map_err(|e| rerr("answer", &e))?;
                                self.snapshot(run)?;
                            }
                            _ => {
                                return Err(DriverError::Run(
                                    "malformed tool call (not handled in slice 1)".into(),
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
}
