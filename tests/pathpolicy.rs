// SPDX-License-Identifier: MIT OR Apache-2.0
//! `PathPolicy`: denied paths are refused before the tool runs, matching is on whole components and ignores case, odd shapes fail
//! closed, a malformed file is an error, and a real drive shows the model the reason without the tool being called.
use std::sync::{Arc, Mutex};

use agent_loop::{
    Driver, End, Hook, MemoryStore, Model, PathPolicy, PathPolicyError, ToolCall, ToolDecision,
    ToolHost, ToolOutcome,
};
use rig_core::completion::{CompletionRequest, ProviderCapabilities, ToolDefinition};
use rig_core::error::ProviderError;
use rig_core::message::ToolName;
use rig_core::streaming::CompletionStream;
use rig_core::test_utils::{MockCompletionModel, MockStreamEvent as E};

const RULES: &str = r#"
[workspace]
mode = "copy-back"

[[path]]
deny = [".git", ".github", "CLAUDE.md", "tools/hooks"]
justification = "a write here changes what runs later"
"#;

fn policy() -> PathPolicy {
    PathPolicy::from_toml(RULES).unwrap()
}

fn decide(p: &mut PathPolicy, args: serde_json::Value) -> ToolDecision {
    p.before_tool(ToolCall {
        id: "c",
        name: "edit",
        args: &args,
    })
}

#[test]
fn denied_paths_are_refused_with_the_justification_and_others_are_allowed() {
    let mut p = policy();
    for bad in [
        ".git/config",
        ".git",
        ".GIT/HEAD",
        "./.github/workflows/ci.yml",
        ".git\\hooks\\pre-commit",
        "CLAUDE.md",
        "claude.md",
        "tools/hooks/x.sh",
    ] {
        let d = decide(&mut p, serde_json::json!({"path": bad}));
        assert!(
            matches!(&d, ToolDecision::Deny(m) if m.contains("a write here changes what runs later")),
            "{bad}: {d:?}"
        );
    }
    // Controls: whole components only, and other paths pass.
    for ok in [
        "src/main.rs",
        ".gitignore",
        "src/.git/x",
        "CLAUDE.md.bak",
        "tools/hooksy/x",
        "tools",
        "docs/CLAUDE.md",
    ] {
        assert_eq!(
            decide(&mut p, serde_json::json!({"path": ok})),
            ToolDecision::Allow,
            "{ok}"
        );
    }
}

#[test]
fn paths_are_found_in_arrays_and_nested_arguments_under_the_known_names() {
    let mut p = policy();
    let deny = |v| matches!(decide(&mut policy(), v), ToolDecision::Deny(_));
    assert!(deny(serde_json::json!({"paths": ["a.rs", ".git/config"]})));
    assert!(deny(
        serde_json::json!({"edits": [{"file_path": ".github/x", "text": "t"}]})
    ));
    assert!(deny(serde_json::json!({"from": "a.rs", "to": ".git/x"})));
    // A path under an unlisted name is not covered, and a non-path string is not read as one.
    assert_eq!(
        decide(&mut p, serde_json::json!({"target": ".git/config"})),
        ToolDecision::Allow
    );
    assert_eq!(
        decide(&mut p, serde_json::json!({"text": ".git/config"})),
        ToolDecision::Allow
    );
    // `with_keys` adds a name.
    let mut q = policy().with_keys(["target"]);
    assert!(matches!(
        decide(&mut q, serde_json::json!({"target": ".git/config"})),
        ToolDecision::Deny(_)
    ));
}

#[test]
fn absolute_and_escaping_paths_are_denied_whatever_the_rules_say() {
    // No rules at all: the shape check still applies.
    let mut p = PathPolicy::from_toml("").unwrap();
    for bad in [
        "/etc/passwd",
        "\\\\srv\\x",
        "C:/x",
        "c:\\x",
        "../x",
        "a/../../x",
    ] {
        let d = decide(&mut p, serde_json::json!({"path": bad}));
        assert!(
            matches!(&d, ToolDecision::Deny(m) if m.contains("outside the workspace")),
            "{bad}: {d:?}"
        );
    }
    assert_eq!(
        decide(&mut p, serde_json::json!({"path": "src/a.rs"})),
        ToolDecision::Allow
    );
}

