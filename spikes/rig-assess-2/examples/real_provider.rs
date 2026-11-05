//! Item 7: the hand-rolled sans-I/O driver against a REAL provider (spike 4). The key is read from the
//! environment and never printed. Run it under with-secret, one provider at a time:
//!
//!   OpenRouter (free tier):  cargo run --example real_provider
//!       RP_MODEL=<model id>                 default google/gemma-4-31b-it:free
//!   Any OpenAI-shaped /chat/completions endpoint (OverMind's providers speak this; see docs/RIG-SPIKE-4.md):
//!       RP_PROVIDER=compat RP_BASE_URL=https://api.groq.com/openai/v1 RP_KEY_ENV=GROQ_CLOUD RP_MODEL=<id> \
//!         cargo run --example real_provider
//!
//! Legs: A streamed tool run; B cut mid-stream, persist, resume on a new connection; C abort mid-stream and
//! re-issue with a message patched in (stock path). Usage is printed from the run, not from the provider.
#[path = "../src/driver.rs"]
mod driver;
#[path = "../src/hosts.rs"]
mod hosts;

use driver::{Driver, Kill};
use hosts::SideEffects;
use rig_agent::run::{AgentRun, RunSpec};
use rig_core::message::Message;
use rig_core::providers::openai::{OpenAIConfig, Route};

const PROMPT: &str = "Use the add tool with x=17 and y=25, then reply with only the resulting number.";

fn spec() -> RunSpec {
    let mut s = RunSpec::new();
    s.max_turns = Some(5);
    s
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// A fresh connection every call, as after a restart.
fn open_fn(provider: &str, key: &str, model: &str, base: Option<&str>) -> (driver::OpenFn<'static>, rig_core::completion::ProviderCapabilities) {
    match provider {
        "compat" => {
            let cfg = OpenAIConfig::new(key.to_string()).with_base_url(base.expect("RP_BASE_URL")).with_route(Route::Chat);
            let m = cfg.client().completion(model);
            let caps = m.capabilities().completion;
            (Box::new(move |r| m.stream(r)), caps)
        }
        _ => {
            let m = rig_core::providers::openrouter::new(key.to_string()).chat(model);
            let caps = m.capabilities().completion;
            (Box::new(move |r| m.stream(r)), caps)
        }
    }
}

#[tokio::main]
async fn main() {
    let provider = env("RP_PROVIDER").unwrap_or_else(|| "openrouter".into());
    let key_env = env("RP_KEY_ENV").unwrap_or_else(|| "OPENROUTER_API_TOKEN".into());
    let Some(key) = env(&key_env) else {
        eprintln!("{key_env} not set");
        std::process::exit(2);
    };
    let model = env("RP_MODEL").unwrap_or_else(|| "google/gemma-4-31b-it:free".into());
    let base = env("RP_BASE_URL");
    println!("provider={provider} model={model} key_env={key_env} key_nonempty=true");

    // Leg A: streamed tool run, uninterrupted.
    let mut host = SideEffects::new();
    let (open, caps) = open_fn(&provider, &key, &model, base.as_deref());
    let mut d = Driver::with_open(open, caps, spec(), &mut host);
    let mut run = AgentRun::new(PROMPT).max_turns(5);
    let end = d.drive(&mut run, &Kill::default()).await;
    println!("A end={end:?}");
    println!("A stream items per attempt: {:?}", d.trace.iter().filter(|t| t.starts_with("item")).count());
    println!("A usage={:?} completion_calls={} history_msgs={}", run.usage(), run.completion_calls().len(), run.full_history().len());

    // Leg B: cut after 3 stream items, persist, resume on a NEW connection.
    let mut host = SideEffects::new();
    let (open, caps) = open_fn(&provider, &key, &model, base.as_deref());
    let mut d = Driver::with_open(open, caps, spec(), &mut host);
    let mut run = AgentRun::new(PROMPT).max_turns(5);
    let end = d.drive(&mut run, &Kill { after_stream_items: Some(3), ..Default::default() }).await;
    println!("B first life: {end:?}; trace={:?}", d.trace.last());
    let saved = serde_json::to_string(&run).unwrap();
    println!("B persisted {} bytes; completion_calls={} (cut attempt's usage is not recorded)", saved.len(), run.completion_calls().len());
    drop(d);
    let mut run: AgentRun = serde_json::from_str(&saved).unwrap();
    let mut host2 = SideEffects::new();
    let (open, caps) = open_fn(&provider, &key, &model, base.as_deref());
    let mut d2 = Driver::with_open(open, caps, spec(), &mut host2);
    let end = d2.drive(&mut run, &Kill::default()).await;
    println!("B resumed: {end:?} usage={:?} turn={}", run.usage(), run.turn());

    // Leg C: abort after 2 items and re-issue with a message patched in (stock RequestPatch.history path).
    let mut host = SideEffects::new();
    let (open, caps) = open_fn(&provider, &key, &model, base.as_deref());
    let mut d = Driver::with_open(open, caps, spec(), &mut host);
    d.inject = Some((2, Message::user("Also: after the number, add the word DONE.")));
    let mut run = AgentRun::new(PROMPT).max_turns(5);
    let end = d.drive(&mut run, &Kill::default()).await;
    println!("C end={end:?}");
    println!("C trace: {:?}", d.trace.iter().filter(|t| t.starts_with("aborted") || t.starts_with("sent")).collect::<Vec<_>>());
    println!("C saved run contains the injected text: {}", serde_json::to_string(&run).unwrap().contains("add the word DONE"));
}
