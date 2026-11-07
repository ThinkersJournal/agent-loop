// SPDX-License-Identifier: MIT OR Apache-2.0
use std::future::Future;
use std::sync::Arc;

use futures::StreamExt;
use rig_agent::run::{
    AgentRun, AgentRunStep, PendingToolCall, RequestPatch, RunSpec, StreamedTurnAssembler,
    StreamedTurnEvent, prepare_request,
};
use rig_core::completion::{CompletionRequest, ProviderCapabilities, ToolDefinition};
use rig_core::conversation::{Aside, Author};
use rig_core::error::ProviderError;
use rig_core::message::Message;
use rig_core::streaming::CompletionStream;
use rig_core::tool::{ToolErrorKind, ToolExecutionError, ToolOutput, ToolResult};

use crate::control::RunControl;
use crate::events::{Event, EventSink};
use crate::hook::{Hook, ModelDecision, StopReason, ToolCall, ToolDecision};
use crate::inbox::Inbox;
use crate::store::{SessionStore, StoreError};

/// Kind of the host record that carries an injected message inside the saved run.
const INJECT_KIND: &str = "host.inject";

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
    /// A soft stop was taken at a model-call boundary. The run is saved and can be resumed with `open` + `drive`.
    Stopped,
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
    control: RunControl,
    inbox: Option<Arc<dyn Inbox>>,
    hook: Option<Box<dyn Hook>>,
    sink: Option<Box<dyn EventSink>>,
    resumed: bool,
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
            control: RunControl::new(),
            inbox: None,
            hook: None,
            sink: None,
            resumed: false,
        }
    }

    /// Install the host's [`Hook`]: stop before a model call, allow/patch/deny a tool call, rewrite its outcome, wrap up on stop.
    pub fn set_hook(&mut self, hook: impl Hook + 'static) {
        self.hook = Some(Box::new(hook));
    }

    fn wrap_up(&mut self, why: StopReason) {
        if let Some(h) = &mut self.hook {
            h.on_stop(&why);
        }
    }

    /// Send this driver's [`Event`]s to `sink`. Without one, events are dropped. A sink never affects the run.
    pub fn set_sink(&mut self, sink: impl EventSink + 'static) {
        self.sink = Some(Box::new(sink));
    }

    fn emit(&mut self, event: Event) {
        if let Some(s) = &mut self.sink {
            s.emit(event);
        }
    }

    /// Deliver durable injections (`RunControl::inject_durable`) from `inbox`. At each model-call boundary pending messages
    /// are copied into the run, the run is saved, and only then are they acknowledged; a message already in the run is
    /// recognised by id and not added again.
    pub fn with_inbox(mut self, inbox: Arc<dyn Inbox>) -> Self {
        self.control.attach(inbox.clone(), self.session.clone());
        self.inbox = Some(inbox);
        self
    }

    /// The supervisor's handle: `inject` and `stop_soft`. Clones share one queue.
    pub fn control(&self) -> RunControl {
        self.control.clone()
    }

    /// Resume the stored run, or start one from `prompt`. An unreadable snapshot is an error, never a fresh start.
    pub fn open(&mut self, prompt: &str, max_turns: usize) -> Result<AgentRun, DriverError> {
        match self.store.load(&self.session)? {
            Some((seq, json)) => {
                self.seq = seq;
                self.resumed = true;
                serde_json::from_str(&json).map_err(|e| DriverError::Snapshot(e.to_string()))
            }
            None => Ok(AgentRun::new(prompt).max_turns(max_turns)),
        }
    }

    /// Copy pending inbox messages that the run does not hold yet into it. Returns every pending id (to acknowledge after the
    /// save). A failing inbox fails the drive: carrying on would silently skip messages a person was told were delivered.
    fn pull_inbox(&mut self, run: &mut AgentRun, sent: usize) -> Result<Vec<u64>, DriverError> {
        let Some(inbox) = &self.inbox else {
            return Ok(vec![]);
        };
        let pending = inbox
            .pending(&self.session)
            .map_err(|e| rerr("inbox", &e))?;
        let known: std::collections::HashSet<u64> = run
            .entries_of(INJECT_KIND)
            .filter_map(|a| a.value["id"].as_u64())
            .collect();
        let mut ids = Vec::new();
        for q in pending {
            ids.push(q.id);
            if !known.contains(&q.id) {
                run.append_entry(Aside::new(
                    INJECT_KIND,
                    Author::default(),
                    serde_json::json!({ "at": sent, "text": q.text, "id": q.id }),
                ));
            }
        }
        Ok(ids)
    }

    /// Persist the run. Called at every step boundary; before any tool executes (write-ahead).
    fn snapshot(&mut self, run: &AgentRun) -> Result<(), DriverError> {
        let json = serde_json::to_string(run).map_err(|e| DriverError::Snapshot(e.to_string()))?;
        self.seq = self.store.save(&self.session, self.seq, &json)?;
        Ok(())
    }

    /// Drive `run` to `Done`, snapshotting at each boundary. Emits `Started` first, then one event per step, and `Error` if it fails.
    pub async fn drive(&mut self, run: &mut AgentRun) -> Result<End, DriverError> {
        self.emit(Event::Started {
            resumed: self.resumed,
        });
        let out = self.drive_steps(run).await;
        match &out {
            Ok(End::Done(_)) => self.emit(Event::Finished),
            Ok(End::Stopped) => self.emit(Event::Stopped),
            Err(e) => self.emit(Event::Error {
                message: e.to_string(),
            }),
        }
        out
    }

    async fn drive_steps(&mut self, run: &mut AgentRun) -> Result<End, DriverError> {
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
                    // Boundary: queued messages become host records in the run, so the snapshot below carries them.
                    // `at` is the position in the transcript this turn sends (history + prompt): later turns only append.
                    let sent = history.len() + 1;
                    for text in self.control.drain() {
                        run.append_entry(Aside::new(
                            INJECT_KIND,
                            Author::default(),
                            serde_json::json!({ "at": sent, "text": text }),
                        ));
                    }
                    let delivered = self.pull_inbox(run, sent)?;
                    self.snapshot(run)?;
                    // Acknowledge only after the save. A failed ack is safe: the message comes back and is recognised by id.
                    if let Some(inbox) = &self.inbox {
                        let _ = inbox.ack(&self.session, &delivered);
                    }
                    if self.control.take_stop() {
                        self.wrap_up(StopReason::Soft);
                        return Ok(End::Stopped);
                    }
                    if let Some(h) = &mut self.hook
                        && let ModelDecision::Stop(why) = h.before_model(turn as u64)
                    {
                        self.wrap_up(StopReason::Hook(why));
                        return Ok(End::Stopped);
                    }
                    let (history, prompt, patch) = splice_injected(run, history, prompt);
                    self.emit(Event::ModelCall { turn: turn as u64 });
                    let caps = self.model.capabilities();
                    let prepared = prepare_request(
                        &self.spec,
                        &caps,
                        &history,
                        self.tools.definitions(),
                        run.output_tool_name(),
                        patch.as_ref(),
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
                    self.emit(Event::Usage {
                        input: response.usage.input_tokens,
                        output: response.usage.output_tokens,
                    });
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
                                let id = c.id().to_string();
                                let name = c.name().as_str().to_string();
                                let args = c.arguments();
                                self.emit(Event::ToolStarted {
                                    id: id.clone(),
                                    name: name.clone(),
                                });
                                let decision = match &mut self.hook {
                                    Some(h) => h.before_tool(ToolCall {
                                        id: &id,
                                        name: &name,
                                        args: &args,
                                    }),
                                    None => ToolDecision::Allow,
                                };
                                let out = match decision {
                                    ToolDecision::Deny(why) => {
                                        ToolOutcome::error(format!("denied: {why}"))
                                    }
                                    d => {
                                        let run_args = match d {
                                            ToolDecision::Patch(v) => v,
                                            _ => args,
                                        };
                                        let out = self.tools.call(&id, &name, &run_args).await;
                                        let call = ToolCall {
                                            id: &id,
                                            name: &name,
                                            args: &run_args,
                                        };
                                        match &mut self.hook {
                                            Some(h) => h.after_tool(call, &out).unwrap_or(out),
                                            None => out,
                                        }
                                    }
                                };
                                self.emit(Event::ToolFinished {
                                    id,
                                    is_error: out.is_error,
                                });
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

/// Rebuild the request transcript with every saved `host.inject` record as a user message.
///
/// rig-agent has no way to append a message to a started run (RIG-SPIKE-4 item 2), so an injection lives in the run as a
/// host record and is spliced into each request here. The run's own history is never changed, so `full_history()` does
/// not contain injected text; the saved run does, and a resumed run splices it again.
fn splice_injected(
    run: &AgentRun,
    history: Vec<Arc<Message>>,
    prompt: Arc<Message>,
) -> (Vec<Arc<Message>>, Arc<Message>, Option<RequestPatch>) {
    let mut asides: Vec<&Aside> = run.entries_of(INJECT_KIND).collect();
    if asides.is_empty() {
        return (history, prompt, None);
    }
    asides.sort_by_key(|a| a.value["at"].as_u64().unwrap_or(0));
    let mut all = history;
    all.push(prompt);
    for (n, a) in asides.iter().enumerate() {
        let at = (a.value["at"].as_u64().unwrap_or(0) as usize + n).min(all.len());
        let text = a.value["text"].as_str().unwrap_or_default();
        all.insert(at, Arc::new(Message::user(text)));
    }
    let prompt = all.pop().expect("at least the prompt");
    let patch = RequestPatch::new().history(all.clone());
    (all, prompt, Some(patch))
}
