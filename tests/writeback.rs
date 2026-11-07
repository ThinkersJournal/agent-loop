// SPDX-License-Identifier: MIT OR Apache-2.0
//! Write-back as a tool: asks a person, applies only on yes, ledgers the set and each approved held path, exists only in copy-back.
use std::collections::{BTreeMap, BTreeSet};

use agent_loop::{
    Approver, ChangeSet, Decision, Manifest, MemoryLedger, RealTree, TOOL_NAME, ToolHost, WorkCopy,
    WorkspaceMode, WriteBack, WriteBackTool, WritePolicy, hash_bytes, write_back,
};

#[derive(Clone, Default)]
struct Tree(BTreeMap<String, Vec<u8>>);
impl Tree {
    fn of(files: &[(&str, &str)]) -> Self {
        Self(
            files
                .iter()
                .map(|(p, b)| (p.to_string(), b.as_bytes().to_vec()))
                .collect(),
        )
    }
    fn manifest(&self) -> Manifest {
        Manifest::from_files(self.0.iter().map(|(p, b)| (p.as_str(), b.as_slice())))
    }
}
impl RealTree for Tree {
    fn hash_of(&self, p: &str) -> Option<String> {
        self.0.get(p).map(|b| hash_bytes(b))
    }
    fn write(&mut self, p: &str, b: &[u8]) -> Result<(), String> {
        self.0.insert(p.into(), b.to_vec());
        Ok(())
    }
    fn remove(&mut self, p: &str) -> Result<(), String> {
        self.0.remove(p);
        Ok(())
    }
}
impl WorkCopy for Tree {
    fn read(&self, p: &str) -> Option<Vec<u8>> {
        self.0.get(p).cloned()
    }
}

/// Answers with a fixed decision and records what it was shown.
struct Fixed {
    answer: Decision,
    asked: Vec<String>,
}
impl Fixed {
    fn new(answer: Decision) -> Self {
        Self {
            answer,
            asked: vec![],
        }
    }
}
impl Approver for Fixed {
    fn ask(&mut self, _set: &ChangeSet, hash: &str) -> Decision {
        self.asked.push(hash.to_string());
        self.answer.clone()
    }
}
fn approve(paths: &[&str]) -> Decision {
    Decision::Approve {
        approved: paths.iter().map(|p| p.to_string()).collect::<BTreeSet<_>>(),
    }
}

const M: WorkspaceMode = WorkspaceMode::CopyBack;

#[test]
fn yes_applies_and_ledgers_the_set_and_each_approved_held_path() {
    let base = Tree::of(&[("a.rs", "1"), ("old.rs", "1")]);
    let work = Tree::of(&[("a.rs", "2")]);
    let (mut real, mut led) = (base.clone(), MemoryLedger::default());
    let mut who = Fixed::new(approve(&["old.rs"]));
    let out = write_back(
        M,
        WritePolicy::Ask,
        &base.manifest(),
        &work.manifest(),
        &work,
        &mut real,
        &mut who,
        &mut led,
    );
    let WriteBack::Applied { set_hash, paths } = out else {
        panic!("{out:?}")
    };
    assert_eq!(paths, ["a.rs", "old.rs"]);
    assert_eq!(real.manifest(), work.manifest());
    assert_eq!(who.asked.len(), 1, "asked once");
    assert_eq!(who.asked[0], set_hash, "about this exact set");
    let got: Vec<(&str, Option<&str>, &str)> = led
        .lines
        .iter()
        .map(|l| (l.event.as_str(), l.path.as_deref(), l.outcome.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("writeback.path", Some("old.rs"), "approved"),
            ("writeback.set", None, "applied")
        ]
    );
    assert!(led.lines.iter().all(|l| l.set_hash == set_hash));
}

#[test]
fn no_leaves_the_real_tree_untouched_and_is_ledgered() {
    let base = Tree::of(&[("a.rs", "1")]);
    let work = Tree::of(&[("a.rs", "2")]);
    let (mut real, mut led) = (base.clone(), MemoryLedger::default());
    let mut who = Fixed::new(Decision::Deny);
    let out = write_back(
        M,
        WritePolicy::Ask,
        &base.manifest(),
        &work.manifest(),
        &work,
        &mut real,
        &mut who,
        &mut led,
    );
    assert!(matches!(out, WriteBack::Denied { .. }));
    assert_eq!(real.manifest(), base.manifest());
    assert_eq!(led.lines.len(), 1);
    assert_eq!(led.lines[0].outcome, "denied");
}

#[test]
fn an_unapproved_held_path_fails_and_writes_nothing() {
    let base = Tree::of(&[("CLAUDE.md", "1"), ("a.rs", "1")]);
    let work = Tree::of(&[("CLAUDE.md", "2"), ("a.rs", "2")]);
    let (mut real, mut led) = (base.clone(), MemoryLedger::default());
    // Yes, but the person approved nothing by name.
    let mut who = Fixed::new(approve(&[]));
    let out = write_back(
        M,
        WritePolicy::Ask,
        &base.manifest(),
        &work.manifest(),
        &work,
        &mut real,
        &mut who,
        &mut led,
    );
    assert!(matches!(out, WriteBack::Failed { .. }), "{out:?}");
    assert_eq!(
        real.manifest(),
        base.manifest(),
        "a.rs must not be written either"
    );
    assert!(led.lines.last().unwrap().outcome.starts_with("failed:"));
    assert!(
        led.lines.iter().all(|l| l.event != "writeback.path"),
        "no approval line for a failed apply"
    );
}

