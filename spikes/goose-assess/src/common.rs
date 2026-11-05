// Spike 3: goose-agent 0.1.0-alpha.13 against the same three points as the Rig spike.
// Scripted provider (no network). Throwaway; not a dependency of anything.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use futures::StreamExt;
use goose_agent::inference::{InferenceEffect, InferenceRunner};
use goose_agent::machine::{EffectHandler, MachineSession, SessionLoader, StateMachine, Step};
use goose_agent::operation::{ConversationEffect, Emitter, MachineEffect};
use goose_agent::tool::{ToolOperation, ToolProvider};
use goose_provider_types::base::{MessageStream, Provider};
use goose_provider_types::conversation::message::{Message, MessageContent};
use goose_provider_types::conversation::token_usage::ProviderUsage;
use goose_provider_types::conversation::Conversation;
use goose_provider_types::errors::ProviderError;
use goose_provider_types::model::ModelConfig;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock, ErrorData, Tool};
use tokio_util::sync::CancellationToken;

// ---- effect type: goose-agent ships no InferenceEffect impl, the host supplies one ----
struct Eff(ConversationEffect);
impl MachineEffect for Eff {
    fn ensure_message_ids(&mut self) {
        self.0.ensure_message_ids()
    }
}
impl From<Message> for Eff {
    fn from(m: Message) -> Self {
        Eff(m.into())
    }
}
impl From<ConversationEffect> for Eff {
    fn from(e: ConversationEffect) -> Self {
        Eff(e)
    }
}
impl InferenceEffect for Eff {
    // usage is dropped here; a host that wants it persists it itself
    fn record_usage(_u: ProviderUsage) -> Self {
        Eff(ConversationEffect::SetMessageVisibility {
            message_id: String::new(),
            user_visible: true,
            agent_visible: true,
        })
    }
}

// ---- session + store: the persisted state is the Conversation as JSON text ----
#[derive(Clone)]
struct Sess {
    id: String,
    conv: Conversation,
}
impl MachineSession for Sess {
    fn id(&self) -> &str {
        &self.id
    }
    fn conversation(&self) -> Option<&Conversation> {
        Some(&self.conv)
    }
}
#[derive(Default)]
struct Store {
    json: Mutex<String>,
    applies: AtomicUsize,
    fail_apply_n: Mutex<Option<usize>>, // fail the Nth apply_effects call (1-based), once
}
#[async_trait]
impl SessionLoader<Sess> for Store {
    async fn load(&self, id: &str) -> Result<Sess> {
        let conv: Conversation = serde_json::from_str(&self.json.lock().unwrap())?;
        Ok(Sess { id: id.into(), conv })
    }
}
#[async_trait]
impl EffectHandler<Sess, Eff> for Store {
    async fn apply_effects(&self, s: &Sess, effects: &mut [Eff], _e: &Emitter) -> Result<()> {
        let n = self.applies.fetch_add(1, Ordering::SeqCst) + 1;
        if *self.fail_apply_n.lock().unwrap() == Some(n) {
            *self.fail_apply_n.lock().unwrap() = None;
            anyhow::bail!("injected crash before persisting apply #{n}");
        }
        let mut conv = s.conv.clone();
        for e in effects.iter() {
            if let ConversationEffect::AppendMessage(m) = &e.0 {
                conv.push(m.clone());
            }
        }
        *self.json.lock().unwrap() = serde_json::to_string(&conv)?;
        Ok(())
    }
}

// ---- scripted provider ----
enum It {
    Chunk(Message),
    Err(ProviderError),
    CancelHang,
}
struct Scripted {
    scripts: Mutex<VecDeque<Vec<It>>>,
    calls: AtomicUsize,
    cancel: CancellationToken,
}
#[async_trait]
impl Provider for Scripted {
    fn get_name(&self) -> &str {
        "scripted"
    }
    async fn stream(
        &self,
        _m: &ModelConfig,
        _s: &str,
        _msgs: &[Message],
        _t: &[Tool],
    ) -> Result<MessageStream, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let items = self.scripts.lock().unwrap().pop_front().expect("script exhausted");
        let tok = self.cancel.clone();
        Ok(Box::pin(futures::stream::iter(items).then(move |it| {
            let tok = tok.clone();
            async move {
                match it {
                    It::Chunk(m) => Ok((Some(m), None)),
                    It::Err(e) => Err(e),
                    It::CancelHang => {
                        tok.cancel();
                        futures::future::pending().await
                    }
                }
            }
        })))
    }
}

