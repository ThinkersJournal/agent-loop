//! Real-provider leg of spike 2. Reads OPENROUTER_API_TOKEN from the environment (never printed),
//! optional OPENROUTER_MODEL (default: a free-tier tool-calling model).
//! Run:  cargo run --example openrouter
use futures::StreamExt;
use rig_agent::agent::MultiTurnStreamItem;
use rig_agent::run::AgentRun;
use rig_agent::test_utils::MockAddTool;
use rig_agent::AgentBuilder;
use rig_core::providers::openrouter;

const PROMPT: &str = "Use the add tool with x=17 and y=25, then reply with only the resulting number.";

fn build(key: &str, model: &str) -> rig_agent::Agent {
    let m = openrouter::new(key.to_string()).chat(model);
    AgentBuilder::new(m).tool(MockAddTool).build()
}

#[tokio::main]
async fn main() {
    let key = match std::env::var("OPENROUTER_API_TOKEN") {
        Ok(k) if !k.is_empty() => k,
        _ => { eprintln!("OPENROUTER_API_TOKEN not set"); std::process::exit(2); }
    };
    let model = std::env::var("OPENROUTER_MODEL").unwrap_or_else(|_| "google/gemma-4-31b-it:free".into());
    println!("model={model} key_len_nonzero=true");

    // The run persisted BEFORE any model call (CallModel emitted), as JSON text.
    let mut run = AgentRun::new(PROMPT).max_turns(5);
    let _ = run.next_step().expect("first step");
    let saved = serde_json::to_string(&run).expect("serialise");
    println!("persisted {} bytes at CallModel-emitted", saved.len());

    // Leg A: non-streaming, resumed from the persisted run (fresh agent, as after a restart).
    let a = build(&key, &model);
    match a.resume(serde_json::from_str::<AgentRun>(&saved).unwrap()).await {
        Ok(r) => println!("A non-streaming resume: output={:?} messages={} usage={:?}", r.output(), r.messages.len(), r.usage),
        Err(e) => println!("A non-streaming resume: ERROR {e}"),
    }

    // Leg B: streaming, resumed from the same persisted run; count item kinds.
    let b = build(&key, &model);
    let mut s = b.resume(serde_json::from_str::<AgentRun>(&saved).unwrap()).stream();
    let (mut text, mut args, mut calls, mut results) = (0, 0, 0, 0);
    let mut fin = None;
    while let Some(item) = s.next().await {
        match item {
            Ok(MultiTurnStreamItem::StreamAssistantItem(i)) => {
                let d = format!("{i:?}");
                if d.contains("Text {") { text += 1 } else if d.contains("Arguments {") { args += 1 }
            }
            Ok(MultiTurnStreamItem::ToolCall { .. }) => calls += 1,
            Ok(MultiTurnStreamItem::ToolResult { .. }) => results += 1,
            Ok(MultiTurnStreamItem::FinalResponse(r)) => fin = Some((r.output(), r.usage())),
            Ok(_) => {}
            Err(e) => { println!("B streaming: ERROR {e}"); break; }
        }
    }
    println!("B streaming resume: text_deltas={text} arg_deltas={args} tool_calls={calls} tool_results={results} final={fin:?}");

    // Leg C: kill mid-stream (drop after the first stream item), then resume the pre-call persisted run.
    let c = build(&key, &model);
    let mut s = c.resume(serde_json::from_str::<AgentRun>(&saved).unwrap()).stream();
    let first = s.next().await.map(|i| i.is_ok());
    drop(s);
    println!("C killed after first item (ok={first:?}); resuming persisted run");
    let c2 = build(&key, &model);
    match c2.resume(serde_json::from_str::<AgentRun>(&saved).unwrap()).await {
        Ok(r) => println!("C recovered: output={:?}", r.output()),
        Err(e) => println!("C recovered: ERROR {e}"),
    }
}
