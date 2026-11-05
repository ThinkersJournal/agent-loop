//! A hand-rolled sans-I/O driver over `AgentRun`, owning the loop, the model call and the tools.
//! Throwaway spike code (items 1-5 of the PM's spike 4). Only rig-core types and `rig_agent::run` are used.
use std::sync::Arc;

use futures::StreamExt;
use rig_agent::run::{
    AgentRun, AgentRunStep, PendingToolCall, RequestPatch, RunSpec,
    StreamedTurnAssembler, StreamedTurnEvent, prepare_request,
};
use rig_core::completion::{CompletionRequest, Message, ProviderCapabilities, ToolDefinition};
use rig_core::error::ProviderError;
use rig_core::streaming::CompletionStream;
use rig_core::streaming::Item;
use rig_core::test_utils::MockCompletionModel;
use rig_core::tool::{ToolOutput, ToolResult};

/// Our `ToolHost` seam: the driver owns tool execution, rig's tool server is not used.
pub trait ToolHost {
    fn defs(&self) -> Vec<Arc<ToolDefinition>>;
    /// Execute one call. `id` is the provider tool-call id.
    fn exec(&mut self, id: &str, name: &str, args: &serde_json::Value) -> String;
}

/// Where the driver "crashes". The caller persists the run (serde) at every step boundary it sees.
#[derive(Clone, Debug, Default)]
pub struct Kill {
    /// Stop reading the stream after this many stream items (a cut mid-stream).
    pub after_stream_items: Option<usize>,
    /// Die after this many tools ran AND were answered in a batch.
    pub after_answers: Option<usize>,
    /// Die after this many tools ran but before the last one's answer reaches the run.
    pub after_exec_before_answer: Option<usize>,
}

#[derive(Debug)]
pub enum End {
    Done(String),
    Killed(String),
    Err(String),
}

/// Hook fired before each model call, as `on_completion_call` would: may return a `RequestPatch`.
pub type PatchFn<'a> = &'a mut dyn FnMut(&AgentRun, usize) -> Option<RequestPatch>;

pub type OpenFn<'a> = Box<dyn Fn(CompletionRequest) -> Result<CompletionStream, ProviderError> + 'a>;

pub struct Driver<'a> {
    /// The model seam: opens a stream for a request (any rig-core Model, mock or real).
    pub open: OpenFn<'a>,
    pub caps: ProviderCapabilities,
    pub spec: RunSpec,
    pub host: &'a mut dyn ToolHost,
    /// Every serialized snapshot taken at a step boundary: (label, json).
    pub snapshots: Vec<(String, String)>,
    pub patch: Option<PatchFn<'a>>,
    /// Stream items seen, for display.
    pub trace: Vec<String>,
    /// Item 2: after this many stream items abort the stream and re-issue the CallModel with the message
    /// patched in (stock rig: via RequestPatch.history + the message as the request prompt).
    pub inject: Option<(usize, Message)>,
    reissue: Option<Message>,
    /// Host-side replacement of the whole request transcript (last message = prompt); item 2c / item 5.
    pub splice: Option<Box<dyn Fn(&AgentRun) -> Option<Vec<Arc<Message>>> + 'a>>,
    /// Item 2 (vendored patch): call `AgentRun::inject_message` instead of patching the request.
    pub use_run_inject: bool,
}

impl<'a> Driver<'a> {
    pub fn new(model: MockCompletionModel, spec: RunSpec, host: &'a mut dyn ToolHost) -> Self {
        let caps = model.capabilities().completion;
        Self::with_open(Box::new(move |r| model.stream(r)), caps, spec, host)
    }

    pub fn with_open(open: OpenFn<'a>, caps: ProviderCapabilities, spec: RunSpec, host: &'a mut dyn ToolHost) -> Self {
        Self { open, caps, spec, host, snapshots: Vec::new(), patch: None, trace: Vec::new(), inject: None, reissue: None, splice: None, use_run_inject: false }
    }

    fn snap(&mut self, label: &str, run: &AgentRun) {
        self.snapshots.push((label.to_string(), serde_json::to_string(run).expect("ser")));
    }

