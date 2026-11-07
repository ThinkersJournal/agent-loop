// SPDX-License-Identifier: MIT OR Apache-2.0
//! Hooks: deny a tool, patch its arguments, rewrite its outcome, stop before a model call, and the wrap-up on stop.
use std::sync::{Arc, Mutex};

use agent_loop::{
    Driver, End, Event, EventLog, Hook, MemoryStore, Model, ModelDecision, StopReason, ToolCall,
    ToolDecision, ToolHost, ToolOutcome,
};
use rig_agent::run::RunSpec;
use rig_core::completion::{CompletionRequest, ProviderCapabilities, ToolDefinition};
use rig_core::error::ProviderError;
use rig_core::message::ToolName;
use rig_core::streaming::CompletionStream;
use rig_core::test_utils::{MockCompletionModel, MockStreamEvent as E};

#[derive(Clone)]
struct Mock(MockCompletionModel);
impl Model for Mock {
    fn capabilities(&self) -> ProviderCapabilities {
        self.0.capabilities().completion
    }
    async fn stream(&self, r: CompletionRequest) -> Result<CompletionStream, ProviderError> {
        self.0.stream(r)
    }
}

fn two_turns() -> Mock {
    Mock(MockCompletionModel::from_stream_turns([
        vec![
            E::tool_call("t1", "ping", serde_json::json!({"who": "model"})),
            E::final_response_with_total_tokens(1),
        ],
        vec![E::text("done"), E::final_response_with_total_tokens(1)],
    ]))
}

fn one_turn() -> Mock {
    Mock(MockCompletionModel::from_stream_turns([vec![
        E::text("done"),
        E::final_response_with_total_tokens(1),
    ]]))
}

/// Records the arguments of every call it actually runs.
#[derive(Default)]
struct Ping {
    ran_with: Vec<serde_json::Value>,
}
impl ToolHost for Ping {
    fn definitions(&self) -> Vec<Arc<ToolDefinition>> {
        vec![Arc::new(ToolDefinition::new(
            ToolName::new("ping").unwrap(),
            "ping",
            serde_json::json!({"type":"object","properties":{}}),
        ))]
    }
    async fn call(&mut self, _: &str, _: &str, args: &serde_json::Value) -> ToolOutcome {
        self.ran_with.push(args.clone());
        ToolOutcome::ok("pong")
    }
}

fn spec() -> RunSpec {
    let mut s = RunSpec::new();
    s.max_turns = Some(4);
    s
}

fn requests(m: &Mock) -> Vec<String> {
    m.0.requests().iter().map(|r| format!("{r:?}")).collect()
}

/// A hook built from closures, with a shared log of what it was asked.
#[derive(Default)]
struct Scripted {
    deny: Option<String>,
    patch: Option<serde_json::Value>,
    rewrite: Option<ToolOutcome>,
    stop_at_turn: Option<u64>,
    seen: Arc<Mutex<Vec<String>>>,
}
impl Hook for Scripted {
    fn before_model(&mut self, turn: u64) -> ModelDecision {
        self.seen.lock().unwrap().push(format!("model {turn}"));
        match self.stop_at_turn {
            Some(t) if turn >= t => ModelDecision::Stop("enough".into()),
            _ => ModelDecision::Continue,
        }
    }
    fn before_tool(&mut self, call: ToolCall<'_>) -> ToolDecision {
        self.seen
            .lock()
            .unwrap()
            .push(format!("before {} {}", call.name, call.args));
        if let Some(why) = &self.deny {
            return ToolDecision::Deny(why.clone());
        }
        match &self.patch {
            Some(v) => ToolDecision::Patch(v.clone()),
            None => ToolDecision::Allow,
        }
    }
    fn after_tool(&mut self, call: ToolCall<'_>, outcome: &ToolOutcome) -> Option<ToolOutcome> {
        self.seen.lock().unwrap().push(format!(
            "after {} {} {}",
            call.name, call.args, outcome.text
        ));
        self.rewrite.clone()
    }
    fn on_stop(&mut self, why: &StopReason) {
        self.seen.lock().unwrap().push(format!("stop {why:?}"));
    }
}

#[tokio::test]
async fn a_denied_call_never_runs_and_the_model_reads_the_reason() {
    let m = two_turns();
    let log = EventLog::new();
    let hook = Scripted {
        deny: Some("not allowed here".into()),
        ..Scripted::default()
    };
    let seen = hook.seen.clone();
    let mut d = Driver::new("s", m.clone(), Ping::default(), MemoryStore::new(), spec());
    d.set_hook(hook);
    d.set_sink(log.clone());
    let mut run = d.open("q", 4).unwrap();
    assert_eq!(d.drive(&mut run).await.unwrap(), End::Done("done".into()));
    assert!(d.tools.ran_with.is_empty(), "the tool must not run");
    assert!(requests(&m)[1].contains("denied: not allowed here"));
    // The call still shows in the event stream, as an error.
    assert!(log.events().contains(&Event::ToolFinished {
        id: "t1".into(),
        is_error: true
    }));
    // after_tool is not called for a denied call.
    assert!(!seen.lock().unwrap().iter().any(|l| l.starts_with("after")));
}

