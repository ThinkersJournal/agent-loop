// SPDX-License-Identifier: MIT OR Apache-2.0
//! Write-back through an AntSpawn copy-back session, against a scripted fake. Replies are written from AntSpawn's
//! `docs/SESSION-PLAN.md`, not captured from a real run.

use std::collections::{BTreeSet, VecDeque};

use agent_loop::{
    AntSpawnSession, ApplyError, Approver, ChangeSet, Decision, Kind, Lines, MemoryLedger, Reason,
    WorkspaceMode, WriteBack, WritePolicy, change_set_from_staged, write_back_staged,
};
use serde_json::Value;

struct Fake {
    sent: Vec<String>,
    replies: VecDeque<String>,
}

impl Fake {
    fn new(replies: &[&str]) -> Self {
        Self {
            sent: vec![],
            replies: replies.iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl Lines for Fake {
    fn send(&mut self, line: &str) -> Result<(), String> {
        self.sent.push(line.to_string());
        Ok(())
    }
    fn recv(&mut self) -> Result<Option<String>, String> {
        Ok(self.replies.pop_front())
    }
}

const H: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const S: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

struct Who {
    answer: Decision,
    asked: Vec<(ChangeSet, String)>,
}

impl Who {
    fn new(answer: Decision) -> Self {
        Self {
            answer,
            asked: vec![],
        }
    }
}

impl Approver for Who {
    fn ask(&mut self, set: &ChangeSet, set_hash: &str) -> Decision {
        self.asked.push((set.clone(), set_hash.to_string()));
        self.answer.clone()
    }
}

fn approve(paths: &[&str]) -> Decision {
    Decision::Approve {
        approved: paths.iter().map(|p| p.to_string()).collect(),
    }
}

fn changes_reply(added: &str, modified: &str, deleted: &str, refused: &str) -> String {
    format!(
        r#"{{"op":"changes","id":1,"added":[{added}],"modified":[{modified}],"deleted":[{deleted}],"later_step":[],"refused":[{refused}],"set_hash":"{H}"}}"#
    )
}

fn file(path: &str) -> String {
    format!(r#"{{"path":"{path}","size":3,"sha256":"{S}"}}"#)
}

fn applied_reply(applied: &str, refused: &str, conflicts: &str) -> String {
    format!(
        r#"{{"op":"applied","id":2,"set_hash":"{H}","applied":[{applied}],"refused":[{refused}],"conflicts":[{conflicts}]}}"#
    )
}

fn session(replies: &[String], mode: WorkspaceMode) -> AntSpawnSession<Fake> {
    let r: Vec<&str> = replies.iter().map(String::as_str).collect();
    AntSpawnSession::new(Fake::new(&r), mode)
}

fn sent(s: &AntSpawnSession<Fake>) -> Vec<Value> {
    s.lines()
        .sent
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn set(v: &Value) -> BTreeSet<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn an_unheld_add_is_applied_and_the_person_sees_antspawns_hash() {
    let mut s = session(
        &[
            changes_reply(&file("src/new.rs"), "", "", ""),
            applied_reply(r#""src/new.rs""#, "", ""),
        ],
        WorkspaceMode::CopyBack,
    );
    let mut who = Who::new(approve(&[]));
    let mut ledger = MemoryLedger::default();
    let out = write_back_staged(&mut s, WritePolicy::Ask, &mut who, &mut ledger);
    assert_eq!(
        out,
        WriteBack::Applied {
            set_hash: H.into(),
            paths: vec!["src/new.rs".into()]
        }
    );
    assert_eq!(who.asked.len(), 1);
    assert_eq!(who.asked[0].1, H, "the Ask carries AntSpawn's set_hash");
    let apply = &sent(&s)[1];
    assert_eq!(apply["op"], "apply");
    assert_eq!(apply["set_hash"], H);
    assert!(set(&apply["deletes"]).is_empty() && set(&apply["protected"]).is_empty());
    let last = ledger.lines.last().unwrap();
    assert_eq!(
        (last.event.as_str(), last.outcome.as_str()),
        ("writeback.set", "applied")
    );
}

#[test]
fn an_approved_delete_goes_in_deletes_and_is_ledgered() {
    let mut s = session(
        &[
            changes_reply(
                "",
                "",
                r#""old.txt""#,
                r#"{"path":"old.txt","why":"delete"}"#,
            ),
            applied_reply(r#""old.txt""#, "", ""),
        ],
        WorkspaceMode::CopyBack,
    );
    let mut who = Who::new(approve(&["old.txt"]));
    let mut ledger = MemoryLedger::default();
    let out = write_back_staged(&mut s, WritePolicy::Ask, &mut who, &mut ledger);
    assert!(matches!(out, WriteBack::Applied { .. }), "{out:?}");
    let apply = &sent(&s)[1];
    assert_eq!(
        set(&apply["deletes"]),
        BTreeSet::from(["old.txt".to_string()])
    );
    assert!(set(&apply["protected"]).is_empty());
    assert!(ledger.lines.iter().any(|l| l.event == "writeback.path"
        && l.path.as_deref() == Some("old.txt")
        && l.outcome == "approved"));
}

#[test]
fn a_protected_path_antspawn_refuses_goes_in_protected_only_when_named() {
    let replies = [
        changes_reply(
            "",
            &file(".github/workflows/ci.yml"),
            "",
            r#"{"path":".github/workflows/ci.yml","why":"protected"}"#,
        ),
        applied_reply(r#"".github/workflows/ci.yml""#, "", ""),
    ];
    let mut s = session(&replies, WorkspaceMode::CopyBack);
    let mut who = Who::new(approve(&[".github/workflows/ci.yml"]));
    let out = write_back_staged(
        &mut s,
        WritePolicy::Ask,
        &mut who,
        &mut MemoryLedger::default(),
    );
    assert!(matches!(out, WriteBack::Applied { .. }), "{out:?}");
    let apply = &sent(&s)[1];
    assert_eq!(
        set(&apply["protected"]),
        BTreeSet::from([".github/workflows/ci.yml".to_string()])
    );
    assert!(set(&apply["deletes"]).is_empty());

    // Approving something else leaves the held path unapproved: no apply is sent.
    let mut s = session(&replies[..1], WorkspaceMode::CopyBack);
    let mut who = Who::new(approve(&["src/other.rs"]));
    let out = write_back_staged(
        &mut s,
        WritePolicy::Ask,
        &mut who,
        &mut MemoryLedger::default(),
    );
    assert_eq!(
        out,
        WriteBack::Failed {
            set_hash: H.into(),
            error: ApplyError::Unapproved(vec![".github/workflows/ci.yml".into()])
        }
    );
    assert_eq!(sent(&s).len(), 1, "only `changes` was sent");
}

#[test]
fn a_build_script_antspawn_did_not_refuse_is_still_held_by_our_rules() {
    let mut s = session(
        &[changes_reply(&file("build.rs"), "", "", "")],
        WorkspaceMode::CopyBack,
    );
    let mut who = Who::new(approve(&[]));
    let out = write_back_staged(
        &mut s,
        WritePolicy::Ask,
        &mut who,
        &mut MemoryLedger::default(),
    );
    assert_eq!(
        out,
        WriteBack::Failed {
            set_hash: H.into(),
            error: ApplyError::Unapproved(vec!["build.rs".into()])
        }
    );
    assert_eq!(sent(&s).len(), 1);
}

#[test]
fn deny_sends_no_apply() {
    let mut s = session(
        &[changes_reply(&file("a.txt"), "", "", "")],
        WorkspaceMode::CopyBack,
    );
    let mut who = Who::new(Decision::Deny);
    let mut ledger = MemoryLedger::default();
    let out = write_back_staged(&mut s, WritePolicy::Ask, &mut who, &mut ledger);
    assert_eq!(out, WriteBack::Denied { set_hash: H.into() });
    assert_eq!(sent(&s).len(), 1);
    assert_eq!(ledger.lines.last().unwrap().outcome, "denied");
}

#[test]
fn a_bind_session_and_a_deny_policy_are_refused_before_anything_is_sent() {
    let mut s = session(&[], WorkspaceMode::Bind);
    let out = write_back_staged(
        &mut s,
        WritePolicy::Ask,
        &mut Who::new(approve(&[])),
        &mut MemoryLedger::default(),
    );
    assert!(matches!(out, WriteBack::Refused(_)));
    assert!(sent(&s).is_empty());

    let mut s = session(&[], WorkspaceMode::CopyBack);
    let mut ledger = MemoryLedger::default();
    let out = write_back_staged(
        &mut s,
        WritePolicy::Deny,
        &mut Who::new(approve(&[])),
        &mut ledger,
    );
    assert!(matches!(out, WriteBack::Refused(_)));
    assert!(sent(&s).is_empty());
    assert_eq!(ledger.lines[0].outcome, "policy-deny");
}

#[test]
fn an_empty_set_asks_nobody_and_applies_nothing() {
    let mut s = session(&[changes_reply("", "", "", "")], WorkspaceMode::CopyBack);
    let mut who = Who::new(approve(&[]));
    let out = write_back_staged(
        &mut s,
        WritePolicy::Ask,
        &mut who,
        &mut MemoryLedger::default(),
    );
    assert_eq!(out, WriteBack::Empty);
    assert!(who.asked.is_empty());
    assert_eq!(sent(&s).len(), 1);
}

#[test]
fn a_conflict_antspawn_reports_is_a_failure_with_nothing_written() {
    let mut s = session(
        &[
            changes_reply("", &file("src/a.rs"), "", ""),
            applied_reply("", "", r#"{"path":"src/a.rs","why":"real file changed"}"#),
        ],
        WorkspaceMode::CopyBack,
    );
    let mut ledger = MemoryLedger::default();
    let out = write_back_staged(
        &mut s,
        WritePolicy::Ask,
        &mut Who::new(approve(&[])),
        &mut ledger,
    );
    let WriteBack::Failed {
        error: ApplyError::Conflict(c),
        ..
    } = out
    else {
        panic!("{out:?}")
    };
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].path, "src/a.rs");
    assert!(ledger.lines.last().unwrap().outcome.starts_with("failed:"));
}

#[test]
fn a_path_that_is_not_plain_is_refused_and_never_asked() {
    let mut s = session(
        &[changes_reply(&file("../escape.txt"), "", "", "")],
        WorkspaceMode::CopyBack,
    );
    let mut who = Who::new(approve(&["../escape.txt"]));
    let out = write_back_staged(
        &mut s,
        WritePolicy::Ask,
        &mut who,
        &mut MemoryLedger::default(),
    );
    assert_eq!(
        out,
        WriteBack::Failed {
            set_hash: H.into(),
            error: ApplyError::Refused(vec!["../escape.txt".into()])
        }
    );
    assert!(who.asked.is_empty());
    assert_eq!(sent(&s).len(), 1);
}

#[test]
fn a_session_error_on_changes_is_a_failure_not_a_panic() {
    let mut s = session(&[], WorkspaceMode::CopyBack); // the fake's stream ends at once
    let out = write_back_staged(
        &mut s,
        WritePolicy::Ask,
        &mut Who::new(approve(&[])),
        &mut MemoryLedger::default(),
    );
    assert!(matches!(
        out,
        WriteBack::Failed {
            error: ApplyError::Io { .. },
            ..
        }
    ));
}

#[test]
fn change_set_from_staged_marks_holds_and_shows_a_refused_only_path() {
    let staged = agent_loop::Staged {
        added: vec![],
        modified: vec![agent_loop::StagedFile {
            path: "src/a.rs".into(),
            size: 3,
            sha256: S.into(),
        }],
        deleted: vec!["gone.txt".into()],
        later_step: vec![],
        refused: vec![agent_loop::PathWhy {
            path: "huge.bin".into(),
            why: "size cap".into(),
        }],
        set_hash: H.into(),
    };
    let cs = change_set_from_staged(&staged);
    let by = |p: &str| cs.changes.iter().find(|c| c.path == p).unwrap();
    assert!(by("src/a.rs").holds.is_empty());
    assert!(by("gone.txt").holds.contains(&Reason::Delete));
    assert_eq!(by("huge.bin").kind, Kind::Modify);
    assert!(by("huge.bin").holds.contains(&Reason::Protected));
    assert!(cs.changes.iter().all(|c| c.base.is_none()));
    let paths: Vec<&str> = cs.changes.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(paths, ["gone.txt", "huge.bin", "src/a.rs"], "path order");
}