// ---- tool ----
struct AddTool {
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl ToolProvider<Sess> for AddTool {
    async fn tools(&self, _s: &Sess) -> Result<Vec<Tool>> {
        let schema = serde_json::json!({"type":"object","properties":{"x":{"type":"integer"},"y":{"type":"integer"}},"required":["x","y"]});
        Ok(vec![Tool::new(
            "add",
            "add two integers",
            Arc::new(schema.as_object().unwrap().clone()),
        )])
    }
    async fn call(
        &self,
        _s: &Sess,
        _id: &str,
        call: CallToolRequestParams,
        _e: &Emitter,
    ) -> Result<CallToolResult, ErrorData> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let a = call.arguments.unwrap_or_default();
        let sum = a["x"].as_i64().unwrap_or(0) + a["y"].as_i64().unwrap_or(0);
        Ok(CallToolResult::success(vec![ContentBlock::text(sum.to_string())]))
    }
}
fn tool_turn() -> Vec<It> {
    let call = CallToolRequestParams::new("add")
        .with_arguments(serde_json::json!({"x":1,"y":2}).as_object().unwrap().clone());
    vec![It::Chunk(Message::assistant().with_tool_request("call_1", Ok(call)))]
}
fn text_turn(parts: &[&str]) -> Vec<It> {
    parts
        .iter()
        .map(|p| It::Chunk(Message::assistant().with_text(*p)))
        .collect()
}

struct Rig {
    store: Arc<Store>,
    provider: Arc<Scripted>,
    tool_calls: Arc<AtomicUsize>,
    cancel: CancellationToken,
}
fn rig(store: Arc<Store>, scripts: Vec<Vec<It>>) -> Rig {
    let cancel = CancellationToken::new();
    Rig {
        store,
        provider: Arc::new(Scripted {
            scripts: Mutex::new(scripts.into()),
            calls: AtomicUsize::new(0),
            cancel: cancel.clone(),
        }),
        tool_calls: Arc::new(AtomicUsize::new(0)),
        cancel,
    }
}
impl Rig {
    fn machine(&self) -> StateMachine<'_, Sess, Eff> {
        let steps: Vec<Step<'_, Sess, Eff>> = vec![
            Step::Operation(Arc::new(
                ToolOperation::<Sess>::new().with_provider(Arc::new(AddTool {
                    calls: self.tool_calls.clone(),
                })),
            )),
            Step::Inference(Arc::new(InferenceRunner::<Sess, Eff>::new(
                self.provider.clone(),
                ModelConfig::new("scripted-model"),
            ))),
        ];
        StateMachine::new(steps, self.cancel.clone())
    }
    fn emitter(
        &self,
    ) -> (
        Emitter,
        tokio::sync::mpsc::UnboundedReceiver<goose_agent::events::AgentEvent>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (Emitter::new(tx, self.cancel.clone()), rx)
    }
}
fn seed(store: &Store) {
    let c = Conversation::new_unvalidated([Message::user()
        .with_text("add 1 and 2")
        .with_generated_id()]);
    *store.json.lock().unwrap() = serde_json::to_string(&c).unwrap();
}
fn shape(json: &str) -> Vec<String> {
    let c: Conversation = serde_json::from_str(json).unwrap();
    c.messages()
        .iter()
        .map(|m| {
            let kinds: Vec<&str> = m
                .content
                .iter()
                .map(|b| match b {
                    MessageContent::Text(_) => "text",
                    MessageContent::ToolRequest(_) => "tool_req",
                    MessageContent::ToolResponse(_) => "tool_resp",
                    _ => "other",
                })
                .collect();
            format!(
                "{:?}:{}{}",
                m.role,
                kinds.join("+"),
                if m.error_kind().is_some() { "(ERROR)" } else { "" }
            )
        })
        .collect()
}
fn final_text(json: &str) -> String {
    let c: Conversation = serde_json::from_str(json).unwrap();
    c.messages()
        .iter()
        .rev()
        .find_map(|m| {
            m.content.iter().find_map(|b| match b {
                MessageContent::Text(t) => Some(t.text.clone()),
                _ => None,
            })
        })
        .unwrap_or_default()
}

