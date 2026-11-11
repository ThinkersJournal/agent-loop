// SPDX-License-Identifier: MIT OR Apache-2.0
//! `CommandPolicy`: a denied command prefix is refused in any shell segment, the examples in the file are checked at load,
//! rules that a hook cannot honour are refused rather than ignored, and each documented limit is pinned by a test.
use agent_loop::{CommandPolicy, CommandPolicyError, Hook, ToolCall, ToolDecision};

const RULES: &str = r#"
[workspace]
mode = "copy-back"

[[command]]
prefix = ["git", "push"]
decision = "deny"
justification = "publishing is the PM's step"
match = ["git push origin main", "cd x && git push", "FOO=1 git push"]
not_match = ["git pull", "git status", "echo git push"]

[[command]]
prefix = ["rm", "-rf"]
decision = "deny"
justification = "no recursive delete"
match = ["rm -rf /tmp/x"]
"#;

fn policy() -> CommandPolicy {
    CommandPolicy::from_toml(RULES).unwrap()
}

fn decide(p: &mut CommandPolicy, args: serde_json::Value) -> ToolDecision {
    p.before_tool(ToolCall {
        id: "c",
        name: "shell",
        args: &args,
    })
}

fn denied(p: &mut CommandPolicy, cmd: &str) -> bool {
    matches!(
        decide(p, serde_json::json!({ "command": cmd })),
        ToolDecision::Deny(_)
    )
}

#[test]
fn a_denied_prefix_is_refused_with_the_justification_and_others_are_allowed() {
    let mut p = policy();
    match decide(
        &mut p,
        serde_json::json!({ "command": "git push origin main" }),
    ) {
        ToolDecision::Deny(why) => {
            assert!(why.contains("git push"), "{why}");
            assert!(why.contains("publishing is the PM's step"), "{why}");
        }
        other => panic!("{other:?}"),
    }
    for ok in [
        "git pull",
        "git status",
        "cargo test",
        "echo git push",
        "git",
        "",
    ] {
        assert!(!denied(&mut p, ok), "{ok:?} must be allowed");
    }
}

#[test]
fn every_segment_is_read_not_only_the_first() {
    let mut p = policy();
    for bad in [
        "echo ok; git push",
        "true && git push",
        "false || git push",
        "ls | git push",
        "sleep 1 & git push",
        "echo $(git push)",
        "echo `git push`",
        "(git push)",
        "{ git push; }",
        "echo hi\ngit push",
    ] {
        assert!(denied(&mut p, bad), "{bad:?} must be denied");
    }
}

#[test]
fn program_names_are_normalised_but_arguments_are_compared_as_written() {
    let mut p = policy();
    for bad in [
        "/usr/bin/git push",
        "\"C:\\Program Files\\Git\\cmd\\GIT.EXE\" push",
        "git.cmd PUSH",
        "\"git\" 'push'",
        "A=1 B=two git push",
        "RM -RF x",
    ] {
        assert!(denied(&mut p, bad), "{bad:?} must be denied");
    }
    // A leading word that is not an assignment is the program.
    assert!(!denied(&mut p, "=x git push"));
    assert!(!denied(&mut p, "1A=x git push"));
}

#[test]
fn separators_inside_quotes_fail_closed() {
    let mut p = policy();
    assert!(denied(&mut p, "echo \"a; git push\""));
}

#[test]
fn documented_limit_options_between_prefix_words_are_not_seen() {
    // Pinned so that nobody relies on it: `git -C dir push` is not matched by `["git", "push"]`.
    let mut p = policy();
    assert!(!denied(&mut p, "git -C dir push"));
}

