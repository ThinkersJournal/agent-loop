// SPDX-License-Identifier: MIT OR Apache-2.0
//! Durable inject: a message written to the inbox survives a crash before delivery, is delivered once even if the
//! acknowledgement is lost, and is acknowledged only after the run holding it is saved.
use std::sync::Arc;

use agent_loop::{
    Driver, DriverError, End, Inbox, InboxError, MemoryInbox, MemoryStore, Model, Queued,
    SessionStore, StoreError, ToolHost, ToolOutcome,
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

fn one_turn() -> Mock {
    turn_two_only()
}

struct Ping;
impl ToolHost for Ping {
    fn definitions(&self) -> Vec<Arc<ToolDefinition>> {
        vec![Arc::new(ToolDefinition::new(
            ToolName::new("ping").unwrap(),
            "ping",
            serde_json::json!({"type":"object","properties":{}}),
        ))]
    }
    async fn call(&mut self, _: &str, _: &str, _: &serde_json::Value) -> ToolOutcome {
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

/// An inbox whose acknowledgements are lost, as if the process died between the save and the ack.
struct LostAck(MemoryInbox);
impl Inbox for LostAck {
    fn append(&self, s: &str, t: &str) -> Result<u64, InboxError> {
        self.0.append(s, t)
    }
    fn pending(&self, s: &str) -> Result<Vec<Queued>, InboxError> {
        self.0.pending(s)
    }
    fn ack(&self, _: &str, _: &[u64]) -> Result<(), InboxError> {
        Err(InboxError::Backend("ack lost".into()))
    }
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
async fn a_durable_message_reaches_the_model_and_is_acknowledged_after() {
    let inbox = MemoryInbox::new();
    let m = two_turns();
    let mut d = Driver::new("s", m.clone(), Ping, MemoryStore::new(), spec())
        .with_inbox(Arc::new(inbox.clone()));
    let id = d.control().inject_durable("DURABLE-ONE").unwrap();
    assert_eq!(
        inbox.pending("s").unwrap(),
        [Queued {
            id,
            text: "DURABLE-ONE".into()
        }]
    );
    let mut run = d.open("q", 4).unwrap();
    assert_eq!(d.drive(&mut run).await.unwrap(), End::Done("done".into()));
    let reqs = requests(&m);
    assert!(reqs[0].contains("DURABLE-ONE") && reqs[1].contains("DURABLE-ONE"));
    assert!(inbox.pending("s").unwrap().is_empty(), "acknowledged");
}

#[tokio::test]
async fn a_message_written_before_a_crash_is_delivered_after_it() {
    let inbox = MemoryInbox::new();
    {
        // The first driver takes the message into the inbox and dies before any boundary: nothing was saved.
        let d = Driver::new("s", one_turn(), Ping, MemoryStore::new(), spec())
            .with_inbox(Arc::new(inbox.clone()));
        d.control().inject_durable("SURVIVES-CRASH").unwrap();
    }
    assert_eq!(inbox.pending("s").unwrap().len(), 1);
    let m = one_turn();
    let mut d2 = Driver::new("s", m.clone(), Ping, MemoryStore::new(), spec())
        .with_inbox(Arc::new(inbox.clone()));
    let mut run = d2.open("q", 4).unwrap();
    assert_eq!(d2.drive(&mut run).await.unwrap(), End::Done("done".into()));
    assert!(requests(&m)[0].contains("SURVIVES-CRASH"));
    // Control: the in-memory inject would have been lost with the first driver.
    let lost = Driver::new("s2", one_turn(), Ping, MemoryStore::new(), spec());
    lost.control().inject("MEMORY-ONLY");
    drop(lost);
    let m3 = one_turn();
    let mut d3 = Driver::new("s2", m3.clone(), Ping, MemoryStore::new(), spec());
    let mut r3 = d3.open("q", 4).unwrap();
    d3.drive(&mut r3).await.unwrap();
    assert!(!requests(&m3)[0].contains("MEMORY-ONLY"));
}

#[tokio::test]
async fn a_lost_acknowledgement_redelivers_but_the_run_holds_it_once() {
    let mem = MemoryInbox::new();
    let mut store = MemoryStore::new();
    let mut d = Driver::new(
        "s",
        two_turns(),
        Ping,
        FailAfter {
            inner: &mut store,
            left: 3,
        },
        spec(),
    )
    .with_inbox(Arc::new(LostAck(mem.clone())));
    d.control().inject_durable("ONCE-ONLY").unwrap();
    let mut run = d.open("q", 4).unwrap();
    // Boundary 1 saves the run holding the message; its ack is lost; the save at boundary 2 crashes.
    assert!(matches!(
        d.drive(&mut run).await,
        Err(DriverError::Store(StoreError::Backend(_)))
    ));
    drop(d);
    assert_eq!(
        mem.pending("s").unwrap().len(),
        1,
        "the ack was lost, so it is still pending"
    );

    let m2 = turn_two_only();
    let mut d2 =
        Driver::new("s", m2.clone(), Ping, store, spec()).with_inbox(Arc::new(mem.clone()));
    let mut r2 = d2.open("ignored", 4).unwrap();
    assert_eq!(d2.drive(&mut r2).await.unwrap(), End::Done("done".into()));
    let req = &requests(&m2)[0];
    assert_eq!(req.matches("ONCE-ONLY").count(), 1, "{req}");
    assert!(
        mem.pending("s").unwrap().is_empty(),
        "acknowledged on resume"
    );
}

#[tokio::test]
async fn a_durable_message_at_stop_time_is_in_the_saved_run_and_delivered_on_resume() {
    let inbox = MemoryInbox::new();
    let mut d = Driver::new("s", two_turns(), Ping, MemoryStore::new(), spec())
        .with_inbox(Arc::new(inbox.clone()));
    d.control().inject_durable("AT-STOP").unwrap();
    d.control().stop_soft();
    let mut run = d.open("q", 4).unwrap();
    assert_eq!(d.drive(&mut run).await.unwrap(), End::Stopped);
    assert!(inbox.pending("s").unwrap().is_empty());
    let store = d.store;

    let m2 = one_turn();
    let mut d2 = Driver::new("s", m2.clone(), Ping, store, spec()).with_inbox(Arc::new(inbox));
    let mut r2 = d2.open("ignored", 4).unwrap();
    assert_eq!(d2.drive(&mut r2).await.unwrap(), End::Done("done".into()));
    assert!(requests(&m2)[0].contains("AT-STOP"));
}

#[test]
fn durable_inject_without_an_inbox_is_an_error_not_a_silent_drop() {
    let d = Driver::new("s", one_turn(), Ping, MemoryStore::new(), spec());
    assert!(matches!(
        d.control().inject_durable("x"),
        Err(InboxError::Backend(_))
    ));
}
