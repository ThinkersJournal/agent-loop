// SPDX-License-Identifier: MIT OR Apache-2.0
//! The recorded write-back through an AntSpawn session, and the `workspace_writeback` tool over one, against a scripted fake.
//! Replies are written from AntSpawn's `docs/SESSION-PLAN.md`, not captured from a real run.

use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use agent_loop::{
    AntSpawnSession, Approver, ChangeSet, Decision, Lines, MemoryBlobs, MemoryLedger,
    MemoryRecords, Records, RecordsError, StagedWriteBackTool, TOOL_NAME, ToolHost, WorkspaceMode,
    WorkspaceRecord, WriteBack, WritePolicy, write_back_staged_recorded,
};

const H: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const S: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// Notes how many records existed when `apply` was sent.
struct Fake {
    replies: VecDeque<String>,
    records: Arc<dyn Records>,
    seen_at_apply: Arc<Mutex<Option<usize>>>,
    sent_apply: Arc<Mutex<bool>>,
}

impl Lines for Fake {
    fn send(&mut self, line: &str) -> Result<(), String> {
        if line.contains(r#""op":"apply""#) {
            let n = self.records.list("s").unwrap().records.len();
            self.seen_at_apply.lock().unwrap().get_or_insert(n);
            *self.sent_apply.lock().unwrap() = true;
        }
        Ok(())
    }
    fn recv(&mut self) -> Result<Option<String>, String> {
        Ok(self.replies.pop_front())
    }
}

struct Ask(Decision);
impl Approver for Ask {
    fn ask(&mut self, _: &ChangeSet, _: &str) -> Decision {
        self.0.clone()
    }
}

fn yes() -> Decision {
    Decision::Approve {
        approved: BTreeSet::new(),
    }
}

/// A log that refuses to append records whose body contains `fail_kind`.
struct Failing {
    inner: MemoryRecords,
    fail_kind: &'static str,
}
impl Records for Failing {
    fn append(&self, session: &str, body: &str) -> Result<u64, RecordsError> {
        if body.contains(self.fail_kind) {
            return Err(RecordsError::Backend("disk full".into()));
        }
        self.inner.append(session, body)
    }
    fn list(&self, session: &str) -> Result<agent_loop::Listing, RecordsError> {
        self.inner.list(session)
    }
}

fn log() -> Arc<MemoryRecords> {
    Arc::new(MemoryRecords::new(MemoryBlobs::new()))
}

