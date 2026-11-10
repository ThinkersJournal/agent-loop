// SPDX-License-Identifier: MIT OR Apache-2.0
//! Summarising dropped turns: a separate model call replaces what the cut leaves out, it is cached and refreshed only when
//! `every` messages have newly fallen out, the run's own history stays whole, and a failed summary stops the drive.
use std::sync::Arc;

use agent_loop::{
    Driver, DriverError, End, Event, EventLog, MemoryStore, Model, Summarise, ToolHost, ToolOutcome,
};
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

#[derive(Default)]
struct Step(usize);
impl ToolHost for Step {
    fn definitions(&self) -> Vec<Arc<ToolDefinition>> {
        vec![Arc::new(ToolDefinition::new(
            ToolName::new("step").unwrap(),
            "step",
            serde_json::json!({"type":"object","properties":{}}),
        ))]
    }
    async fn call(&mut self, _: &str, _: &str, _: &serde_json::Value) -> ToolOutcome {
        self.0 += 1;
        ToolOutcome::ok(format!("RESULT-{}", self.0))
    }
}

fn tool(id: &str) -> Vec<E> {
    vec![
        E::tool_call(id, "step", serde_json::json!({})),
        E::final_response_with_total_tokens(1),
    ]
}
fn text(t: &str) -> Vec<E> {
    vec![E::text(t), E::final_response_with_total_tokens(1)]
}

fn spec() -> agent_loop::RunConfig {
    agent_loop::RunConfig::new().max_turns(8)
}

fn requests(m: &Mock) -> Vec<String> {
    m.0.requests().iter().map(|r| format!("{r:?}")).collect()
}

async fn run(m: &Mock, every: usize) -> (Result<End, DriverError>, agent_loop::Run, EventLog) {
    let log = EventLog::new();
    let mut d = Driver::new("s", m.clone(), Step::default(), MemoryStore::new(), spec());
    d.set_summariser(Summarise {
        head: 1,
        tail: 2,
        every,
    });
    d.set_sink(log.clone());
    let mut r = d.open("TASK-PROMPT", 8).unwrap();
    let end = d.drive(&mut r).await;
    (end, r, log)
}

#[tokio::test]
async fn dropped_turns_become_a_summary_that_is_refreshed_as_more_fall_out() {
    // Model calls in order: call 1, call 2, summary 1 (before call 3), call 3, summary 2 (before call 4), call 4.
    let m = Mock(MockCompletionModel::from_stream_turns([
        tool("c1"),
        tool("c2"),
        text("SUMMARY-ONE"),
        tool("c3"),
        text("SUMMARY-TWO"),
        text("done"),
    ]));
    let (end, r, log) = run(&m, 1).await;
    assert_eq!(end.unwrap(), End::Done("done".into()));
    let q = requests(&m);
    assert_eq!(q.len(), 6);

    // Calls 1 and 2 are short enough to send whole.
    assert!(!q[0].contains("SUMMARY") && !q[1].contains("SUMMARY"));
    // Summary 1 sees the dropped turn (call 1 and its result) and no tools.
    assert!(
        q[2].contains("Excerpt") && q[2].contains("RESULT-1"),
        "{}",
        q[2]
    );
    assert!(!q[2].contains("RESULT-2"));
    // Call 3 sends the task, the summary and the latest turn; the dropped result is gone.
    assert!(
        q[3].contains("TASK-PROMPT") && q[3].contains("SUMMARY-ONE"),
        "{}",
        q[3]
    );
    assert!(
        q[3].contains("RESULT-2") && !q[3].contains("RESULT-1"),
        "{}",
        q[3]
    );
    // Summary 2 is built from the old summary plus only the newly dropped turn.
    assert!(
        q[4].contains("SUMMARY-ONE") && q[4].contains("RESULT-2") && !q[4].contains("RESULT-1"),
        "{}",
        q[4]
    );
    // Call 4 carries the new summary in place of the old one.
    assert!(
        q[5].contains("SUMMARY-TWO") && !q[5].contains("SUMMARY-ONE"),
        "{}",
        q[5]
    );
    assert!(
        q[5].contains("RESULT-3") && !q[5].contains("RESULT-2"),
        "{}",
        q[5]
    );

    // The run's own history is whole and never holds a summary.
    let h = r.history_json().unwrap();
    assert!(h.contains("RESULT-1") && h.contains("RESULT-2") && h.contains("RESULT-3"));
    assert!(!h.contains("SUMMARY"));

    // Each summary call is counted as usage, and each shortened call is a `Compacted`.
    let ev = log.events();
    let usage = ev
        .iter()
        .filter(|e| matches!(e, Event::Usage { .. }))
        .count();
    let compacted = ev
        .iter()
        .filter(|e| matches!(e, Event::Compacted { .. }))
        .count();
    assert_eq!((usage, compacted), (6, 2));
}

#[tokio::test]
async fn a_larger_every_sends_unsummarised_messages_instead_of_calling_again() {
    // One summary only: call 4 reuses it and sends the turn since as it is.
    let m = Mock(MockCompletionModel::from_stream_turns([
        tool("c1"),
        tool("c2"),
        text("SUMMARY-ONE"),
        tool("c3"),
        text("done"),
    ]));
    let (end, _r, _log) = run(&m, 10).await;
    assert_eq!(end.unwrap(), End::Done("done".into()));
    let q = requests(&m);
    assert_eq!(q.len(), 5, "no second summary call");
    assert!(
        q[4].contains("SUMMARY-ONE") && q[4].contains("RESULT-2") && q[4].contains("RESULT-3"),
        "{}",
        q[4]
    );
    assert!(!q[4].contains("RESULT-1"), "{}", q[4]);
}

#[tokio::test]
async fn a_summary_with_no_text_stops_the_drive_before_the_call_is_made() {
    // The summary call returns nothing: the drive fails and call 3 is never sent.
    let m = Mock(MockCompletionModel::from_stream_turns([
        tool("c1"),
        tool("c2"),
        vec![E::final_response_with_total_tokens(1)],
    ]));
    let (end, _r, _log) = run(&m, 1).await;
    let err = end.unwrap_err();
    assert!(
        matches!(&err, DriverError::Run(s) if s.contains("summariser")),
        "{err}"
    );
    assert_eq!(requests(&m).len(), 3, "calls 1, 2 and the summary only");
}
