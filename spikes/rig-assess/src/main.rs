//! Spike 2: rig-agent 0.46.0 assessment. Throwaway; nothing here is a library choice.
use futures::StreamExt;
use rig_agent::agent::MultiTurnStreamItem;
use rig_agent::run::AgentRun;
use rig_agent::AgentBuilder;
use rig_agent::test_utils::{MockAddTool, MockCompletionModel, MockStreamEvent, MockTurn};

fn blocking_model() -> MockCompletionModel {
    MockCompletionModel::from_turns([
        MockTurn::tool_call("c1", "add", serde_json::json!({"x":1,"y":2})),
        MockTurn::text("3"),
    ])
}
fn stream_model() -> MockCompletionModel {
    MockCompletionModel::from_stream_turns([
        vec![
            MockStreamEvent::tool_call_name_delta("t1", "add"),
            MockStreamEvent::tool_call_arguments_delta("t1", "{\"x\":1,"),
            MockStreamEvent::tool_call_arguments_delta("t1", "\"y\":2}"),
            MockStreamEvent::tool_call_end("t1"),
            MockStreamEvent::final_response_with_total_tokens(3),
        ],
        vec![
            MockStreamEvent::text("3"),
            MockStreamEvent::final_response_with_total_tokens(1),
        ],
    ])
}

#[tokio::main]
async fn main() {
    // ---- Leg 1: persist after CallModel was emitted (the re-issue case) ----
    let agent = AgentBuilder::new(blocking_model()).tool(MockAddTool).build();
    let control = agent.prompt("add 1 and 2").max_turns(3).await.expect("control");
    println!("L1 control: text={:?} usage={:?} msgs={}", control.output(), control.usage, control.messages.len());

    let mut run = AgentRun::new("add 1 and 2").max_turns(3);
    let step = run.next_step().expect("step");
    println!("L1 first step: {}", step_name(&step));
    let saved = serde_json::to_string(&run).expect("ser");
    println!("L1 persisted {} bytes at CallModel-emitted", saved.len());
    let reloaded: AgentRun = serde_json::from_str(&saved).expect("de");
    assert_eq!(serde_json::to_string(&reloaded).unwrap(), saved, "re-serializes byte-identical");
    // fresh model (the first model call is "dropped": new process)
    let agent2 = AgentBuilder::new(blocking_model()).tool(MockAddTool).build();
    let resumed = agent2.resume(reloaded).await.expect("resume");
    println!("L1 resumed: text={:?} usage={:?} msgs={} turn_equal={}", resumed.output(), resumed.usage, resumed.messages.len(),
        serde_json::to_string(&resumed.messages).unwrap() == serde_json::to_string(&control.messages).unwrap());
    // re-issue is idempotent and does not consume a turn
    let mut r2: AgentRun = serde_json::from_str(&saved).unwrap();
    let a = r2.next_step().unwrap();
    let b = r2.next_step().unwrap();
    println!("L1 re-issue: {} then {} turn_counter={}", step_name(&a), step_name(&b), r2.turn());

    // ---- Leg 2: streaming ----
    let sagent = AgentBuilder::new(stream_model()).tool(MockAddTool).build();
    let mut s = sagent.prompt("add 1 and 2").max_turns(3).stream();
    let mut kinds = Vec::new();
    let mut final_text = None;
    while let Some(item) = s.next().await {
        match item {
            Ok(MultiTurnStreamItem::StreamAssistantItem(i)) => kinds.push(format!("assistant:{:?}", short(&format!("{i:?}")))),
            Ok(MultiTurnStreamItem::ToolCall { .. }) => kinds.push("ToolCall".into()),
            Ok(MultiTurnStreamItem::ToolResult { .. }) => kinds.push("ToolResult".into()),
            Ok(MultiTurnStreamItem::FinalResponse(r)) => { final_text = Some(r.output().clone()); kinds.push("Final".into()) }
            Ok(other) => kinds.push(format!("other:{}", short(&format!("{other:?}")))),
            Err(e) => kinds.push(format!("ERR:{e}")),
        }
    }
    println!("L2 stream items ({}): {:#?}", kinds.len(), kinds);
    println!("L2 final={final_text:?}");

    // streamed resume from persisted CallModel
    let reloaded: AgentRun = serde_json::from_str(&saved).unwrap();
    let sagent2 = AgentBuilder::new(stream_model()).tool(MockAddTool).build();
    let mut s2 = sagent2.resume(reloaded).stream();
    let mut fin = None;
    while let Some(item) = s2.next().await {
        if let Ok(MultiTurnStreamItem::FinalResponse(r)) = item { fin = Some(r.output().clone()); }
    }
    println!("L2 streamed resume final={fin:?}");
    leg3(&saved).await;
}

struct StopOnText;
impl rig_agent::agent::AgentHook for StopOnText {
    async fn on_text_delta(&self, _c: &rig_agent::agent::HookContext, _e: rig_agent::agent::TextDelta<'_>) -> rig_agent::agent::ObservationAction {
        rig_agent::agent::ObservationAction::Stop("host soft stop".into())
    }
}

async fn leg3(saved: &str) {
    // 3a: hook stops mid-stream (during turn 2 text; turn 1 tool already ran)
    let a = AgentBuilder::new(stream_model()).tool(MockAddTool).build();
    let mut s = a.prompt("add 1 and 2").max_turns(3).add_hook(StopOnText).stream();
    let mut last = String::new();
    while let Some(i) = s.next().await {
        match i { Ok(_) => {}, Err(e) => { last = format!("{e:?}"); } }
    }
    println!("L3a stop error: {}", short(&last));
    if let Some(i) = last.find("history") { println!("L3a error carries history: yes (at {i})"); }
    // 3b: stream cut by transport error mid-turn; recover by resuming the pre-call persisted run
    let cut = MockCompletionModel::from_stream_turns([vec![
        MockStreamEvent::text("par"),
        MockStreamEvent::error("connection reset"),
    ]]);
    let a = AgentBuilder::new(cut).tool(MockAddTool).build();
    let mut s = a.resume(serde_json::from_str(saved).unwrap()).stream();
    let mut err = None;
    while let Some(i) = s.next().await { if let Err(e) = i { err = Some(format!("{e:?}")); } }
    println!("L3b cut stream error: {}", short(&err.unwrap_or("NONE".into())));
    let a = AgentBuilder::new(stream_model()).tool(MockAddTool).build();
    let mut s = a.resume(serde_json::from_str(saved).unwrap()).stream();
    let mut fin = None;
    while let Some(i) = s.next().await { if let Ok(MultiTurnStreamItem::FinalResponse(r)) = i { fin = Some(r.output()); } }
    println!("L3b recovered from pre-call persisted run: final={fin:?}");
}

fn step_name(s: &rig_agent::run::AgentRunStep) -> &'static str {
    use rig_agent::run::AgentRunStep::*;
    match s { CallModel{..} => "CallModel", CallTools{..} => "CallTools", Done(_) => "Done" }
}
fn short(s: &str) -> String { s.chars().take(90).collect() }