fn changes_reply(added: &[&str]) -> String {
    let files: Vec<String> = added
        .iter()
        .map(|p| format!(r#"{{"path":"{p}","size":3,"sha256":"{S}"}}"#))
        .collect();
    format!(
        r#"{{"op":"changes","id":1,"added":[{}],"modified":[],"deleted":[],"later_step":[],"refused":[],"set_hash":"{H}"}}"#,
        files.join(",")
    )
}

fn applied_reply(applied: &[&str]) -> String {
    let a: Vec<String> = applied.iter().map(|p| format!(r#""{p}""#)).collect();
    format!(
        r#"{{"op":"applied","id":2,"set_hash":"{H}","applied":[{}],"refused":[],"conflicts":[]}}"#,
        a.join(",")
    )
}

struct Rig {
    session: AntSpawnSession<Fake>,
    seen_at_apply: Arc<Mutex<Option<usize>>>,
    sent_apply: Arc<Mutex<bool>>,
}

fn rig(replies: &[String], mode: WorkspaceMode, records: Arc<dyn Records>) -> Rig {
    let seen_at_apply = Arc::new(Mutex::new(None));
    let sent_apply = Arc::new(Mutex::new(false));
    let fake = Fake {
        replies: replies.iter().cloned().collect(),
        records,
        seen_at_apply: seen_at_apply.clone(),
        sent_apply: sent_apply.clone(),
    };
    Rig {
        session: AntSpawnSession::new(fake, mode),
        seen_at_apply,
        sent_apply,
    }
}

fn kinds(r: &dyn Records) -> Vec<String> {
    r.list("s")
        .unwrap()
        .records
        .iter()
        .map(|x| {
            serde_json::from_str::<serde_json::Value>(&x.body).unwrap()["kind"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect()
}

fn one_add() -> Vec<String> {
    vec![
        changes_reply(&["src/new.rs"]),
        applied_reply(&["src/new.rs"]),
    ]
}

#[test]
fn the_intent_is_durable_before_apply_is_sent_and_the_done_follows() {
    let records = log();
    let mut r = rig(&one_add(), WorkspaceMode::CopyBack, records.clone());
    let out = write_back_staged_recorded(
        &*records,
        "s",
        &mut r.session,
        WritePolicy::Ask,
        &mut Ask(yes()),
        &mut MemoryLedger::default(),
    );
    assert_eq!(
        out.outcome,
        WriteBack::Applied {
            set_hash: H.into(),
            paths: vec!["src/new.rs".into()]
        }
    );
    assert_eq!(out.record_error, None);
    assert_eq!(
        *r.seen_at_apply.lock().unwrap(),
        Some(1),
        "one record (the intent) existed when apply went out"
    );
    assert_eq!(kinds(&*records), ["writeback.intent", "writeback.done"]);
    let f = records.resume_facts("s").unwrap();
    assert_eq!(f.unfinished, None);
    match &f.done[0] {
        WorkspaceRecord::Done {
            set_hash, outcome, ..
        } => assert_eq!((set_hash.as_str(), outcome.as_str()), (H, "applied")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn if_the_intent_cannot_be_recorded_apply_is_not_sent() {
    let records = Failing {
        inner: MemoryRecords::new(MemoryBlobs::new()),
        fail_kind: "writeback.intent",
    };
    let records: Arc<dyn Records> = Arc::new(records);
    let mut r = rig(&one_add(), WorkspaceMode::CopyBack, records.clone());
    let mut led = MemoryLedger::default();
    let out = write_back_staged_recorded(
        &*records,
        "s",
        &mut r.session,
        WritePolicy::Ask,
        &mut Ask(yes()),
        &mut led,
    );
    assert!(matches!(out.outcome, WriteBack::Refused(_)), "{out:?}");
    assert_eq!(
        out.record_error.as_deref(),
        Some("records backend: disk full")
    );
    assert!(!*r.sent_apply.lock().unwrap(), "apply was never sent");
    assert!(led.lines.iter().any(|l| l.outcome == "record-failed"));
}

#[test]
fn if_the_done_cannot_be_recorded_the_apply_is_still_reported_and_the_intent_stays_unfinished() {
    let failing = Arc::new(Failing {
        inner: MemoryRecords::new(MemoryBlobs::new()),
        fail_kind: "writeback.done",
    });
    let records: Arc<dyn Records> = failing.clone();
    let mut r = rig(&one_add(), WorkspaceMode::CopyBack, records.clone());
    let out = write_back_staged_recorded(
        &*records,
        "s",
        &mut r.session,
        WritePolicy::Ask,
        &mut Ask(yes()),
        &mut MemoryLedger::default(),
    );
    assert!(matches!(out.outcome, WriteBack::Applied { .. }), "{out:?}");
    assert!(out.record_error.is_some());
    let f = failing.inner.resume_facts("s").unwrap();
    assert!(f.unfinished.is_some(), "a resumed session sees the intent");
}

#[test]
fn only_a_real_apply_attempt_has_an_intent_and_every_outcome_has_a_done() {
    // Denied by the person.
    let records = log();
    let mut r = rig(
        &[changes_reply(&["a.rs"])],
        WorkspaceMode::CopyBack,
        records.clone(),
    );
    let out = write_back_staged_recorded(
        &*records,
        "s",
        &mut r.session,
        WritePolicy::Ask,
        &mut Ask(Decision::Deny),
        &mut MemoryLedger::default(),
    );
    assert!(matches!(out.outcome, WriteBack::Denied { .. }));
    assert_eq!(kinds(&*records), ["writeback.done"]);
    assert!(!*r.sent_apply.lock().unwrap());

    // Policy denies: refused before the session is asked anything.
    let records = log();
    let mut r = rig(&[], WorkspaceMode::CopyBack, records.clone());
    let out = write_back_staged_recorded(
        &*records,
        "s",
        &mut r.session,
        WritePolicy::Deny,
        &mut Ask(yes()),
        &mut MemoryLedger::default(),
    );
    assert!(matches!(out.outcome, WriteBack::Refused(_)));
    assert_eq!(kinds(&*records), ["writeback.done"]);

    // Bind session: refused, recorded as not-copy-back.
    let records = log();
    let mut r = rig(&[], WorkspaceMode::Bind, records.clone());
    let out = write_back_staged_recorded(
        &*records,
        "s",
        &mut r.session,
        WritePolicy::Ask,
        &mut Ask(yes()),
        &mut MemoryLedger::default(),
    );
    assert!(matches!(out.outcome, WriteBack::Refused(_)));
    assert_eq!(
        records.resume_facts("s").unwrap().done.len(),
        1,
        "one done, no intent"
    );
    assert_eq!(kinds(&*records), ["writeback.done"]);
}

fn tool(
    mode: WorkspaceMode,
    policy: WritePolicy,
    replies: &[String],
    records: Option<Arc<dyn Records>>,
) -> StagedWriteBackTool<Fake, Ask, MemoryLedger> {
    let probe: Arc<dyn Records> = records.clone().unwrap_or_else(|| log() as Arc<dyn Records>);
    StagedWriteBackTool {
        session: rig(replies, mode, probe).session,
        policy,
        approver: Ask(yes()),
        ledger: MemoryLedger::default(),
        records: records.map(|r| (r, "s".to_string())),
    }
}

#[test]
fn the_tool_is_offered_only_in_copy_back_and_not_when_policy_denies() {
    let n = |m, p| tool(m, p, &[], None).definitions().len();
    assert_eq!(n(WorkspaceMode::CopyBack, WritePolicy::Ask), 1);
    assert_eq!(n(WorkspaceMode::CopyBack, WritePolicy::Deny), 0);
    assert_eq!(n(WorkspaceMode::Bind, WritePolicy::Ask), 0);
}

#[tokio::test]
async fn calling_the_tool_applies_and_records_when_it_has_a_log() {
    let records = log();
    let mut t = tool(
        WorkspaceMode::CopyBack,
        WritePolicy::Ask,
        &one_add(),
        Some(records.clone()),
    );
    let out = t.call("c1", TOOL_NAME, &serde_json::json!({})).await;
    assert!(
        !out.is_error && out.text.contains("src/new.rs"),
        "{}",
        out.text
    );
    assert!(!out.text.contains("WARNING"));
    assert_eq!(kinds(&*records), ["writeback.intent", "writeback.done"]);
    assert!(t.call("c2", "other", &serde_json::json!({})).await.is_error);
    assert_eq!(kinds(&*records).len(), 2, "an unknown name records nothing");
}

#[tokio::test]
async fn the_tool_without_a_log_still_applies_and_a_failed_intent_is_an_error() {
    let mut t = tool(WorkspaceMode::CopyBack, WritePolicy::Ask, &one_add(), None);
    let out = t.call("c1", TOOL_NAME, &serde_json::json!({})).await;
    assert!(!out.is_error, "{}", out.text);

    let failing: Arc<dyn Records> = Arc::new(Failing {
        inner: MemoryRecords::new(MemoryBlobs::new()),
        fail_kind: "writeback.intent",
    });
    let mut t = tool(
        WorkspaceMode::CopyBack,
        WritePolicy::Ask,
        &one_add(),
        Some(failing),
    );
    let out = t.call("c1", TOOL_NAME, &serde_json::json!({})).await;
    assert!(out.is_error && out.text.contains("WARNING"), "{}", out.text);
}
