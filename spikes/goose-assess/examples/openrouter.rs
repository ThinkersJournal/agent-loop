//! Real-provider leg of spike 3 (Goose). Reads OPENROUTER_API_TOKEN from the environment (never printed),
//! optional OPENROUTER_MODEL (default: a free-tier tool-calling model).
//! Run:  cargo run --example openrouter
#![allow(dead_code, unused_imports)]
include!("../src/common.rs");

use goose_providers::api_client::{ApiClient, AuthMethod};
use goose_providers::openrouter::OpenRouterProvider;

const PROMPT: &str = "Use the add tool with x=17 and y=25, then reply with only the resulting number.";

fn machine<'a>(provider: Arc<dyn Provider>, model: &str, tool_calls: Arc<AtomicUsize>, cancel: CancellationToken)
    -> StateMachine<'a, Sess, Eff> {
    let steps: Vec<Step<'a, Sess, Eff>> = vec![
        Step::Operation(Arc::new(ToolOperation::<Sess>::new().with_provider(Arc::new(AddTool { calls: tool_calls })))),
        Step::Inference(Arc::new(InferenceRunner::<Sess, Eff>::new(provider, ModelConfig::new(model)))),
    ];
    StateMachine::new(steps, cancel)
}

fn kinds(json: &str) -> Vec<String> { shape(json) }

#[tokio::main]
async fn main() -> Result<()> {
    let key = match std::env::var("OPENROUTER_API_TOKEN") {
        Ok(k) if !k.is_empty() => k,
        _ => { eprintln!("OPENROUTER_API_TOKEN not set"); std::process::exit(2); }
    };
    let model = std::env::var("OPENROUTER_MODEL").unwrap_or_else(|_| "google/gemma-4-31b-it:free".into());
    println!("model={model} key_len_nonzero=true");
    let mk_provider = || -> Result<Arc<dyn Provider>> {
        let client = ApiClient::new_with_tls("https://openrouter.ai".into(), AuthMethod::BearerToken(key.clone()), None)?;
        Ok(Arc::new(OpenRouterProvider::new(client, None, None)))
    };

    // Leg 1: persisted before any model call (user message only); full run on a fresh machine; count stream events.
    let store = Arc::new(Store::default());
    *store.json.lock().unwrap() = serde_json::to_string(&Conversation::new_unvalidated([
        Message::user().with_text(PROMPT).with_generated_id()]))?;
    let tool_calls = Arc::new(AtomicUsize::new(0));
    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let em = Emitter::new(tx, cancel.clone());
    let res = machine(mk_provider()?, &model, tool_calls.clone(), cancel).run(&*store, "s", &em).await;
    drop(em);
    let mut events = 0;
    while rx.recv().await.is_some() { events += 1; }
    println!("leg1 run_ok={} events={events} tool_calls={} shape={:?} final={:?}", res.is_ok(),
        tool_calls.load(Ordering::SeqCst), kinds(&store.json.lock().unwrap()), final_text(&store.json.lock().unwrap()));
    if let Err(e) = &res { println!("leg1 error: {e}"); }

    // Leg 2: cancel on the first streamed message, persist, then resume on a fresh machine with no new user message.
    let store = Arc::new(Store::default());
    *store.json.lock().unwrap() = serde_json::to_string(&Conversation::new_unvalidated([
        Message::user().with_text(PROMPT).with_generated_id()]))?;
    let tool_calls = Arc::new(AtomicUsize::new(0));
    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let em = Emitter::new(tx, cancel.clone());
    let c2 = cancel.clone();
    let watcher = tokio::spawn(async move { if rx.recv().await.is_some() { c2.cancel(); } });
    let res = machine(mk_provider()?, &model, tool_calls.clone(), cancel).run(&*store, "s", &em).await;
    let _ = watcher.await;
    let cut = kinds(&store.json.lock().unwrap());
    println!("leg2 after cancel: run_ok={} tool_calls={} shape={:?}", res.is_ok(), tool_calls.load(Ordering::SeqCst), cut);
    let cancel = CancellationToken::new();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let em = Emitter::new(tx, cancel.clone());
    let res = machine(mk_provider()?, &model, tool_calls.clone(), cancel).run(&*store, "s", &em).await;
    println!("leg2 after resume: run_ok={} tool_calls={} shape={:?} final={:?}", res.is_ok(), tool_calls.load(Ordering::SeqCst),
        kinds(&store.json.lock().unwrap()), final_text(&store.json.lock().unwrap()));
    Ok(())
}
