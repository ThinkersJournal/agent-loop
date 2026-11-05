//! Tool hosts for the spike: a side-effecting tool and an idempotency journal around any host.
use std::collections::BTreeMap;
use std::sync::Arc;

use rig_core::completion::ToolDefinition;

use crate::driver::ToolHost;

/// The "world": a log of side effects. `effect` appends its tag; `add` is pure.
pub struct SideEffects {
    pub log: Vec<String>,
}
impl SideEffects {
    pub fn new() -> Self {
        Self { log: Vec::new() }
    }
}
pub fn defs() -> Vec<Arc<ToolDefinition>> {
    vec![
        Arc::new(ToolDefinition::new(
            rig_core::message::ToolName::new("add").unwrap(),
            "add x and y",
            serde_json::json!({"type":"object","properties":{"x":{"type":"number"},"y":{"type":"number"}},"required":["x","y"]}),
        )),
        Arc::new(ToolDefinition::new(
            rig_core::message::ToolName::new("effect").unwrap(),
            "performs a side effect tagged `tag`",
            serde_json::json!({"type":"object","properties":{"tag":{"type":"string"}},"required":["tag"]}),
        )),
    ]
}
impl ToolHost for SideEffects {
    fn defs(&self) -> Vec<Arc<ToolDefinition>> {
        defs()
    }
    fn exec(&mut self, _id: &str, name: &str, args: &serde_json::Value) -> String {
        match name {
            "add" => (args["x"].as_i64().unwrap_or(0) + args["y"].as_i64().unwrap_or(0)).to_string(),
            _ => {
                let tag = args["tag"].as_str().unwrap_or("?").to_string();
                self.log.push(tag.clone());
                format!("ran:{tag}")
            }
        }
    }
}

/// Idempotency journal keyed by tool-call id: returns the recorded result instead of re-running.
pub struct Journal<H: ToolHost> {
    pub inner: H,
    pub done: BTreeMap<String, String>,
    pub hits: usize,
}
impl<H: ToolHost> Journal<H> {
    pub fn new(inner: H) -> Self {
        Self { inner, done: BTreeMap::new(), hits: 0 }
    }
    pub fn save(&self) -> String {
        serde_json::to_string(&self.done).unwrap()
    }
    pub fn load(s: &str, inner: H) -> Self {
        Self { inner, done: serde_json::from_str(s).unwrap(), hits: 0 }
    }
}
impl<H: ToolHost> ToolHost for Journal<H> {
    fn defs(&self) -> Vec<Arc<ToolDefinition>> {
        self.inner.defs()
    }
    fn exec(&mut self, id: &str, name: &str, args: &serde_json::Value) -> String {
        if let Some(r) = self.done.get(id) {
            self.hits += 1;
            return r.clone();
        }
        let r = self.inner.exec(id, name, args);
        self.done.insert(id.to_string(), r.clone());
        r
    }
}
