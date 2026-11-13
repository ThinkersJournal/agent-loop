// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `spawn session` client against a scripted fake. Replies are written from AntSpawn's `docs/SESSION-PLAN.md`
//! (at `3c9461b`), not captured from a real run.

use std::collections::{BTreeSet, VecDeque};

use agent_loop::{AntSpawnSession, Lines, SessionError, WorkspaceMode};
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

fn sent(s: &AntSpawnSession<Fake>) -> Vec<Value> {
    s.lines()
        .sent
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn exec_sends_the_documented_line_and_decodes_base64() {
    let mut s = AntSpawnSession::new(
        Fake::new(&[
            r#"{"op":"exec","id":1,"status":"exited","exit_code":0,"stdout":"aGk=","stderr":""}"#,
        ]),
        WorkspaceMode::CopyBack,
    );
    let r = s.exec(&["cmd".into(), "/c".into(), "dir".into()]).unwrap();
    assert_eq!(r.stdout, b"hi");
    assert_eq!(r.exit_code, Some(0));
    assert_eq!(
        sent(&s)[0],
        serde_json::json!({"op":"exec","id":1,"cmd":["cmd","/c","dir"]})
    );
}

#[test]
fn changes_parses_the_set_and_apply_sends_only_what_it_is_given() {
    let changes = format!(
        r#"{{"op":"changes","id":1,"added":[{{"path":"n.txt","size":3,"sha256":"aa"}}],"modified":[{{"path":"a.txt","size":1,"sha256":"bb"}}],"deleted":["old.txt"],"later_step":["build.rs"],"refused":[{{"path":".github/x","why":"protected"}}],"set_hash":"{H}"}}"#
    );
    let applied = format!(
        r#"{{"op":"applied","id":2,"set_hash":"{H}","applied":["n.txt","a.txt"],"refused":[],"conflicts":[]}}"#
    );
    let mut s = AntSpawnSession::new(Fake::new(&[&changes, &applied]), WorkspaceMode::CopyBack);
    let st = s.changes().unwrap();
    assert_eq!(st.added[0].path, "n.txt");
    assert_eq!(st.modified[0].size, 1);
    assert_eq!(st.deleted, ["old.txt"]);
    assert_eq!(st.later_step, ["build.rs"]);
    assert_eq!(st.refused[0].why, "protected");
    assert_eq!(st.paths().count(), 3);
    assert!(!st.is_empty());

    let r = s
        .apply(&st.set_hash, &BTreeSet::new(), &BTreeSet::new())
        .unwrap();
    assert!(r.written());
    assert_eq!(
        sent(&s)[1],
        serde_json::json!({"op":"apply","id":2,"set_hash":H,"deletes":[],"protected":[]})
    );
}

#[test]
fn apply_with_conflicts_or_refusals_is_not_written() {
    let conflict = format!(
        r#"{{"op":"applied","id":1,"set_hash":"{H}","applied":[],"refused":[],"conflicts":[{{"path":"a.txt","why":"changed"}}]}}"#
    );
    let refused = format!(
        r#"{{"op":"applied","id":2,"set_hash":"{H}","applied":[],"refused":[{{"path":"x","why":"deletes"}}],"conflicts":[]}}"#
    );
    let mut s = AntSpawnSession::new(Fake::new(&[&conflict, &refused]), WorkspaceMode::CopyBack);
    assert!(
        !s.apply(H, &BTreeSet::new(), &BTreeSet::new())
            .unwrap()
            .written()
    );
    assert!(
        !s.apply(H, &BTreeSet::new(), &BTreeSet::new())
            .unwrap()
            .written()
    );
}

#[test]
fn bind_never_sends_changes_or_apply() {
    let mut s = AntSpawnSession::new(Fake::new(&[]), WorkspaceMode::Bind);
    assert!(matches!(s.changes(), Err(SessionError::Refused(_))));
    assert!(matches!(
        s.apply(H, &BTreeSet::new(), &BTreeSet::new()),
        Err(SessionError::Refused(_))
    ));
    assert!(s.lines().sent.is_empty(), "nothing reached the child");
}

#[test]
fn an_error_reply_is_refused_and_a_wrong_id_is_a_protocol_error() {
    let mut s = AntSpawnSession::new(
        Fake::new(&[
            r#"{"op":"error","id":1,"message":"nothing staged"}"#,
            r#"{"op":"changes","id":99}"#,
        ]),
        WorkspaceMode::CopyBack,
    );
    assert_eq!(
        s.apply(H, &BTreeSet::new(), &BTreeSet::new()),
        Err(SessionError::Refused("nothing staged".into()))
    );
    assert!(matches!(s.changes(), Err(SessionError::Protocol(_))));
}

#[test]
fn a_bad_set_hash_and_an_ended_child_are_errors_not_empty_sets() {
    let mut s = AntSpawnSession::new(
        Fake::new(&[
            r#"{"op":"changes","id":1,"added":[],"modified":[],"deleted":[],"set_hash":"zz"}"#,
        ]),
        WorkspaceMode::CopyBack,
    );
    assert!(matches!(s.changes(), Err(SessionError::Protocol(_))));
    // the fake is now out of replies: the child "exited"
    assert!(matches!(
        s.exec(&["x".into()]),
        Err(SessionError::Transport(_))
    ));
}

#[test]
fn close_is_idempotent_and_ends_further_calls() {
    let mut s = AntSpawnSession::new(
        Fake::new(&[r#"{"op":"closed","id":1}"#]),
        WorkspaceMode::CopyBack,
    );
    s.close().unwrap();
    s.close().unwrap();
    assert_eq!(s.lines().sent.len(), 1, "the second close sent nothing");
    assert!(matches!(s.changes(), Err(SessionError::Protocol(_))));
}