#[test]
fn a_conflict_is_reported_and_ledgered() {
    let base = Tree::of(&[("a.rs", "1")]);
    let work = Tree::of(&[("a.rs", "2")]);
    let (mut real, mut led) = (Tree::of(&[("a.rs", "edited")]), MemoryLedger::default());
    let mut who = Fixed::new(approve(&[]));
    let out = write_back(
        M,
        WritePolicy::Ask,
        &base.manifest(),
        &work.manifest(),
        &work,
        &mut real,
        &mut who,
        &mut led,
    );
    assert!(matches!(out, WriteBack::Failed { .. }));
    assert_eq!(real.0["a.rs"], b"edited");
}

#[test]
fn an_empty_set_does_not_ask() {
    let t = Tree::of(&[("a.rs", "1")]);
    let (mut real, mut led) = (t.clone(), MemoryLedger::default());
    let mut who = Fixed::new(Decision::Deny);
    let out = write_back(
        M,
        WritePolicy::Ask,
        &t.manifest(),
        &t.manifest(),
        &t,
        &mut real,
        &mut who,
        &mut led,
    );
    assert_eq!(out, WriteBack::Empty);
    assert!(who.asked.is_empty());
}

#[test]
fn bind_mode_and_deny_policy_never_ask_and_are_ledgered() {
    let base = Tree::of(&[("a.rs", "1")]);
    let work = Tree::of(&[("a.rs", "2")]);
    for (mode, policy, why) in [
        (WorkspaceMode::Bind, WritePolicy::Ask, "not-copy-back"),
        (M, WritePolicy::Deny, "policy-deny"),
    ] {
        let (mut real, mut led) = (base.clone(), MemoryLedger::default());
        let mut who = Fixed::new(approve(&[]));
        let out = write_back(
            mode,
            policy,
            &base.manifest(),
            &work.manifest(),
            &work,
            &mut real,
            &mut who,
            &mut led,
        );
        assert!(matches!(out, WriteBack::Refused(_)));
        assert!(who.asked.is_empty());
        assert_eq!(real.manifest(), base.manifest());
        assert_eq!(
            (led.lines[0].event.as_str(), led.lines[0].outcome.as_str()),
            ("writeback.refused", why)
        );
    }
}

fn tool(
    mode: WorkspaceMode,
    policy: WritePolicy,
    base: &Tree,
    work: &Tree,
    answer: Decision,
) -> WriteBackTool<Tree, Tree, Fixed, MemoryLedger> {
    WriteBackTool {
        mode,
        policy,
        base: base.manifest(),
        work: work.clone(),
        real: base.clone(),
        approver: Fixed::new(answer),
        ledger: MemoryLedger::default(),
        manifest_of: |w| w.manifest(),
    }
}

#[tokio::test]
async fn the_tool_is_offered_only_in_copy_back_with_ask() {
    let t = Tree::of(&[("a.rs", "1")]);
    let names = |h: &WriteBackTool<Tree, Tree, Fixed, MemoryLedger>| {
        h.definitions()
            .iter()
            .map(|d| format!("{d:?}"))
            .collect::<Vec<_>>()
    };
    let on = tool(M, WritePolicy::Ask, &t, &t, Decision::Deny);
    let defs = names(&on);
    assert_eq!(defs.len(), 1);
    assert!(defs[0].contains(TOOL_NAME));
    assert!(
        names(&tool(
            WorkspaceMode::Bind,
            WritePolicy::Ask,
            &t,
            &t,
            Decision::Deny
        ))
        .is_empty()
    );
    assert!(names(&tool(M, WritePolicy::Deny, &t, &t, Decision::Deny)).is_empty());
}

#[tokio::test]
async fn calling_the_tool_applies_then_the_next_call_diffs_from_the_new_base() {
    let base = Tree::of(&[("a.rs", "1")]);
    let work = Tree::of(&[("a.rs", "2")]);
    let mut t = tool(M, WritePolicy::Ask, &base, &work, approve(&[]));
    let out = t.call("c1", TOOL_NAME, &serde_json::json!({})).await;
    assert!(!out.is_error, "{}", out.text);
    assert!(out.text.contains("a.rs"));
    assert_eq!(t.real.manifest(), work.manifest());
    // Second call: nothing new, so it does not ask again.
    let out = t.call("c2", TOOL_NAME, &serde_json::json!({})).await;
    assert_eq!(out.text, "nothing to write back");
    assert_eq!(t.approver.asked.len(), 1);
    // A declined call is an error result the model can read.
    let mut d = tool(M, WritePolicy::Ask, &base, &work, Decision::Deny);
    assert!(
        d.call("c3", TOOL_NAME, &serde_json::json!({}))
            .await
            .is_error
    );
    // An unknown tool name is an error, not a write-back.
    assert!(d.call("c4", "other", &serde_json::json!({})).await.is_error);
}
