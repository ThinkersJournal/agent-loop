//! Items 5 and 6: rig's own AgentHook on the high-level agent path (the streaming runner).
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use rig_agent::AgentBuilder;
use rig_agent::agent::{
    AgentHook, CompletionCallAction, CompletionCallEvent, DispatchAction, DispatchEvent, HookContext,
    ModelSelection, ModelSelectionAction, ModelTurnAction, ModelTurnFinished, ObservationAction,
    OutcomeAction, OutcomeEvent, RunStart, RunStartAction, TextDelta, ToolCallDelta,
};
use rig_agent::run::{PromptError, RequestPatch};
use rig_agent::test_utils::MockAddTool;
use rig_core::completion::{Document, Message, Usage};
use rig_core::effect::EffectKind;
use rig_core::error::{ErrorKind, ErrorReport};
use rig_core::test_utils::{MockCompletionModel, MockStreamEvent as E};

pub type Log = Arc<Mutex<Vec<String>>>;

/// Where the probe hook stops / what it does. Everything else only logs.
#[derive(Clone, Default)]
pub struct Cfg {
    pub stop_at: Option<&'static str>,
    pub patch_tool_args: bool,
    pub deny_tool: Option<ErrorKind>,
    pub rewrite_tool_output: bool,
}

#[derive(Clone)]
pub struct Probe {
    pub log: Log,
    pub cfg: Cfg,
}
impl Probe {
    fn note(&self, s: impl Into<String>) {
        self.log.lock().unwrap().push(s.into());
    }
    fn stop(&self, point: &'static str) -> bool {
        self.cfg.stop_at == Some(point)
    }
}

impl AgentHook for Probe {
    async fn on_run_start(&self, _c: &HookContext, _e: RunStart<'_>) -> RunStartAction {
        self.note("run_start");
        if self.stop("run_start") { RunStartAction::stop("stop@run_start") } else { RunStartAction::Continue }
    }
    fn on_model_select(&self, _c: &HookContext, _e: ModelSelection<'_>) -> ModelSelectionAction {
        self.note("model_select");
        if self.stop("model_select") { ModelSelectionAction::stop("stop@model_select") } else { ModelSelectionAction::Continue }
    }
    async fn on_completion_call(&self, _c: &HookContext, e: CompletionCallEvent<'_>) -> CompletionCallAction {
        self.note(format!("completion_call turn={}", e.turn));
        if self.stop("completion_call") { CompletionCallAction::stop("stop@completion_call") } else { CompletionCallAction::Continue }
    }
    async fn on_text_delta(&self, _c: &HookContext, e: TextDelta<'_>) -> ObservationAction {
        self.note(format!("text_delta {:?}", e.delta));
        if self.stop("text_delta") { ObservationAction::stop("stop@text_delta") } else { ObservationAction::Continue }
    }
    async fn on_tool_call_delta(&self, _c: &HookContext, e: ToolCallDelta<'_>) -> ObservationAction {
        self.note(format!("tool_call_delta tool={}", e.tool_name));
        if self.stop("tool_call_delta") { ObservationAction::stop("stop@tool_call_delta") } else { ObservationAction::Continue }
    }
    async fn on_model_turn_finished(&self, _c: &HookContext, e: ModelTurnFinished<'_>) -> ModelTurnAction {
        self.note(format!("model_turn_finished turn={} usage_total={:?} in={:?} out={:?}", e.turn, e.usage.total_tokens, e.usage.input_tokens, e.usage.output_tokens));
        if self.stop("model_turn_finished") { ModelTurnAction::stop("stop@model_turn_finished") } else { ModelTurnAction::Continue }
    }
    async fn on_dispatch(&self, _c: &HookContext, e: DispatchEvent<'_>) -> DispatchAction {
        match e.kind {
            EffectKind::ToolCall { name, args } => {
                self.note(format!("dispatch ToolCall {name} {args}"));
                if self.stop("dispatch_tool") {
                    return DispatchAction::deny(ErrorReport::new(ErrorKind::Cancelled, "stop@dispatch_tool"));
                }
                if let Some(kind) = &self.cfg.deny_tool {
                    return DispatchAction::deny(ErrorReport::new(kind.clone(), "denied by policy"));
                }
                if self.cfg.patch_tool_args {
                    return DispatchAction::patch(EffectKind::ToolCall { name: name.clone(), args: "{\"x\":10,\"y\":20}".into() });
                }
            }
            other => self.note(format!("dispatch {}", other.name())),
        }
        DispatchAction::Proceed
    }
    async fn on_outcome(&self, _c: &HookContext, e: OutcomeEvent<'_>) -> OutcomeAction {
        self.note(format!("outcome {} ok={}", e.kind.name(), e.outcome.is_ok()));
        if matches!(e.kind, EffectKind::ToolCall { .. }) {
            if self.stop("outcome_tool") {
                return OutcomeAction::stop("stop@outcome_tool");
            }
            if self.cfg.rewrite_tool_output {
                return OutcomeAction::rewrite_tool_output(&e, rig_core::tool::ToolOutput::text("REDACTED"));
            }
        }
        OutcomeAction::Proceed
    }
}

