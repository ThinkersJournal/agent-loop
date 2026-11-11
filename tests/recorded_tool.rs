// SPDX-License-Identifier: MIT OR Apache-2.0
//! `RecordedWriteBackTool`: the same tool as `WriteBackTool`, with the run record kept. It writes the intent first, records the
//! outcome, tells the model when a record could not be written, and offers exactly what the plain tool offers.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use agent_loop::{
    Approver, ChangeSet, Decision, Manifest, MemoryBlobs, MemoryLedger, MemoryRecords, RealTree,
    RecordedWriteBackTool, Records, RecordsError, TOOL_NAME, ToolHost, WorkCopy, WorkspaceMode,
    WorkspaceRecord, WritePolicy,
};

const M: WorkspaceMode = WorkspaceMode::CopyBack;

#[derive(Clone, Default)]
struct Tree(BTreeMap<String, Vec<u8>>);
impl Tree {
    fn of(files: &[(&str, &str)]) -> Self {
        Self(
            files
                .iter()
                .map(|(p, c)| (p.to_string(), c.as_bytes().to_vec()))
                .collect(),
        )
    }
    fn manifest(&self) -> Manifest {
        Manifest::from_files(self.0.iter().map(|(p, b)| (p.as_str(), b.as_slice())))
    }
}
impl RealTree for Tree {
    fn hash_of(&self, p: &str) -> Option<String> {
        self.0.get(p).map(|b| agent_loop::hash_bytes(b))
    }
    fn write(&mut self, p: &str, b: &[u8]) -> Result<(), String> {
        self.0.insert(p.to_string(), b.to_vec());
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

struct Ask(Decision);
impl Approver for Ask {
    fn ask(&mut self, _: &ChangeSet, _: &str) -> Decision {
        self.0.clone()
    }
}

/// A log that refuses to append records of the named kind (empty: appends everything).
struct Failing {
    inner: MemoryRecords,
    fail_kind: &'static str,
}
impl Records for Failing {
    fn append(&self, session: &str, body: &str) -> Result<u64, RecordsError> {
        if !self.fail_kind.is_empty() && body.contains(self.fail_kind) {
            return Err(RecordsError::Backend("disk full".into()));
        }
        self.inner.append(session, body)
    }
    fn list(&self, session: &str) -> Result<agent_loop::Listing, RecordsError> {
        self.inner.list(session)
    }
}

fn tool(
    mode: WorkspaceMode,
    policy: WritePolicy,
    base: &Tree,
    work: &Tree,
    answer: Decision,
    records: Arc<dyn Records>,
) -> RecordedWriteBackTool<Tree, Tree, Ask, MemoryLedger> {
    RecordedWriteBackTool {
        mode,
        policy,
        base: base.manifest(),
        work: work.clone(),
        real: base.clone(),
        approver: Ask(answer),
        ledger: MemoryLedger::default(),
        manifest_of: |w| w.manifest(),
        records,
        session: "s".into(),
    }
}

fn approve() -> Decision {
    Decision::Approve {
        approved: BTreeSet::new(),
    }
}

fn log() -> Arc<MemoryRecords> {
    Arc::new(MemoryRecords::new(MemoryBlobs::new()))
}

#[tokio::test]
async fn it_is_offered_exactly_when_the_plain_tool_is() {
    let t = Tree::of(&[("a.rs", "1")]);
    let n = |mode, policy| {
        tool(mode, policy, &t, &t, Decision::Deny, log())
            .definitions()
            .len()
    };
    assert_eq!(n(M, WritePolicy::Ask), 1);
    assert_eq!(n(WorkspaceMode::Bind, WritePolicy::Ask), 0);
    assert_eq!(n(M, WritePolicy::Deny), 0);
    let d = tool(M, WritePolicy::Ask, &t, &t, Decision::Deny, log()).definitions();
    assert!(format!("{:?}", d[0]).contains(TOOL_NAME));
}

#[tokio::test]
async fn calling_it_applies_records_intent_then_done_and_moves_the_base() {
    let base = Tree::of(&[("a.rs", "1")]);
    let work = Tree::of(&[("a.rs", "2")]);
    let records = log();
    let mut t = tool(
        M,
        WritePolicy::Ask,
        &base,
        &work,
        approve(),
        records.clone(),
    );
    let out = t.call("c1", TOOL_NAME, &serde_json::json!({})).await;
    assert!(!out.is_error && out.text.contains("a.rs"), "{}", out.text);
    assert!(!out.text.contains("WARNING"));
    assert_eq!(t.real.manifest(), work.manifest());
    let f = records.resume_facts("s").unwrap();
    assert_eq!(f.unfinished, None);
    assert_eq!(f.done.len(), 1);
    // The base moved, so a second call has nothing to write and records an `empty` outcome.
    let out = t.call("c2", TOOL_NAME, &serde_json::json!({})).await;
    assert_eq!(out.text, "nothing to write back");
    assert_eq!(records.resume_facts("s").unwrap().done.len(), 2);
    // An unknown tool name is an error and records nothing.
    assert!(t.call("c3", "other", &serde_json::json!({})).await.is_error);
    assert_eq!(records.resume_facts("s").unwrap().done.len(), 2);
}

#[tokio::test]
async fn a_declined_call_is_an_error_result_and_is_recorded() {
    let base = Tree::of(&[("a.rs", "1")]);
    let work = Tree::of(&[("a.rs", "2")]);
    let records = log();
    let mut t = tool(
        M,
        WritePolicy::Ask,
        &base,
        &work,
        Decision::Deny,
        records.clone(),
    );
    let out = t.call("c1", TOOL_NAME, &serde_json::json!({})).await;
    assert!(out.is_error);
    assert_eq!(t.real.manifest(), base.manifest());
    match &records.resume_facts("s").unwrap().done[..] {
        [WorkspaceRecord::Done { outcome, .. }] => assert_eq!(outcome, "denied"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn if_the_intent_cannot_be_recorded_the_model_is_told_and_nothing_is_written() {
    let base = Tree::of(&[("a.rs", "1")]);
    let work = Tree::of(&[("a.rs", "2")]);
    let records = Arc::new(Failing {
        inner: MemoryRecords::new(MemoryBlobs::new()),
        fail_kind: "writeback.intent",
    });
    let mut t = tool(M, WritePolicy::Ask, &base, &work, approve(), records);
    let out = t.call("c1", TOOL_NAME, &serde_json::json!({})).await;
    assert!(out.is_error);
    assert!(
        out.text.contains("nothing was written") && out.text.contains("disk full"),
        "{}",
        out.text
    );
    assert_eq!(t.real.manifest(), base.manifest());
}

#[tokio::test]
async fn if_only_the_done_cannot_be_recorded_the_result_stands_with_a_warning() {
    let base = Tree::of(&[("a.rs", "1")]);
    let work = Tree::of(&[("a.rs", "2")]);
    let records = Arc::new(Failing {
        inner: MemoryRecords::new(MemoryBlobs::new()),
        fail_kind: "writeback.done",
    });
    let mut t = tool(
        M,
        WritePolicy::Ask,
        &base,
        &work,
        approve(),
        records.clone(),
    );
    let out = t.call("c1", TOOL_NAME, &serde_json::json!({})).await;
    assert!(!out.is_error, "the write happened: {}", out.text);
    assert!(
        out.text.contains("written: a.rs") && out.text.contains("WARNING"),
        "{}",
        out.text
    );
    assert_eq!(t.real.manifest(), work.manifest());
    // A resumed session sees an unfinished intent.
    assert!(records.resume_facts("s").unwrap().unfinished.is_some());
}
