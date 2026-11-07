// SPDX-License-Identifier: MIT OR Apache-2.0
//! RunControl: inject reaches the model and survives a restart (saved in the run); soft stop finishes the tool
//! batch, saves, and resumes; a message queued at stop time is delivered after resume.
use std::sync::Arc;

use agent_loop::{
    Driver, DriverError, End, MemoryStore, Model, RunControl, SessionStore, StoreError, ToolHost,
    ToolOutcome,
};
use rig_core::completion::{CompletionRequest, ProviderCapabilities, ToolDefinition};
use rig_core::error::ProviderError;
use rig_core::message::ToolName;
use rig_core::streaming::CompletionStream;
use rig_core::test_utils::{MockCompletionModel, MockStreamEvent as E};

/// Wraps a mock and keeps a clone so the test can read the requests the model received.
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
            E::tool_call("t1", "ping", serde_json::json!({})),
            E::final_response_with_total_tokens(1),
        ],
        vec![E::text("done"), E::final_response_with_total_tokens(1)],
    ]))
}

fn turn_two_only() -> Mock {
    Mock(MockCompletionModel::from_stream_turns([vec![
        E::text("done"),
        E::final_response_with_total_tokens(1),
    ]]))
}

/// One tool; `on_call` lets a test poke the control handle while a tool batch is running.
struct Ping {
    control: Option<RunControl>,
    on_call: fn(&RunControl),
    runs: usize,
}
impl Ping {
    fn plain() -> Self {
        Self {
            control: None,
            on_call: |_| {},
            runs: 0,
        }
    }
}
impl ToolHost for Ping {
    fn definitions(&self) -> Vec<Arc<ToolDefinition>> {
        vec![Arc::new(ToolDefinition::new(
            ToolName::new("ping").unwrap(),
            "ping",
            serde_json::json!({"type":"object","properties":{}}),
        ))]
    }
    async fn call(&mut self, _: &str, _: &str, _: &serde_json::Value) -> ToolOutcome {
        self.runs += 1;
        if let Some(c) = &self.control {
            (self.on_call)(c);
        }
        ToolOutcome::ok("pong")
    }
}

fn spec() -> agent_loop::RunConfig {
    agent_loop::RunConfig::new().max_turns(4)
}

fn requests_text(m: &Mock) -> Vec<String> {
    m.0.requests().iter().map(|r| format!("{r:?}")).collect()
}

struct FailAfter<'a> {
    inner: &'a mut MemoryStore,
    left: usize,
}
impl SessionStore for FailAfter<'_> {
    fn load(&self, s: &str) -> Result<Option<(u64, String)>, StoreError> {
        self.inner.load(s)
    }
    fn save(&mut self, s: &str, e: u64, snap: &str) -> Result<u64, StoreError> {
        if self.left == 0 {
            return Err(StoreError::Backend("simulated crash".into()));
        }
        self.left -= 1;
        self.inner.save(s, e, snap)
    }
}

#[tokio::test]
async fn inject_reaches_the_model_on_every_later_turn() {
    let m = two_turns();
    let mut d = Driver::new("s", m.clone(), Ping::plain(), MemoryStore::new(), spec());
    d.control().inject("INJECTED-ONE");
    let mut run = d.open("first question", 4).unwrap();
    assert_eq!(d.drive(&mut run).await.unwrap(), End::Done("done".into()));
    let reqs = requests_text(&m);
    assert_eq!(reqs.len(), 2);
    assert!(reqs[0].contains("INJECTED-ONE"), "turn 1: {}", reqs[0]);
    assert!(reqs[1].contains("INJECTED-ONE"), "turn 2: {}", reqs[1]);
    // The run's own history is untouched: injected text lives in a host record, spliced per request.
    assert!(!run.history_json().unwrap().contains("INJECTED-ONE"));
}

#[tokio::test]
async fn control_without_inject_changes_nothing() {
    let m = two_turns();
    let mut d = Driver::new("s", m.clone(), Ping::plain(), MemoryStore::new(), spec());
    let mut run = d.open("first question", 4).unwrap();
    assert_eq!(d.drive(&mut run).await.unwrap(), End::Done("done".into()));
    assert!(requests_text(&m).iter().all(|r| !r.contains("host.inject")));
}

#[tokio::test]
async fn injected_message_survives_a_restart() {
    let mut store = MemoryStore::new();
    let mut d = Driver::new(
        "s",
        two_turns(),
        Ping::plain(),
        FailAfter {
            inner: &mut store,
            left: 3,
        },
        spec(),
    );
    d.control().inject("INJECTED-SURVIVES");
    let mut run = d.open("first question", 4).unwrap();
    assert!(matches!(
        d.drive(&mut run).await,
        Err(DriverError::Store(StoreError::Backend(_)))
    ));
    drop(d);

    let m2 = turn_two_only();
    let mut d2 = Driver::new("s", m2.clone(), Ping::plain(), store, spec());
    let mut r2 = d2.open("ignored", 4).unwrap();
    assert_eq!(d2.drive(&mut r2).await.unwrap(), End::Done("done".into()));
    assert_eq!(d2.tools.runs, 0, "answered tool call must not re-run");
    let reqs = requests_text(&m2);
    assert_eq!(reqs.len(), 1);
    assert!(
        reqs[0].contains("INJECTED-SURVIVES"),
        "resumed request: {}",
        reqs[0]
    );
}

#[tokio::test]
async fn soft_stop_finishes_the_batch_saves_and_resumes_with_the_queued_message() {
    let m = two_turns();
    let mut d = Driver::new("s", m.clone(), Ping::plain(), MemoryStore::new(), spec());
    // The tool gets the control handle: during its call it asks to stop and queues a message.
    d.tools.control = Some(d.control());
    d.tools.on_call = |c| {
        c.stop_soft();
        c.inject("QUEUED-AT-STOP");
    };
    let mut run = d.open("first question", 4).unwrap();
    assert_eq!(d.drive(&mut run).await.unwrap(), End::Stopped);
    assert_eq!(d.tools.runs, 1);
    assert_eq!(
        m.0.requests().len(),
        1,
        "no second model call after the stop"
    );
    // The batch was finished: the tool result is in the run.
    assert!(run.history_json().unwrap().contains("pong"));
    let store = d.store;

    let m2 = turn_two_only();
    let mut d2 = Driver::new("s", m2.clone(), Ping::plain(), store, spec());
    let mut r2 = d2.open("ignored", 4).unwrap();
    assert_eq!(d2.drive(&mut r2).await.unwrap(), End::Done("done".into()));
    assert_eq!(d2.tools.runs, 0);
    assert!(requests_text(&m2)[0].contains("QUEUED-AT-STOP"));
}