fn usage(i: u64, o: u64) -> Usage {
    Usage::default().input_tokens(i).output_tokens(o).total_tokens(i + o)
}

fn one_tool_model() -> MockCompletionModel {
    MockCompletionModel::from_stream_turns([
        vec![
            E::text("thinking "),
            E::tool_call_name_delta("t1", "add"),
            E::tool_call_arguments_delta("t1", "{\"x\":1,\"y\":2}"),
            E::tool_call_end("t1"),
            E::final_response(usage(100, 20)),
        ],
        vec![E::text("the answer"), E::final_response(usage(130, 5))],
    ])
}

struct Outcome {
    err: Option<String>,
    cancelled_history: Option<usize>,
    final_text: Option<String>,
    usage: Option<String>,
    history_json: Option<String>,
}

async fn run(model: MockCompletionModel, probe: Probe) -> Outcome {
    use rig_agent::agent::MultiTurnStreamItem;
    let agent = AgentBuilder::new(model).tool(MockAddTool).build();
    let mut s = agent.prompt("add 1 and 2").max_turns(5).add_hook(probe).stream();
    let mut out = Outcome { err: None, cancelled_history: None, final_text: None, usage: None, history_json: None };
    while let Some(i) = s.next().await {
        match i {
            Ok(MultiTurnStreamItem::FinalResponse(r)) => {
                out.final_text = Some(r.output().clone());
                out.usage = Some(format!("in={:?} out={:?} total={:?}", r.usage.input_tokens, r.usage.output_tokens, r.usage.total_tokens));
                out.history_json = serde_json::to_string(&r.messages).ok();
            }
            Ok(_) => {}
            Err(e) => {
                if let PromptError::Cancelled { chat_history, .. } = &e {
                    out.cancelled_history = Some(chat_history.len());
                }
                out.err = Some(e.to_string());
            }
        }
    }
    out
}

