// SPDX-License-Identifier: MIT OR Apache-2.0
//! `RunConfig` and `Run` are our own types: the settings reach the model request, the budget is enforced, and the run handle
//! reports its history, all without this file naming a rig-agent type.
use std::sync::Arc;

use agent_loop::{Driver, End, MemoryStore, Model, RunConfig, ToolHost, ToolOutcome};
use rig_core::completion::{CompletionRequest, ProviderCapabilities, ToolDefinition};
use rig_core::error::ProviderError;
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

struct NoTools;
impl ToolHost for NoTools {
    fn definitions(&self) -> Vec<Arc<ToolDefinition>> {
        vec![]
    }
    async fn call(&mut self, _: &str, _: &str, _: &serde_json::Value) -> ToolOutcome {
        ToolOutcome::error("no tools")
    }
}

fn answer() -> Mock {
    Mock(MockCompletionModel::from_stream_turns([vec![
        E::text("done"),
        E::final_response_with_total_tokens(1),
    ]]))
}

#[tokio::test]
async fn config_settings_reach_the_request_and_a_default_config_sends_none() {
    let m = answer();
    let cfg = RunConfig::new()
        .preamble("YOU-ARE-A-TEST-PREAMBLE")
        .max_turns(3)
        .max_tokens(777)
        .temperature(0.25)
        .additional_params(serde_json::json!({"marker": "EXTRA-PARAM"}));
    let mut d = Driver::new("s", m.clone(), NoTools, MemoryStore::new(), cfg);
    let mut run = d.open("q", 3).unwrap();
    assert_eq!(d.drive(&mut run).await.unwrap(), End::Done("done".into()));
    let req = format!("{:?}", m.0.requests()[0]);
    for want in ["YOU-ARE-A-TEST-PREAMBLE", "777", "0.25", "EXTRA-PARAM"] {
        assert!(req.contains(want), "{want} missing from {req}");
    }

    // Control: the same driver with a default config sends none of them.
    let m = answer();
    let mut d = Driver::new(
        "s",
        m.clone(),
        NoTools,
        MemoryStore::new(),
        RunConfig::new(),
    );
    let mut run = d.open("q", 3).unwrap();
    d.drive(&mut run).await.unwrap();
    let req = format!("{:?}", m.0.requests()[0]);
    for absent in ["YOU-ARE-A-TEST-PREAMBLE", "777", "EXTRA-PARAM"] {
        assert!(!req.contains(absent), "{absent} present in {req}");
    }
}

#[tokio::test]
async fn the_run_handle_reports_its_history() {
    let mut d = Driver::new(
        "s",
        answer(),
        NoTools,
        MemoryStore::new(),
        RunConfig::new().max_turns(2),
    );
    let mut run = d.open("the question", 2).unwrap();
    d.drive(&mut run).await.unwrap();
    // The question and the answer.
    assert_eq!(run.history_len(), 2);
    let json = run.history_json().unwrap();
    assert!(
        json.contains("the question") && json.contains("done"),
        "{json}"
    );
}

#[test]
fn run_config_is_plain_data() {
    let a = RunConfig::new().max_turns(4).preamble("x");
    assert_eq!(a.clone(), a);
    assert_ne!(a, RunConfig::new());
}
