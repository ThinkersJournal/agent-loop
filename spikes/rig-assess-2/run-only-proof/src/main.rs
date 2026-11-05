//! Proof that the spike's driver (src/driver.rs, unchanged) runs on the extracted `run` module + rig-core only.
//! No rig-agent engine, hooks, tool server or rig-memory in this build.
#[path = "../../src/driver.rs"]
mod driver;
#[path = "../../src/hosts.rs"]
mod hosts;

use driver::{Driver, Kill};
use hosts::SideEffects;
use rig_agent::run::{AgentRun, RunSpec};
use rig_core::test_utils::{MockCompletionModel, MockStreamEvent as E};

fn spec() -> RunSpec {
    let mut s = RunSpec::new();
    s.max_turns = Some(3);
    s
}
fn model() -> MockCompletionModel {
    MockCompletionModel::from_stream_turns([
        vec![
            E::tool_call("ca", "effect", serde_json::json!({"tag":"A"})),
            E::tool_call("cb", "effect", serde_json::json!({"tag":"B"})),
            E::final_response_with_total_tokens(3),
        ],
        vec![E::text("all done"), E::final_response_with_total_tokens(1)],
    ])
}

#[tokio::main]
async fn main() {
    // cut mid-stream, persist, resume
    let mut host = SideEffects::new();
    let mut d = Driver::new(model(), spec(), &mut host);
    let mut run = AgentRun::new("do A and B").max_turns(3);
    let end = d.drive(&mut run, &Kill { after_stream_items: Some(1), ..Default::default() }).await;
    println!("cut: {end:?}");
    let saved = serde_json::to_string(&run).unwrap();
    drop(d);
    let mut run: AgentRun = serde_json::from_str(&saved).unwrap();
    let mut host = SideEffects::new();
    let mut d = Driver::new(model(), spec(), &mut host);
    let end = d.drive(&mut run, &Kill::default()).await;
    println!("resumed: {end:?}");
    // crash between tools, resume: only the unanswered call re-executes
    let mut host = SideEffects::new();
    let mut d = Driver::new(model(), spec(), &mut host);
    let mut run = AgentRun::new("do A and B").max_turns(3);
    let end = d.drive(&mut run, &Kill { after_answers: Some(1), ..Default::default() }).await;
    println!("crash between tools: {end:?}");
    let saved = serde_json::to_string(&run).unwrap();
    drop(d);
    let first_log = host.log.clone();
    let mut run: AgentRun = serde_json::from_str(&saved).unwrap();
    let mut host2 = SideEffects::new();
    let m = MockCompletionModel::from_stream_turns([vec![E::text("all done"), E::final_response_with_total_tokens(1)]]);
    let mut d2 = Driver::new(m, spec(), &mut host2);
    let end = d2.drive(&mut run, &Kill::default()).await;
    drop(d2);
    println!("resumed: {end:?}; first life ran {first_log:?}, second life ran {:?}", host2.log);
}