pub async fn item6() {
    println!("\n=== ITEM 6: hooks (rig AgentHook on the streaming runner, mock model reporting usage) ===");
    // A: pass-through, show the event order and the usage the hook and the final response saw.
    let log: Log = Default::default();
    let o = run(one_tool_model(), Probe { log: log.clone(), cfg: Cfg::default() }).await;
    println!("6A final={:?} usage={:?}", o.final_text, o.usage);
    for l in log.lock().unwrap().iter() { println!("6A event: {l}"); }

    // B: Stop at each point.
    for point in ["run_start", "model_select", "completion_call", "text_delta", "tool_call_delta", "model_turn_finished", "dispatch_tool", "outcome_tool"] {
        let log: Log = Default::default();
        let o = run(one_tool_model(), Probe { log: log.clone(), cfg: Cfg { stop_at: Some(point), ..Default::default() } }).await;
        let l = log.lock().unwrap();
        let tool_ran = l.iter().any(|s| s.starts_with("outcome ToolCall") || s.starts_with("outcome tool"));
        println!("6B stop@{point}: err={:?} cancelled_history_len={:?} tool_outcome_seen={} events={}", o.err, o.cancelled_history, tool_ran, l.len());
    }

    // C: Patch / Deny / rewrite on the tool dispatch.
    let log: Log = Default::default();
    let o = run(one_tool_model(), Probe { log: log.clone(), cfg: Cfg { patch_tool_args: true, ..Default::default() } }).await;
    let h = o.history_json.clone().unwrap_or_default();
    println!("6C Patch args x:1,y:2 -> 10,20: final={:?} history has result 30: {} (has 3: {})", o.final_text, h.contains("\"30\"") || h.contains("30"), h.contains("\"3\""));
    let o = run(one_tool_model(), Probe { log: Default::default(), cfg: Cfg { deny_tool: Some(ErrorKind::Other), ..Default::default() } }).await;
    let h = o.history_json.clone().unwrap_or_default();
    println!("6C Deny(non-Cancelled) -> skipped result the model sees: final={:?} err={:?} history mentions denied: {}", o.final_text, o.err, h.contains("denied by policy"));
    let o = run(one_tool_model(), Probe { log: Default::default(), cfg: Cfg { deny_tool: Some(ErrorKind::Cancelled), ..Default::default() } }).await;
    println!("6C Deny(Cancelled) cancels the run: err={:?} history_len={:?}", o.err, o.cancelled_history);
    let o = run(one_tool_model(), Probe { log: Default::default(), cfg: Cfg { rewrite_tool_output: true, ..Default::default() } }).await;
    let h = o.history_json.clone().unwrap_or_default();
    println!("6C on_outcome rewrite_tool_output: final={:?} history has REDACTED: {} has raw 3: {}", o.final_text, h.contains("REDACTED"), h.contains("\"3\""));
}

// ---------------------------------------------------------------- item 5 ----

/// Pairing-safe trim: keep the first message and the last complete tool exchange; everything between is
/// dropped from the REQUEST (never from the run). The dropped span is stored in an external store, keyed by turn.
/// `history` precedes the prompt; the prompt (last) is the tool result of the newest exchange.
pub fn trim(history: &[Arc<rig_core::completion::Message>], keep_tail: usize) -> (Vec<Arc<Message>>, usize) {
    let n = history.len();
    if n <= 1 + keep_tail {
        return (history.to_vec(), 0);
    }
    let mut out = vec![history[0].clone()];
    out.extend_from_slice(&history[n - keep_tail..]);
    (out, n - 1 - keep_tail)
}

pub struct TrimHook {
    pub store: Arc<Mutex<HashMap<String, String>>>, // external store: doc id -> text
    pub keep_tail: usize,
    pub log: Log,
}
impl AgentHook for TrimHook {
    async fn on_completion_call(&self, _c: &HookContext, e: CompletionCallEvent<'_>) -> CompletionCallAction {
        if e.turn < 4 {
            return CompletionCallAction::Continue;
        }
        let (kept, dropped) = trim(e.history, self.keep_tail);
        // "retrieval" from the external store: the facts that were in the dropped span.
        let docs: Vec<Document> = self
            .store
            .lock()
            .unwrap()
            .iter()
            .map(|(id, text)| Document { id: id.clone(), text: text.clone(), additional_props: Default::default() })
            .collect();
        let mut sorted = docs;
        sorted.sort_by(|a, b| a.id.cmp(&b.id));
        self.log.lock().unwrap().push(format!(
            "turn {}: history {} -> {} (dropped {dropped}), {} documents retrieved; kept pairing valid: {:?}",
            e.turn,
            e.history.len(),
            kept.len(),
            sorted.len(),
            {
                let mut all: Vec<Arc<Message>> = kept.clone();
                all.push(Arc::new(e.prompt.clone()));
                rig_core::conversation::pairing::validate(&all)
            }
        ));
        CompletionCallAction::patch(RequestPatch::new().history(kept).extra_context(sorted))
    }
}

