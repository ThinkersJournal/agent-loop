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

use crate::context::{ContextPolicy, Summarise};
use crate::control::RunControl;
use crate::events::{Event, EventSink};
use crate::hook::{Hook, ModelDecision, StopReason, ToolCall, ToolDecision};
use crate::inbox::Inbox;
use crate::run::{Run, RunConfig};
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

/// The cached summary of the turns a cut dropped: it covers the transcript up to (not including) `covered`.
#[derive(Clone, Default)]
struct SummaryState {
    covered: usize,
    text: Option<String>,
}

/// Runs one session over our three traits.
pub struct Driver<M, T, S> {
    pub model: M,
    pub tools: T,
    pub store: S,
    spec: RunSpec,
    session: String,
    seq: u64,
    control: RunControl,
    inbox: Option<Arc<dyn Inbox>>,
    hook: Option<Box<dyn Hook>>,
    context: Option<Box<dyn ContextPolicy>>,
    summary: Option<(Summarise, SummaryState)>,
    sink: Option<Box<dyn EventSink>>,
    resumed: bool,
}

impl<M: Model, T: ToolHost, S: SessionStore> Driver<M, T, S> {
    pub fn new(
        session: impl Into<String>,
        model: M,
        tools: T,
        store: S,
        config: RunConfig,
    ) -> Self {
        Self {
            model,
            tools,
            store,
            spec: config.to_spec(),
            session: session.into(),
            seq: 0,
            control: RunControl::new(),
            inbox: None,
            hook: None,
            context: None,
            summary: None,
            sink: None,
            resumed: false,
        }
    }

    /// Install a [`ContextPolicy`] that shapes what each model call sends. The run's own history is never shortened; a
    /// policy answer that breaks tool-call pairing is a drive error (the model is not called).
    pub fn set_context_policy(&mut self, policy: impl ContextPolicy + 'static) {
        self.context = Some(Box::new(policy));
        self.summary = None;
    }

    /// Replace the messages a cut leaves out with a model-written summary, instead of dropping them without trace.
    ///
    /// The cut is the same one [`KeepRecent`](crate::KeepRecent) makes (`head` kept, `tail` kept, never inside a tool call and
    /// its result). The summary is a separate model call, made through this driver's [`Model`] with no tools, and is cached: it
    /// is refreshed (the old summary plus the newly dropped messages) only when `every` or more messages have newly fallen out.
    /// Between refreshes the not-yet-summarised messages are sent as they are. The run's own history is never shortened, and the
    /// summary is not saved with it: a resumed drive writes it again, once. A failed summary call is a drive error, the request
    /// is not sent without it. Replaces any [`ContextPolicy`] (and a policy replaces this).
    pub fn set_summariser(&mut self, cfg: Summarise) {
        self.summary = Some((cfg, SummaryState::default()));
        self.context = None;
    }

    /// The summarising counterpart of [`Self::shape_context`]; emits `Compacted` and the summary call's `Usage`.
    #[allow(clippy::type_complexity)]
    async fn summarise_context(
        &mut self,
        history: Vec<Arc<Message>>,
        prompt: Arc<Message>,
        patch: Option<RequestPatch>,
    ) -> Result<(Vec<Arc<Message>>, Arc<Message>, Option<RequestPatch>), DriverError> {
        let (cfg, mut state) = match &self.summary {
            Some((c, st)) => (*c, st.clone()),
            None => return Ok((history, prompt, patch)),
        };
        let mut all = history;
        all.push(prompt.clone());
        let len = all.len();
        let start = rig_core::conversation::pairing::safe_start(&all, len.saturating_sub(cfg.tail));
        if len <= cfg.head + cfg.tail || start <= cfg.head {
            all.pop();
            return Ok((all, prompt, patch));
        }
        if state.text.is_none() || start.saturating_sub(state.covered) >= cfg.every.max(1) {
            let from = state.covered.max(cfg.head);
            let text = self
                .summarise(state.text.as_deref(), &all[from..start])
                .await?;
            state = SummaryState {
                covered: start,
                text: Some(text),
            };
            self.summary = Some((cfg, state.clone()));
        }
        let text = state.text.unwrap_or_default();
        let from = state.covered.max(cfg.head);
        let mut shaped: Vec<Arc<Message>> = all[..cfg.head].to_vec();
        shaped.push(Arc::new(Message::user(format!(
            "Summary of the earlier turns of this conversation (they are no longer shown):\n{text}"
        ))));
        shaped.extend_from_slice(&all[from..]);
        match shaped.last() {
            Some(last) if Arc::ptr_eq(last, &prompt) || **last == *prompt => {}
            _ => {
                return Err(DriverError::Run(
                    "summariser dropped the turn's prompt (it must stay last)".into(),
                ));
            }
        }
        rig_core::conversation::pairing::validate(&shaped).map_err(|e| rerr("summariser", &e))?;
        let (dropped, kept) = (from - cfg.head, shaped.len());
        shaped.pop();
        let patch = Some(RequestPatch::new().history(shaped.clone()));
        self.emit(Event::Compacted { dropped, kept });
        Ok((shaped, prompt, patch))
    }

