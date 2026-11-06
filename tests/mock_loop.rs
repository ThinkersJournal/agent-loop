// SPDX-License-Identifier: MIT OR Apache-2.0
//! Slice 1 acceptance: the three traits drive a rig `AgentRun` with a mock model, and a run whose store dies
//! mid-run resumes from the stored snapshot to the same history as an uninterrupted one.
use std::sync::Arc;

use agent_loop::{
    Driver, DriverError, End, MemoryStore, Model, SessionStore, StoreError, ToolHost, ToolOutcome,
};
use rig_agent::run::RunSpec;
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

fn model() -> Mock {
    Mock(MockCompletionModel::from_stream_turns([
        vec![
            E::tool_call("t1", "add", serde_json::json!({"x":1,"y":2})),
            E::final_response_with_total_tokens(3),
        ],
        vec![
            E::text("the answer is 3"),
            E::final_response_with_total_tokens(1),
        ],
    ]))
}

/// A model that only has the second turn: what a resumed run needs after the tool was answered.
fn model_turn2() -> Mock {
    Mock(MockCompletionModel::from_stream_turns([vec![
        E::text("the answer is 3"),
        E::final_response_with_total_tokens(1),
    ]]))
}

#[derive(Default)]
struct Adder {
    calls: Vec<String>,
}
impl ToolHost for Adder {
    fn definitions(&self) -> Vec<Arc<ToolDefinition>> {
        vec![Arc::new(ToolDefinition::new(
            ToolName::new("add").unwrap(),
            "add x and y",
            serde_json::json!({"type":"object","properties":{"x":{"type":"number"},"y":{"type":"number"}},"required":["x","y"]}),
        ))]
    }
    async fn call(&mut self, id: &str, _name: &str, a: &serde_json::Value) -> ToolOutcome {
        self.calls.push(id.to_string());
        ToolOutcome::ok((a["x"].as_i64().unwrap() + a["y"].as_i64().unwrap()).to_string())
    }
}

fn spec() -> RunSpec {
    let mut s = RunSpec::new();
    s.max_turns = Some(3);
    s
}

/// Fails every save after `left` successful ones: a crash at a chosen step boundary.
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
async fn drives_to_done_and_snapshots() {
    let mut d = Driver::new("s", model(), Adder::default(), MemoryStore::new(), spec());
    let mut run = d.open("add 1 and 2", 3).unwrap();
    assert_eq!(
        d.drive(&mut run).await.unwrap(),
        End::Done("the answer is 3".into())
    );
    assert_eq!(d.tools.calls, ["t1"]);
    assert_eq!(run.full_history().len(), 4);
    let (seq, json) = d.store.load("s").unwrap().expect("snapshot stored");
    assert_eq!(seq, 5, "CallModel, CallTools, answered, CallModel, Done");
    assert!(json.contains("the answer is 3"));
}

#[tokio::test]
async fn resume_after_crash_matches_uninterrupted_run() {
    let mut d = Driver::new("s", model(), Adder::default(), MemoryStore::new(), spec());
    let mut run = d.open("add 1 and 2", 3).unwrap();
    d.drive(&mut run).await.unwrap();
    let control = serde_json::to_string(&run.full_history()).unwrap();

    // The store dies on the 4th save (the second CallModel): saves 1-3 were CallModel, CallTools, answered.
    let mut store = MemoryStore::new();
    let mut d2 = Driver::new(
        "s",
        model(),
        Adder::default(),
        FailAfter {
            inner: &mut store,
            left: 3,
        },
        spec(),
    );
    let mut r2 = d2.open("add 1 and 2", 3).unwrap();
    assert!(matches!(
        d2.drive(&mut r2).await,
        Err(DriverError::Store(StoreError::Backend(_)))
    ));
    assert_eq!(d2.tools.calls, ["t1"]);
    drop(d2);
    assert_eq!(store.load("s").unwrap().unwrap().0, 3);

    // A new process: fresh model (turn 2 only), fresh tools, the stored run. The tool must not run again.
    let mut d3 = Driver::new("s", model_turn2(), Adder::default(), store, spec());
    let mut r3 = d3.open("ignored on resume", 3).unwrap();
    assert_eq!(
        d3.drive(&mut r3).await.unwrap(),
        End::Done("the answer is 3".into())
    );
    assert!(
        d3.tools.calls.is_empty(),
        "answered tool call must not re-run"
    );
    assert_eq!(serde_json::to_string(&r3.full_history()).unwrap(), control);
}

#[test]
fn second_writer_is_refused() {
    let mut s = MemoryStore::new();
    assert_eq!(s.save("a", 0, "x").unwrap(), 1);
    assert_eq!(
        s.save("a", 0, "y"),
        Err(StoreError::Conflict {
            expected: 0,
            actual: 1
        })
    );
    assert_eq!(s.load("a").unwrap(), Some((1, "x".into())));
}

#[test]
fn unreadable_snapshot_stops_instead_of_starting_fresh() {
    let mut store = MemoryStore::new();
    store.save("s", 0, "{not a run").unwrap();
    let mut d = Driver::new("s", model(), Adder::default(), store, spec());
    assert!(matches!(d.open("p", 3), Err(DriverError::Snapshot(_))));
}
