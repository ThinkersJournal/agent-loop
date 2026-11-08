// SPDX-License-Identifier: MIT OR Apache-2.0
//! Events: the driver reports each step in order; a stop, an error and a resume are visible; `Status` folds activity and
//! cost from them, and an unreported token count stays unknown.
use std::sync::Arc;

use agent_loop::{
    Activity, Driver, DriverError, End, Event, EventLog, MemoryStore, Model, SessionStore, Status,
    Step, StoreError, ToolHost, ToolOutcome,
};
use rig_core::completion::{CompletionRequest, ProviderCapabilities, ToolDefinition};
use rig_core::error::ProviderError;
use rig_core::message::ToolName;
use rig_core::streaming::CompletionStream;
use rig_core::test_utils::{MockCompletionModel, MockStreamEvent as E};

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
            E::final_response_with_total_tokens(7),
        ],
        vec![E::text("done"), E::final_response_with_total_tokens(5)],
    ]))
}

struct Ping {
    fail: bool,
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
        if self.fail {
            ToolOutcome::error("boom")
        } else {
            ToolOutcome::ok("pong")
        }
    }
}

fn spec() -> agent_loop::RunConfig {
    agent_loop::RunConfig::new().max_turns(4)
}

#[tokio::test]
async fn a_two_turn_run_emits_its_steps_in_order() {
    let log = EventLog::new();
    let mut d = Driver::new(
        "s",
        two_turns(),
        Ping { fail: false },
        MemoryStore::new(),
        spec(),
    );
    d.set_sink(log.clone());
    let mut run = d.open("q", 4).unwrap();
    assert_eq!(d.drive(&mut run).await.unwrap(), End::Done("done".into()));
    let kinds: Vec<&str> = log
        .events()
        .iter()
        .map(|e| match e {
            Event::Started { .. } => "started",
            Event::ModelCall { .. } => "model",
            Event::Usage { .. } => "usage",
            Event::ToolStarted { .. } => "tool+",
            Event::ToolFinished { .. } => "tool-",
            Event::Compacted { .. } => "compacted",
            Event::Stopped => "stopped",
            Event::Finished => "finished",
            Event::Error { .. } => "error",
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "started", "model", "usage", "tool+", "tool-", "model", "usage", "finished"
        ]
    );
    let evs = log.events();
    assert_eq!(evs[0], Event::Started { resumed: false });
    assert!(evs.contains(&Event::ToolStarted {
        id: "t1".into(),
        name: "ping".into()
    }));
    assert!(evs.contains(&Event::ToolFinished {
        id: "t1".into(),
        is_error: false
    }));
}

#[tokio::test]
async fn a_failing_tool_is_reported_as_an_error_result_not_a_run_error() {
    let log = EventLog::new();
    let mut d = Driver::new(
        "s",
        two_turns(),
        Ping { fail: true },
        MemoryStore::new(),
        spec(),
    );
    d.set_sink(log.clone());
    let mut run = d.open("q", 4).unwrap();
    d.drive(&mut run).await.unwrap();
    assert!(log.events().contains(&Event::ToolFinished {
        id: "t1".into(),
        is_error: true
    }));
    assert!(
        !log.events()
            .iter()
            .any(|e| matches!(e, Event::Error { .. }))
    );
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
async fn an_error_is_an_event_and_a_resume_says_so() {
    let mut store = MemoryStore::new();
    let log = EventLog::new();
    let mut d = Driver::new(
        "s",
        two_turns(),
        Ping { fail: false },
        FailAfter {
            inner: &mut store,
            left: 3,
        },
        spec(),
    );
    d.set_sink(log.clone());
    let mut run = d.open("q", 4).unwrap();
    assert!(matches!(
        d.drive(&mut run).await,
        Err(DriverError::Store(_))
    ));
    let last = log.events().pop().unwrap();
    assert!(
        matches!(&last, Event::Error { message } if message.contains("simulated crash")),
        "{last:?}"
    );
    assert_eq!(
        Status::from_events(&log.events()).activity,
        Activity::Failed
    );
    drop(d);

    // Resume from the stored run: the first event says resumed.
    let second = EventLog::new();
    let m2 = Mock(MockCompletionModel::from_stream_turns([vec![
        E::text("done"),
        E::final_response_with_total_tokens(5),
    ]]));
    let mut d2 = Driver::new("s", m2, Ping { fail: false }, store, spec());
    d2.set_sink(second.clone());
    let mut r2 = d2.open("ignored", 4).unwrap();
    d2.drive(&mut r2).await.unwrap();
    assert_eq!(second.events()[0], Event::Started { resumed: true });
    assert_eq!(second.events().last(), Some(&Event::Finished));
}

#[tokio::test]
async fn a_soft_stop_is_a_stopped_event_and_idle() {
    let log = EventLog::new();
    let mut d = Driver::new(
        "s",
        two_turns(),
        Ping { fail: false },
        MemoryStore::new(),
        spec(),
    );
    d.set_sink(log.clone());
    d.control().stop_soft();
    let mut run = d.open("q", 4).unwrap();
    assert_eq!(d.drive(&mut run).await.unwrap(), End::Stopped);
    assert_eq!(log.events().last(), Some(&Event::Stopped));
    assert_eq!(Status::from_events(&log.events()).activity, Activity::Idle);
    assert_eq!(
        log.events()
            .iter()
            .filter(|e| matches!(e, Event::ModelCall { .. }))
            .count(),
        0,
        "stopped before any model call"
    );
}

#[test]
fn status_folds_activity_and_keeps_unreported_tokens_unknown() {
    let busy = [
        Event::Started { resumed: false },
        Event::ModelCall { turn: 1 },
    ];
    assert_eq!(
        Status::from_events(&busy).activity,
        Activity::Busy(Step::Model { turn: 1 })
    );
    let tools = [
        Event::ToolStarted {
            id: "a".into(),
            name: "x".into(),
        },
        Event::ToolStarted {
            id: "b".into(),
            name: "x".into(),
        },
        Event::ToolFinished {
            id: "a".into(),
            is_error: false,
        },
    ];
    assert_eq!(
        Status::from_events(&tools).activity,
        Activity::Busy(Step::Tools { open: 1 })
    );

    // No usage reported at all: unknown, not zero.
    let none = Status::from_events(&[Event::ModelCall { turn: 1 }]);
    assert_eq!((none.tokens_in, none.tokens_out), (None, None));
    // One call reports only output: input stays unknown; later calls add.
    let some = Status::from_events(&[
        Event::Usage {
            input: None,
            output: Some(3),
        },
        Event::Usage {
            input: None,
            output: Some(4),
        },
        Event::Usage {
            input: Some(10),
            output: None,
        },
    ]);
    assert_eq!((some.tokens_in, some.tokens_out), (Some(10), Some(7)));
}