#[test]
fn the_command_is_found_under_the_default_names_at_any_depth_and_the_names_can_change() {
    let mut p = policy();
    assert!(matches!(
        decide(&mut p, serde_json::json!({ "cmd": "git push" })),
        ToolDecision::Deny(_)
    ));
    assert!(matches!(
        decide(
            &mut p,
            serde_json::json!({ "steps": [{ "command": "git push" }] })
        ),
        ToolDecision::Deny(_)
    ));
    // A string under another name is not a command line until that name is added.
    assert_eq!(
        decide(&mut p, serde_json::json!({ "script": "git push" })),
        ToolDecision::Allow
    );
    let mut q = policy().with_keys(["script"]);
    assert!(matches!(
        decide(&mut q, serde_json::json!({ "script": "git push" })),
        ToolDecision::Deny(_)
    ));
    assert_eq!(
        decide(&mut q, serde_json::json!({ "command": "git push" })),
        ToolDecision::Allow
    );
}

fn rule_err(text: &str) -> String {
    match CommandPolicy::from_toml(text) {
        Err(CommandPolicyError::Rule(m)) => m,
        Err(e) => panic!("expected a rule error, got {e}"),
        Ok(_) => panic!("expected an error"),
    }
}

#[test]
fn examples_that_disagree_with_the_prefix_stop_the_file_from_loading() {
    let head =
        "[[command]]\nprefix = [\"git\", \"push\"]\ndecision = \"deny\"\njustification = \"j\"\n";
    let m = rule_err(&format!("{head}match = [\"git pull\"]\n"));
    assert!(m.contains("git pull") && m.contains("`match`"), "{m}");
    let m = rule_err(&format!(
        "{head}match = [\"git push\"]\nnot_match = [\"git push -f\"]\n"
    ));
    assert!(
        m.contains("git push -f") && m.contains("`not_match`"),
        "{m}"
    );
    // A rule with no `match` example is refused: the examples are the test.
    let m = rule_err(head);
    assert!(m.contains("match"), "{m}");
}

#[test]
fn rules_a_hook_cannot_honour_or_that_are_malformed_are_errors_not_empty_sets() {
    let ok = "match = [\"git push\"]\n";
    for (body, needle) in [
        (
            "prefix = [\"git\", \"push\"]\ndecision = \"ask\"\njustification = \"j\"\n",
            "ask",
        ),
        (
            "prefix = [\"git\", \"push\"]\ndecision = \"allow\"\njustification = \"j\"\n",
            "allow",
        ),
        (
            "prefix = [\"git\", \"push\"]\ndecision = \"deny\"\njustification = \"  \"\n",
            "justification",
        ),
        (
            "prefix = []\ndecision = \"deny\"\njustification = \"j\"\n",
            "prefix",
        ),
        (
            "prefix = [\"git push\"]\ndecision = \"deny\"\njustification = \"j\"\n",
            "one word",
        ),
        (
            "prefix = [\"git\", \" \"]\ndecision = \"deny\"\njustification = \"j\"\n",
            "prefix",
        ),
    ] {
        let m = rule_err(&format!("[[command]]\n{body}{ok}"));
        assert!(m.contains(needle), "{needle}: {m}");
    }
    // An unknown key (a typo such as `prefx`, or `decison`) is a TOML error, never a weaker rule.
    let typo = "[[command]]\nprefix = [\"git\"]\ndecison = \"deny\"\ndecision = \"deny\"\njustification = \"j\"\nmatch = [\"git\"]\n";
    assert!(matches!(
        CommandPolicy::from_toml(typo),
        Err(CommandPolicyError::Toml(_))
    ));
    assert!(matches!(
        CommandPolicy::from_toml("not toml ["),
        Err(CommandPolicyError::Toml(_))
    ));
}

#[test]
fn a_file_with_no_command_rules_loads_and_allows_everything() {
    let mut p = CommandPolicy::from_toml("[workspace]\nmode = \"bind\"\n").unwrap();
    assert!(!denied(&mut p, "git push"));
}

#[test]
fn the_same_file_loads_for_the_path_rules_too() {
    // One policy file, two readers: `[[command]]` does not upset `PathPolicy`, and `[[path]]` does not upset this one.
    let both = format!("{RULES}\n[[path]]\ndeny = [\".git\"]\njustification = \"j\"\n");
    assert!(CommandPolicy::from_toml(&both).is_ok());
    assert!(agent_loop::PathPolicy::from_toml(&both).is_ok());
}