#[tokio::test]
async fn a_patch_changes_what_the_tool_gets_and_after_tool_sees_the_patched_args() {
    let m = two_turns();
    let hook = Scripted {
        patch: Some(serde_json::json!({"who": "hook"})),
        ..Scripted::default()
    };
    let seen = hook.seen.clone();
    let mut d = Driver::new("s", m.clone(), Ping::default(), MemoryStore::new(), spec());
    d.set_hook(hook);
    let mut run = d.open("q", 4).unwrap();
    d.drive(&mut run).await.unwrap();
    assert_eq!(d.tools.ran_with, [serde_json::json!({"who": "hook"})]);
    let seen = seen.lock().unwrap().clone();
    assert!(
        seen.contains(&r#"before ping {"who":"model"}"#.to_string()),
        "{seen:?}"
    );
    assert!(
        seen.contains(&r#"after ping {"who":"hook"} pong"#.to_string()),
        "{seen:?}"
    );
}

#[tokio::test]
async fn after_tool_can_rewrite_the_outcome_the_model_sees() {
    let m = two_turns();
    let hook = Scripted {
        rewrite: Some(ToolOutcome::ok("REWRITTEN-BY-HOOK")),
        ..Scripted::default()
    };
    let mut d = Driver::new("s", m.clone(), Ping::default(), MemoryStore::new(), spec());
    d.set_hook(hook);
    let mut run = d.open("q", 4).unwrap();
    d.drive(&mut run).await.unwrap();
    assert_eq!(d.tools.ran_with.len(), 1, "the tool did run");
    let second = &requests(&m)[1];
    assert!(second.contains("REWRITTEN-BY-HOOK"));
    assert!(!second.contains("pong"));
}

#[tokio::test]
async fn no_hook_and_an_all_default_hook_change_nothing() {
    struct Nothing;
    impl Hook for Nothing {}
    let m = two_turns();
    let mut d = Driver::new("s", m.clone(), Ping::default(), MemoryStore::new(), spec());
    d.set_hook(Nothing);
    let mut run = d.open("q", 4).unwrap();
    assert_eq!(d.drive(&mut run).await.unwrap(), End::Done("done".into()));
    assert_eq!(d.tools.ran_with, [serde_json::json!({"who": "model"})]);
    assert!(requests(&m)[1].contains("pong"));
}

#[tokio::test]
async fn a_hook_stop_saves_runs_the_wrap_up_and_resumes() {
    let hook = Scripted {
        stop_at_turn: Some(2),
        ..Scripted::default()
    };
    let seen = hook.seen.clone();
    let m = two_turns();
    let mut d = Driver::new("s", m.clone(), Ping::default(), MemoryStore::new(), spec());
    d.set_hook(hook);
    let mut run = d.open("q", 4).unwrap();
    // Turn 1 runs the tool; the hook stops before turn 2's model call.
    assert_eq!(d.drive(&mut run).await.unwrap(), End::Stopped);
    assert_eq!(d.tools.ran_with.len(), 1);
    assert_eq!(m.0.requests().len(), 1, "no second model call");
    let seen_now = seen.lock().unwrap().clone();
    assert_eq!(
        seen_now.last().map(String::as_str),
        Some(r#"stop Hook("enough")"#),
        "{seen_now:?}"
    );
    let store = d.store;

    // Resume without the hook: the run was saved before the stop, so it finishes and does not rerun the tool.
    let m2 = one_turn();
    let mut d2 = Driver::new("s", m2.clone(), Ping::default(), store, spec());
    let mut r2 = d2.open("ignored", 4).unwrap();
    assert_eq!(d2.drive(&mut r2).await.unwrap(), End::Done("done".into()));
    assert!(d2.tools.ran_with.is_empty());
}

#[tokio::test]
async fn a_soft_stop_runs_the_wrap_up_with_reason_soft() {
    let hook = Scripted::default();
    let seen = hook.seen.clone();
    let mut d = Driver::new(
        "s",
        two_turns(),
        Ping::default(),
        MemoryStore::new(),
        spec(),
    );
    d.set_hook(hook);
    d.control().stop_soft();
    let mut run = d.open("q", 4).unwrap();
    assert_eq!(d.drive(&mut run).await.unwrap(), End::Stopped);
    let seen = seen.lock().unwrap().clone();
    assert_eq!(
        seen,
        ["stop Soft"],
        "before_model is not asked once a soft stop is taken"
    );
}