pub fn four_turn_model() -> MockCompletionModel {
    let call = |id: &str, x: i32, y: i32| vec![E::tool_call(id, "add", serde_json::json!({"x": x, "y": y})), E::final_response(usage(10, 5))];
    MockCompletionModel::from_stream_turns([
        call("k1", 1, 2),
        call("k2", 3, 4),
        call("k3", 5, 6),
        vec![E::text("sum is 3, 7, 11"), E::final_response(usage(10, 5))],
    ])
}

pub async fn item5() {
    println!("\n=== ITEM 5: context replacement via on_completion_call (RequestPatch.history + extra_context) ===");
    let store: Arc<Mutex<HashMap<String, String>>> = Default::default();
    store.lock().unwrap().insert("fact-1".into(), "turns 1-2 computed 1+2=3 and 3+4=7".into());
    let log: Log = Default::default();
    let model = four_turn_model();
    let probe = model.clone();
    let agent = AgentBuilder::new(model).tool(MockAddTool).build();
    let mut s = agent.prompt("add three pairs").max_turns(6).add_hook(TrimHook { store, keep_tail: 1, log: log.clone() }).stream();
    let mut fin = None;
    let mut err = None;
    while let Some(i) = s.next().await {
        match i {
            Ok(rig_agent::agent::MultiTurnStreamItem::FinalResponse(r)) => fin = Some((r.output().clone(), r.messages.len())),
            Ok(_) => {}
            Err(e) => err = Some(e.to_string()),
        }
    }
    println!("5 trim (pairing-safe) final={fin:?} err={err:?}");
    for l in log.lock().unwrap().iter() { println!("5 hook: {l}"); }
    let reqs = probe.requests();
    for (i, r) in reqs.iter().enumerate() {
        let dump = format!("{:?}", r.chat_history);
        println!("5 request {} chat_history={} documents={} doc_text_in_messages={} mentions_first_prompt={}", i + 1, r.chat_history.len(), r.documents.len(), dump.contains("turns 1-2 computed"), dump.contains("add three pairs"));
    }
    println!("5 AUTHORITATIVE copy: run history after the run has {} messages (untrimmed: prompt + 3 x (call,result) + final = 8); requests were trimmed copies", fin.as_ref().map(|f| f.1).unwrap_or(0));

    // Naive trim that cuts between a tool call and its result.
    let log: Log = Default::default();
    struct Naive(Log);
    impl AgentHook for Naive {
        async fn on_completion_call(&self, _c: &HookContext, e: CompletionCallEvent<'_>) -> CompletionCallAction {
            if e.turn < 4 { return CompletionCallAction::Continue; }
            // keep the first message and the 2nd-to-last (an assistant tool call) only: orphan call + orphan result (the prompt)
            let n = e.history.len();
            let kept = vec![e.history[0].clone(), e.history[n - 3].clone()];
            let mut all = kept.clone();
            all.push(Arc::new(e.prompt.clone()));
            self.0.lock().unwrap().push(format!("naive cut pairing: {:?}", rig_core::conversation::pairing::validate(&all)));
            CompletionCallAction::patch(RequestPatch::new().history(kept))
        }
    }
    let model = four_turn_model();
    let probe2 = model.clone();
    let agent = AgentBuilder::new(model).tool(MockAddTool).build();
    let mut s = agent.prompt("add three pairs").max_turns(6).add_hook(Naive(log.clone())).stream();
    let mut err = None;
    let mut fin = None;
    while let Some(i) = s.next().await {
        match i {
            Ok(rig_agent::agent::MultiTurnStreamItem::FinalResponse(r)) => fin = Some(r.output().clone()),
            Ok(_) => {}
            Err(e) => err = Some(e.to_string()),
        }
    }
    for l in log.lock().unwrap().iter() { println!("5 {l}"); }
    if let Some(r) = probe2.requests().last() {
        let kinds: Vec<String> = r.chat_history.iter().map(|m| { let t = format!("{m:?}"); if t.contains("ToolResult") { "U(tool_result)".to_string() } else if t.contains("ToolCall") { "A(tool_call)".to_string() } else if t.starts_with("User") { "U".to_string() } else { "A".to_string() } }).collect();
        println!("5 naive trim: what reached the model on turn 4: {kinds:?}");
    }
    println!("5 naive trim: final={fin:?} err={err:?} requests_reached_model={}", probe2.request_count());
}

