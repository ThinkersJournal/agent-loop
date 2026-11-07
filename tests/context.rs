// SPDX-License-Identifier: MIT OR Apache-2.0
//! Context policy: a model call sends a shortened, pairing-safe copy; the run's own history is untouched; a policy that
//! breaks pairing stops the drive before the model is called; a resume derives the same request.
use std::sync::Arc;

use agent_loop::{
    ContextPolicy, Driver, DriverError, End, Event, EventLog, KeepRecent, MemoryStore, Model,
    RunControl, ToolHost, ToolOutcome,
};
use rig_agent::run::RunSpec;
use rig_core::completion::{CompletionRequest, ProviderCapabilities, ToolDefinition};
use rig_core::error::ProviderError;
use rig_core::message::{Message, ToolName};
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

/// Three tool turns, then text: the model makes four calls.
fn four_calls() -> Mock {
    let call = |id: &str| {
        vec![
            E::tool_call(id, "step", serde_json::json!({})),
            E::final_response_with_total_tokens(1),
        ]
    };
    Mock(MockCompletionModel::from_stream_turns([
        call("c1"),
        call("c2"),
        call("c3"),
        vec![E::text("done"), E::final_response_with_total_tokens(1)],
    ]))
}

/// Each call answers with a distinct marker, so a request shows which results it carried.
#[derive(Default)]
struct Step {
    n: usize,
    /// Ask for a soft stop during this call number.
    stop_on: Option<(usize, RunControl)>,
}
impl ToolHost for Step {
    fn definitions(&self) -> Vec<Arc<ToolDefinition>> {
        vec![Arc::new(ToolDefinition::new(
            ToolName::new("step").unwrap(),
            "step",
            serde_json::json!({"type":"object","properties":{}}),
        ))]
    }
    async fn call(&mut self, _: &str, _: &str, _: &serde_json::Value) -> ToolOutcome {
        self.n += 1;
        if let Some((k, c)) = &self.stop_on
            && *k == self.n
        {
            c.stop_soft();
        }
        ToolOutcome::ok(format!("RESULT-{}", self.n))
    }
}

fn spec() -> RunSpec {
    let mut s = RunSpec::new();
    s.max_turns = Some(8);
    s
}

fn requests(m: &Mock) -> Vec<String> {
    m.0.requests().iter().map(|r| format!("{r:?}")).collect()
}

#[tokio::test]
async fn old_turns_are_left_out_of_the_request_but_stay_in_the_run() {
    // Control: without a policy the last request carries every result.
    let base = four_calls();
    let mut d = Driver::new(
        "s",
        base.clone(),
        Step::default(),
        MemoryStore::new(),
        spec(),
    );
    let mut run = d.open("TASK-PROMPT", 8).unwrap();
    d.drive(&mut run).await.unwrap();
    let full = requests(&base);
    assert!(full[3].contains("RESULT-1") && full[3].contains("RESULT-3"));

    let m = four_calls();
    let log = EventLog::new();
    let mut d = Driver::new("s", m.clone(), Step::default(), MemoryStore::new(), spec());
    d.set_context_policy(KeepRecent { head: 1, tail: 2 });
    d.set_sink(log.clone());
    let mut run = d.open("TASK-PROMPT", 8).unwrap();
    assert_eq!(d.drive(&mut run).await.unwrap(), End::Done("done".into()));
    let reqs = requests(&m);
    // Call 4: the task and the latest turn (call 3 and its result); turns 1 and 2 are left out.
    assert!(reqs[3].contains("TASK-PROMPT"), "the head is kept");
    assert!(reqs[3].contains("RESULT-3"));
    assert!(
        !reqs[3].contains("RESULT-1") && !reqs[3].contains("RESULT-2"),
        "{}",
        reqs[3]
    );
    // Turns 1 and 2 are short enough to send whole.
    assert!(reqs[2].contains("RESULT-2") && !reqs[2].contains("RESULT-1"));
    assert!(reqs[1].contains("RESULT-1"));
    // The run is complete and untrimmed.
    let history = serde_json::to_string(&run.full_history()).unwrap();
    assert!(
        history.contains("RESULT-1")
            && history.contains("RESULT-2")
            && history.contains("RESULT-3")
    );
    // One `Compacted` per shortened call, saying how much.
    let compacted: Vec<Event> = log
        .events()
        .into_iter()
        .filter(|e| matches!(e, Event::Compacted { .. }))
        .collect();
    assert_eq!(
        compacted,
        [
            Event::Compacted {
                dropped: 2,
                kept: 3
            },
            Event::Compacted {
                dropped: 4,
                kept: 3
            }
        ]
    );
}