#[test]
fn a_malformed_file_is_an_error_not_an_empty_rule_set() {
    let bad = |t: &str| PathPolicy::from_toml(t).err();
    assert!(matches!(
        bad("[[path]]\ndeny = [\".git\"]"),
        Some(PathPolicyError::Toml(_))
    ));
    assert!(matches!(
        bad("[[path]]\ndeny = [\".git\"]\njustification = \"x\"\nallow = [\"a\"]"),
        Some(PathPolicyError::Toml(_))
    ));
    assert!(matches!(
        bad("[[path]]\ndeny = []\njustification = \"x\""),
        Some(PathPolicyError::Rule(_))
    ));
    assert!(matches!(
        bad("[[path]]\ndeny = [\"\"]\njustification = \"x\""),
        Some(PathPolicyError::Rule(_))
    ));
    assert!(matches!(
        bad("[[path]]\ndeny = [\"/abs\"]\njustification = \"x\""),
        Some(PathPolicyError::Rule(_))
    ));
    assert!(matches!(
        bad("[[path]]\ndeny = [\".git\"]\njustification = \"  \""),
        Some(PathPolicyError::Rule(_))
    ));
    // Control: the well-formed file loads, and other tables are ignored.
    assert!(PathPolicy::from_toml(RULES).is_ok());
}

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

struct Edit(Arc<Mutex<Vec<String>>>);
impl ToolHost for Edit {
    fn definitions(&self) -> Vec<Arc<ToolDefinition>> {
        vec![Arc::new(ToolDefinition::new(
            ToolName::new("edit").unwrap(),
            "edit",
            serde_json::json!({"type":"object","properties":{"path":{"type":"string"}}}),
        ))]
    }
    async fn call(&mut self, _: &str, _: &str, args: &serde_json::Value) -> ToolOutcome {
        self.0
            .lock()
            .unwrap()
            .push(args["path"].as_str().unwrap().into());
        ToolOutcome::ok("edited")
    }
}

#[tokio::test]
async fn a_denied_call_never_reaches_the_tool_and_the_model_sees_why() {
    let m = Mock(MockCompletionModel::from_stream_turns([
        vec![
            E::tool_call("c1", "edit", serde_json::json!({"path": ".git/config"})),
            E::final_response_with_total_tokens(1),
        ],
        vec![
            E::tool_call("c2", "edit", serde_json::json!({"path": "src/a.rs"})),
            E::final_response_with_total_tokens(1),
        ],
        vec![E::text("done"), E::final_response_with_total_tokens(1)],
    ]));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let cfg = agent_loop::RunConfig::new().max_turns(5);
    let mut d = Driver::new("s", m.clone(), Edit(calls.clone()), MemoryStore::new(), cfg);
    d.set_hook(policy());
    let mut run = d.open("go", 5).unwrap();
    assert_eq!(d.drive(&mut run).await.unwrap(), End::Done("done".into()));
    // Only the allowed call ran.
    assert_eq!(*calls.lock().unwrap(), ["src/a.rs"]);
    // The model's second request carried the denial and its reason.
    let second = format!("{:?}", m.0.requests()[1]);
    assert!(
        second.contains("denied by policy")
            && second.contains("a write here changes what runs later"),
        "{second}"
    );
}

#[test]
fn examples_are_checked_at_load_and_stay_optional() {
    let head = "[[path]]\ndeny = [\".git\", \"tools/hooks\"]\njustification = \"j\"\n";
    // Agreeing examples load; no examples still loads.
    PathPolicy::from_toml(&format!(
        "{head}match = [\".git/config\", \"Tools/Hooks/pre-push\"]\nnot_match = [\"src/.git-notes\", \"tools/other\"]\n"
    ))
    .unwrap();
    PathPolicy::from_toml(head).unwrap();
    for (extra, needle) in [
        ("match = [\"src/main.rs\"]\n", "not denied"),
        ("not_match = [\".git/HEAD\"]\n", "is denied"),
        ("match = [\"/etc/passwd\"]\n", "workspace-relative"),
        ("not_match = [\"../x\"]\n", "workspace-relative"),
        ("match = [\"\"]\n", "workspace-relative"),
    ] {
        match PathPolicy::from_toml(&format!("{head}{extra}")) {
            Err(PathPolicyError::Rule(m)) => assert!(m.contains(needle), "{extra}: {m}"),
            other => panic!("{extra}: {:?}", other.map(|_| ())),
        }
    }
    // An example is judged by its own rule only: a path denied by another rule is not a `match` of this one.
    let two = "[[path]]\ndeny = [\"a\"]\njustification = \"j\"\nmatch = [\"b/x\"]\n\n[[path]]\ndeny = [\"b\"]\njustification = \"j\"\n";
    assert!(matches!(
        PathPolicy::from_toml(two),
        Err(PathPolicyError::Rule(_))
    ));
}