    /// One model call, no tools and no history, that turns `prior` (an earlier summary) and `msgs` into a new summary.
    async fn summarise(
        &mut self,
        prior: Option<&str>,
        msgs: &[Arc<Message>],
    ) -> Result<String, DriverError> {
        let excerpt = serde_json::to_string(msgs).map_err(|e| rerr("summariser", &e))?;
        let mut ask = String::from(
            "Summarise this conversation excerpt for your own later use. Keep the task, decisions, facts, names, results and \
             anything unfinished; leave out the rest. Reply with the summary only.\n\n",
        );
        if let Some(p) = prior {
            ask.push_str(&format!("Earlier summary:\n{p}\n\n"));
        }
        ask.push_str(&format!("Excerpt (JSON messages):\n{excerpt}"));
        let stream = self
            .model
            .stream(CompletionRequest::new(Message::user(ask)))
            .await
            .map_err(|e| rerr("summary stream open", &e))?;
        let response = stream
            .finish()
            .await
            .map_err(|e| rerr("summary finish", &e))?;
        self.emit(Event::Usage {
            input: response.usage.input_tokens,
            output: response.usage.output_tokens,
        });
        let text = response.text();
        if text.trim().is_empty() {
            return Err(DriverError::Run(
                "summariser: the model returned no text".into(),
            ));
        }
        Ok(text)
    }

    /// Apply the context policy, if any, to the transcript for this call (prompt last). Emits `Compacted` when it shortens it.
    #[allow(clippy::type_complexity)]
    fn shape_context(
        &mut self,
        history: Vec<Arc<Message>>,
        prompt: Arc<Message>,
        patch: Option<RequestPatch>,
    ) -> Result<(Vec<Arc<Message>>, Arc<Message>, Option<RequestPatch>), DriverError> {
        let Some(policy) = &mut self.context else {
            return Ok((history, prompt, patch));
        };
        let mut all = history;
        all.push(prompt.clone());
        let Some(mut shaped) = policy.shape(&all) else {
            all.pop();
            return Ok((all, prompt, patch));
        };
        match shaped.last() {
            Some(last) if Arc::ptr_eq(last, &prompt) || **last == *prompt => {}
            _ => {
                return Err(DriverError::Run(
                    "context policy dropped the turn's prompt (it must stay last)".into(),
                ));
            }
        }
        rig_core::conversation::pairing::validate(&shaped)
            .map_err(|e| rerr("context policy", &e))?;
        let (dropped, kept) = (all.len() - shaped.len(), shaped.len());
        shaped.pop();
        let patch = Some(RequestPatch::new().history(shaped.clone()));
        self.emit(Event::Compacted { dropped, kept });
        Ok((shaped, prompt, patch))
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
    pub fn open(&mut self, prompt: &str, max_turns: usize) -> Result<Run, DriverError> {
        match self.store.load(&self.session)? {
            Some((seq, json)) => {
                self.seq = seq;
                self.resumed = true;
                serde_json::from_str(&json)
                    .map(Run)
                    .map_err(|e| DriverError::Snapshot(e.to_string()))
            }
            None => Ok(Run(AgentRun::new(prompt).max_turns(max_turns))),
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
    pub async fn drive(&mut self, run: &mut Run) -> Result<End, DriverError> {
        self.emit(Event::Started {
            resumed: self.resumed,
        });
        let out = self.drive_steps(&mut run.0).await;
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
                    let (history, prompt, patch) = if self.summary.is_some() {
                        self.summarise_context(history, prompt, patch).await?
                    } else {
                        self.shape_context(history, prompt, patch)?
                    };
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