/// Keeps the opening message and the final one, dropping the assistant call in between: an orphan tool result.
struct Orphans;
impl ContextPolicy for Orphans {
    fn shape(&mut self, t: &[Arc<Message>]) -> Option<Vec<Arc<Message>>> {
        (t.len() >= 3).then(|| vec![t[0].clone(), t[t.len() - 1].clone()])
    }
}

#[tokio::test]
async fn a_policy_that_breaks_pairing_stops_the_drive_before_the_model_is_called() {
    let m = four_calls();
    let mut d = Driver::new("s", m.clone(), Step::default(), MemoryStore::new(), spec());
    d.set_context_policy(Orphans);
    let mut run = d.open("TASK-PROMPT", 8).unwrap();
    let err = d.drive(&mut run).await.unwrap_err();
    assert!(
        matches!(&err, DriverError::Run(m) if m.contains("context policy")),
        "{err}"
    );
    // The first call has a 1-message transcript, so the policy leaves it alone and it is sent. The second has 3 messages, the
    // policy cuts out the assistant call, and the drive stops before that request reaches the model.
    assert_eq!(m.0.requests().len(), 1);
}

/// Drops everything, the prompt included.
struct DropsPrompt;
impl ContextPolicy for DropsPrompt {
    fn shape(&mut self, t: &[Arc<Message>]) -> Option<Vec<Arc<Message>>> {
        Some(t[..t.len() - 1].to_vec())
    }
}

#[tokio::test]
async fn a_policy_that_drops_the_prompt_is_an_error() {
    let m = four_calls();
    let mut d = Driver::new("s", m.clone(), Step::default(), MemoryStore::new(), spec());
    d.set_context_policy(DropsPrompt);
    let mut run = d.open("TASK-PROMPT", 8).unwrap();
    let err = d.drive(&mut run).await.unwrap_err();
    assert!(
        matches!(&err, DriverError::Run(m) if m.contains("prompt")),
        "{err}"
    );
    assert!(m.0.requests().is_empty());
}

#[tokio::test]
async fn a_resume_derives_the_same_shortened_request() {
    // Uninterrupted: the request for the 4th call.
    let m = four_calls();
    let mut d = Driver::new("s", m.clone(), Step::default(), MemoryStore::new(), spec());
    d.set_context_policy(KeepRecent { head: 1, tail: 2 });
    let mut run = d.open("TASK-PROMPT", 8).unwrap();
    d.drive(&mut run).await.unwrap();
    let want = requests(&m)[3].clone();

    // Interrupted: the third tool call asks for a stop, so the drive stops at the 4th boundary with the run saved.
    let mut d1 = Driver::new(
        "s",
        four_calls(),
        Step::default(),
        MemoryStore::new(),
        spec(),
    );
    d1.set_context_policy(KeepRecent { head: 1, tail: 2 });
    d1.tools.stop_on = Some((3, d1.control()));
    let mut r1 = d1.open("TASK-PROMPT", 8).unwrap();
    assert_eq!(d1.drive(&mut r1).await.unwrap(), End::Stopped);
    let store = d1.store;

    // A new driver resumes; only the 4th model call is left.
    let m2 = Mock(MockCompletionModel::from_stream_turns([vec![
        E::text("done"),
        E::final_response_with_total_tokens(1),
    ]]));
    let mut d2 = Driver::new("s", m2.clone(), Step::default(), store, spec());
    d2.set_context_policy(KeepRecent { head: 1, tail: 2 });
    let mut r2 = d2.open("ignored", 8).unwrap();
    assert_eq!(d2.drive(&mut r2).await.unwrap(), End::Done("done".into()));
    let got = &requests(&m2)[0];
    // Same shape: the task and the latest turn are present, the older turns are not, on both paths.
    for marker in ["TASK-PROMPT", "RESULT-3"] {
        assert!(want.contains(marker) && got.contains(marker), "{marker}");
    }
    for marker in ["RESULT-1", "RESULT-2"] {
        assert!(!want.contains(marker) && !got.contains(marker), "{marker}");
    }
}
