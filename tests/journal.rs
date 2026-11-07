// SPDX-License-Identifier: MIT OR Apache-2.0
//! The idempotency journal: a crash after a tool ran but before its answer was saved must not run it twice.
use std::sync::Arc;

use agent_loop::{
    Driver, DriverError, End, Journal, MemoryStore, Model, SessionStore, StoreError, ToolHost,
    ToolOutcome,
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
            E::tool_call("c1", "charge", serde_json::json!({"cents":100})),
            E::final_response_with_total_tokens(3),
        ],
        vec![E::text("charged"), E::final_response_with_total_tokens(1)],
    ]))
}

fn model_turn2() -> Mock {
    Mock(MockCompletionModel::from_stream_turns([vec![
        E::text("charged"),
        E::final_response_with_total_tokens(1),
    ]]))
}

/// The world: every real charge is logged.
#[derive(Default)]
struct Bank {
    charges: Vec<i64>,
}
impl ToolHost for Bank {
    fn definitions(&self) -> Vec<Arc<ToolDefinition>> {
        vec![Arc::new(ToolDefinition::new(
            ToolName::new("charge").unwrap(),
            "charge cents",
            serde_json::json!({"type":"object","properties":{"cents":{"type":"number"}},"required":["cents"]}),
        ))]
    }
    async fn call(&mut self, _id: &str, _n: &str, a: &serde_json::Value) -> ToolOutcome {
        self.charges.push(a["cents"].as_i64().unwrap());
        ToolOutcome::ok("charged")
    }
}

fn spec() -> RunSpec {
    let mut s = RunSpec::new();
    s.max_turns = Some(3);
    s
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

/// Crash on the save that follows the tool's answer: saves 1-2 (CallModel, CallTools) are durable, the tool has run.
async fn crashed_after_tool_ran(store: &mut MemoryStore) -> Journal<Bank> {
    let mut d = Driver::new(
        "s",
        model(),
        Journal::new(Bank::default()),
        FailAfter {
            inner: store,
            left: 2,
        },
        spec(),
    );
    let mut run = d.open("charge me", 3).unwrap();
    assert!(matches!(
        d.drive(&mut run).await,
        Err(DriverError::Store(StoreError::Backend(_)))
    ));
    assert_eq!(d.tools.inner().charges, [100]);
    d.tools
}

#[tokio::test]
async fn without_the_journal_the_charge_repeats() {
    // Control: same crash, plain host. The stored run still has the call pending, so a new life charges again.
    let mut store = MemoryStore::new();
    let mut d = Driver::new(
        "s",
        model(),
        Bank::default(),
        FailAfter {
            inner: &mut store,
            left: 2,
        },
        spec(),
    );
    let mut run = d.open("charge me", 3).unwrap();
    assert!(d.drive(&mut run).await.is_err());
    drop(d);
    let mut d2 = Driver::new("s", model_turn2(), Bank::default(), store, spec());
    let mut r2 = d2.open("x", 3).unwrap();
    assert_eq!(
        d2.drive(&mut r2).await.unwrap(),
        End::Done("charged".into())
    );
    assert_eq!(
        d2.tools.charges,
        [100],
        "the second life ran the tool again (at-least-once)"
    );
}

#[tokio::test]
async fn journal_stops_the_repeat_across_a_restart() {
    let mut store = MemoryStore::new();
    let saved = crashed_after_tool_ran(&mut store).await.to_json();

    let journal = Journal::from_json(&saved, Bank::default()).unwrap();
    let mut d2 = Driver::new("s", model_turn2(), journal, store, spec());
    let mut r2 = d2.open("x", 3).unwrap();
    assert_eq!(
        d2.drive(&mut r2).await.unwrap(),
        End::Done("charged".into())
    );
    assert!(
        d2.tools.inner().charges.is_empty(),
        "the tool must not run in the second life"
    );
    assert_eq!(d2.tools.hits(), 1);
}

#[tokio::test]
async fn journal_round_trips_errors_too() {
    struct Failing;
    impl ToolHost for Failing {
        fn definitions(&self) -> Vec<Arc<ToolDefinition>> {
            vec![]
        }
        async fn call(&mut self, _: &str, _: &str, _: &serde_json::Value) -> ToolOutcome {
            ToolOutcome::error("boom")
        }
    }
    let mut j = Journal::new(Failing);
    let out = j.call("i", "n", &serde_json::json!({})).await;
    assert!(out.is_error);
    let j2 = Journal::from_json(&j.to_json(), Failing).unwrap();
    let mut j2 = j2;
    assert_eq!(j2.call("i", "n", &serde_json::json!({})).await, out);
    assert_eq!(j2.hits(), 1);
}