    /// Drive `run` to Done or to the kill point. The run is left in its current state either way.
    pub async fn drive(&mut self, run: &mut AgentRun, kill: &Kill) -> End {
        'outer: loop {
            let step = match run.next_step() {
                Ok(s) => s,
                Err(e) => return End::Err(format!("next_step: {e}")),
            };
            match step {
                AgentRunStep::Done(resp) => return End::Done(resp.output().clone()),
                AgentRunStep::CallModel { prompt, history, turn } => {
                    self.snap(&format!("CallModel#{turn}"), run);
                    let mut prompt = prompt;
                    let mut patch = match self.patch.as_mut() {
                        Some(p) => p(run, turn),
                        None => None,
                    };
                    if let Some(f) = &self.splice
                        && let Some(mut v) = f(run)
                        && let Some(last) = v.pop()
                    {
                        patch = Some(RequestPatch::new().history(v));
                        prompt = last;
                    }
                    if let Some(m) = self.reissue.take() {
                        // Stock rig: the injected message becomes the request prompt; the old prompt joins the
                        // history through the patch. None of this reaches the run.
                        let mut h = history.clone();
                        h.push(prompt.clone());
                        patch = Some(RequestPatch::new().history(h));
                        prompt = Arc::new(m);
                    }
                    let tools = self.host.defs();
                    let caps = self.caps;
                    let prepared = match prepare_request(
                        &self.spec, &caps, &history, tools.clone(), run.output_tool_name(), patch.as_ref(),
                    ) {
                        Ok(p) => p,
                        Err(e) => return End::Err(format!("prepare: {e}")),
                    };
                    run.advertise_tools(turn, prepared.tools.clone());
                    let policy = prepared.policy.clone();
                    let request: CompletionRequest = prepared.apply(CompletionRequest::new(prompt));
                    self.trace.push(format!("sent request with {} chat_history messages", request.chat_history.len()));
                    let mut stream = match (self.open)(request) {
                        Ok(s) => s,
                        Err(e) => return End::Err(format!("stream open: {e}")),
                    };
                    let mut assembler = StreamedTurnAssembler::new(policy);
                    let mut seen = 0usize;
                    while let Some(item) = stream.next().await {
                        let item: Item<_> = match item {
                            Ok(i) => i,
                            Err(e) => {
                                // A cut by the transport: the partial reply is on the stream.
                                let partial = stream.partial();
                                self.trace.push(format!("stream error {e}; partial choice parts={}", partial.choice.len()));
                                return End::Err(format!("stream: {e}"));
                            }
                        };
                        seen += 1;
                        self.trace.push(format!("item {seen}"));
                        match assembler.ingest(&item) {
                            Ok(StreamedTurnEvent::InvalidToolCall(_)) => {
                                return End::Err("invalid tool call (not exercised here)".into());
                            }
                            Ok(_) => {}
                            Err(e) => return End::Err(format!("ingest: {e}")),
                        }
                        if let Some((n, _)) = &self.inject
                            && *n == seen
                        {
                            let partial = stream.partial();
                            self.trace.push(format!("aborted at item {seen}; partial parts={}", partial.choice.len()));
                            drop(stream);
                            let (_, m) = self.inject.take().unwrap();
                            if self.use_run_inject {
                                inject_hook(run, m);
                            } else {
                                self.reissue = Some(m);
                            }
                            continue 'outer;
                        }
                        if kill.after_stream_items == Some(seen) {
                            let partial = stream.partial();
                            self.trace.push(format!("cut at item {seen}; partial parts={} text={:?}", partial.choice.len(), partial.text()));
                            // Drop the stream: "stop and cut mid-stream". The run stays AwaitingModel.
                            drop(stream);
                            return End::Killed(format!("cut after {seen} stream items"));
                        }
                    }
                    let response = match stream.finish().await {
                        Ok(r) => r,
                        Err(e) => return End::Err(format!("finish: {e}")),
                    };
                    if let Err(e) = run.record_streamed_completion_call(
                        response.usage, response.identity(), response.finish_reason(), response.raw.clone(),
                    ) {
                        return End::Err(format!("record: {e}"));
                    }
                    let st = assembler.finish(&response);
                    if let Err(e) = run.streamed_turn(st) {
                        return End::Err(format!("streamed_turn: {e}"));
                    }
                }
                AgentRunStep::CallTools { calls } => {
                    // Persist BEFORE executing: the model reply is now durable.
                    self.snap("CallTools", run);
                    let mut answered = 0usize;
                    let total = calls.len();
                    for (i, call) in calls.into_iter().enumerate() {
                        match call {
                            PendingToolCall::Execute(c) => {
                                let out = self.host.exec(&c.id().to_string(), c.name().as_str(), &c.arguments());
                                if kill.after_exec_before_answer == Some(i + 1) {
                                    return End::Killed(format!("died after exec #{} before its answer", i + 1));
                                }
                                if let Err(e) = run.answer(c.answer(ToolResult::success(ToolOutput::text(out)))) {
                                    return End::Err(format!("answer: {e}"));
                                }
                                answered += 1;
                                self.snap(&format!("answered {answered}/{total}"), run);
                                if kill.after_answers == Some(answered) && answered < total {
                                    return End::Killed(format!("died after {answered} answers"));
                                }
                            }
                            PendingToolCall::Malformed(_) => return End::Err("malformed call".into()),
                            _ => return End::Err("unknown pending call kind".into()),
                        }
                    }
                }
                _ => return End::Err("unknown step".into()),
            }
        }
    }
}

/// Item 2 hook point: with the vendored patch this calls `AgentRun::inject_message`; stock builds do not have it.
#[cfg(feature = "inject")]
fn inject_hook(run: &mut AgentRun, m: Message) {
    run.inject_message(m).expect("inject_message");
}
#[cfg(not(feature = "inject"))]
fn inject_hook(_run: &mut AgentRun, _m: Message) {
    panic!("built without the vendored inject_message patch");
}