/// Item 5, sans-I/O half: the same trim as a host-owned patch function, persisted mid-run and resumed.
pub async fn item5_resume() {
    use crate::driver::{Driver, Kill};
    use crate::hosts::SideEffects;
    use rig_agent::run::{AgentRun, RunSpec};
    println!("\n=== ITEM 5b: trim + resume (sans-I/O driver, snapshot taken before the trimmed turn) ===");
    let mut spec = RunSpec::new();
    spec.max_turns = Some(6);
    let mut trimmer = |run: &AgentRun, turn: usize| -> Option<RequestPatch> {
        if turn < 4 {
            return None;
        }
        let msgs = run.messages();
        let (prompt_less, _) = msgs.split_at(msgs.len() - 1); // everything before the prompt
        let (kept, _) = trim(prompt_less, 1);
        let doc = Document { id: "fact-1".into(), text: "turns 1-2 computed 1+2=3 and 3+4=7".into(), additional_props: Default::default() };
        Some(RequestPatch::new().history(kept).context(doc))
    };
    let model = four_turn_model();
    let probe = model.clone();
    let mut host = SideEffects::new();
    let mut d = Driver::new(model, spec.clone(), &mut host);
    d.patch = Some(&mut trimmer);
    let mut run = AgentRun::new("add three pairs").max_turns(6);
    let end = d.drive(&mut run, &Kill::default()).await;
    println!("5b uninterrupted end={end:?}; run history msgs={} (authoritative, untrimmed)", run.full_history().len());
    let h1 = serde_json::to_string(&run.full_history()).unwrap();
    let snap3 = d.snapshots.iter().find(|(l, _)| l == "CallModel#3").unwrap().1.clone();
    let snap4 = d.snapshots.iter().find(|(l, _)| l == "CallModel#4").unwrap().1.clone();
    drop(d);
    let last_len = probe.requests().last().map(|r| r.chat_history.len());
    println!("5b turn-4 request chat_history msgs (trimmed copy)={last_len:?}");
    println!("5b snapshot at CallModel#4 holds {} bytes and the untrimmed history: contains first tool result 'k1': {}", snap4.len(), snap4.contains("k1"));
    // Resume from CallModel#3 (before the trim ever ran) in a new process.
    let mut trimmer2 = |run: &AgentRun, turn: usize| -> Option<RequestPatch> {
        if turn < 4 { return None; }
        let msgs = run.messages();
        let (prompt_less, _) = msgs.split_at(msgs.len() - 1);
        let (kept, _) = trim(prompt_less, 1);
        let doc = Document { id: "fact-1".into(), text: "turns 1-2 computed 1+2=3 and 3+4=7".into(), additional_props: Default::default() };
        Some(RequestPatch::new().history(kept).context(doc))
    };
    let mut run2: AgentRun = serde_json::from_str(&snap3).unwrap();
    let m2 = MockCompletionModel::from_stream_turns([
        vec![E::tool_call("k3", "add", serde_json::json!({"x":5,"y":6})), E::final_response(usage(10, 5))],
        vec![E::text("sum is 3, 7, 11"), E::final_response(usage(10, 5))],
    ]);
    let p2 = m2.clone();
    let mut host2 = SideEffects::new();
    let mut d2 = Driver::new(m2, spec, &mut host2);
    d2.patch = Some(&mut trimmer2);
    let end = d2.drive(&mut run2, &Kill::default()).await;
    println!("5b resumed from CallModel#3: end={end:?}; run history msgs={}", run2.full_history().len());
    println!("5b resumed turn-4 request chat_history msgs={:?}", p2.requests().last().map(|r| r.chat_history.len()));
    println!("5b same final history as the uninterrupted run: {}", serde_json::to_string(&run2.full_history()).unwrap() == h1);
}
