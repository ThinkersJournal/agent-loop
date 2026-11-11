// SPDX-License-Identifier: MIT OR Apache-2.0
//! `write_back_recorded`: the intent is durable before the real tree changes, a failed intent stops the write, a failed `done` is
//! reported and leaves an unfinished intent, every outcome is recorded, and `recover_unfinished` settles what a crash left.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use agent_loop::{
    Approver, ChangeSet, Decision, Manifest, MemoryBlobs, MemoryLedger, MemoryRecords, RealTree,
    Records, RecordsError, Recovery, WorkCopy, WorkspaceMode, WorkspaceRecord, WriteBack,
    WritePolicy, check_resume, diff, recover_unfinished, write_back_recorded,
};

const M: WorkspaceMode = WorkspaceMode::CopyBack;

#[derive(Clone)]
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

struct Yes;
impl Approver for Yes {
    fn ask(&mut self, _: &ChangeSet, _: &str) -> Decision {
        Decision::Approve {
            approved: BTreeSet::new(),
        }
    }
}

/// A real tree that notes how many records existed the moment the first write reached it.
struct Watching {
    tree: Tree,
    records: Arc<MemoryRecords>,
    seen_at_first_write: Mutex<Option<usize>>,
}
impl RealTree for Watching {
    fn hash_of(&self, p: &str) -> Option<String> {
        self.tree.hash_of(p)
    }
    fn write(&mut self, p: &str, b: &[u8]) -> Result<(), String> {
        let n = self.records.list("s").unwrap().records.len();
        self.seen_at_first_write.lock().unwrap().get_or_insert(n);
        self.tree.write(p, b)
    }
    fn remove(&mut self, p: &str) -> Result<(), String> {
        self.tree.remove(p)
    }
}

/// A log that refuses to append records of the named kind.
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

fn outcomes(r: &dyn Records) -> Vec<String> {
    r.resume_facts("s")
        .unwrap()
        .done
        .iter()
        .map(|d| match d {
            WorkspaceRecord::Done { outcome, .. } => outcome.clone(),
            _ => unreachable!(),
        })
        .collect()
}

#[test]
fn the_intent_is_durable_before_the_first_write_and_the_done_follows() {
    let base = Tree::of(&[("a.rs", "1"), ("b.rs", "1")]);
    let work = Tree::of(&[("a.rs", "2"), ("b.rs", "2")]);
    let records = Arc::new(MemoryRecords::new(MemoryBlobs::new()));
    let mut real = Watching {
        tree: base.clone(),
        records: records.clone(),
        seen_at_first_write: Mutex::new(None),
    };
    let out = write_back_recorded(
        &*records,
        "s",
        M,
        WritePolicy::Ask,
        &base.manifest(),
        &work.manifest(),
        &work,
        &mut real,
        &mut Yes,
        &mut MemoryLedger::default(),
    );
    assert!(matches!(out.outcome, WriteBack::Applied { .. }), "{out:?}");
    assert_eq!(out.record_error, None);
    assert_eq!(
        *real.seen_at_first_write.lock().unwrap(),
        Some(1),
        "exactly the intent existed when the real tree was first written"
    );
    assert_eq!(kinds(&*records), ["writeback.intent", "writeback.done"]);
    let f = records.resume_facts("s").unwrap();
    assert_eq!(f.unfinished, None);
    assert_eq!(outcomes(&*records), ["applied"]);
    assert_eq!(real.tree.manifest(), work.manifest());
}

#[test]
fn if_the_intent_cannot_be_recorded_the_real_tree_is_not_touched() {
    let base = Tree::of(&[("a.rs", "1")]);
    let work = Tree::of(&[("a.rs", "2")]);
    let records = Failing {
        inner: MemoryRecords::new(MemoryBlobs::new()),
        fail_kind: "writeback.intent",
    };
    let mut real = base.clone();
    let mut led = MemoryLedger::default();
    let out = write_back_recorded(
        &records,
        "s",
        M,
        WritePolicy::Ask,
        &base.manifest(),
        &work.manifest(),
        &work,
        &mut real,
        &mut Yes,
        &mut led,
    );
    assert!(matches!(out.outcome, WriteBack::Refused(_)), "{out:?}");
    assert_eq!(
        out.record_error.as_deref(),
        Some("records backend: disk full")
    );
    assert_eq!(real.manifest(), base.manifest(), "nothing was written");
    assert!(
        led.lines.iter().any(|l| l.outcome == "record-failed"),
        "the refusal is ledgered"
    );
    assert!(records.inner.list("s").unwrap().records.is_empty());
}

#[test]
fn if_the_done_cannot_be_recorded_the_apply_is_still_reported_and_the_intent_stays_unfinished() {
    let base = Tree::of(&[("a.rs", "1")]);
    let work = Tree::of(&[("a.rs", "2")]);
    let records = Failing {
        inner: MemoryRecords::new(MemoryBlobs::new()),
        fail_kind: "writeback.done",
    };
    let mut real = base.clone();
    let out = write_back_recorded(
        &records,
        "s",
        M,
        WritePolicy::Ask,
        &base.manifest(),
        &work.manifest(),
        &work,
        &mut real,
        &mut Yes,
        &mut MemoryLedger::default(),
    );
    assert!(matches!(out.outcome, WriteBack::Applied { .. }), "{out:?}");
    assert!(out.record_error.is_some());
    assert_eq!(real.manifest(), work.manifest(), "the tree did change");
    let f = records.resume_facts("s").unwrap();
    assert!(matches!(f.unfinished, Some(WorkspaceRecord::Intent { .. })));

    // A resumed session settles it from the tree: every path holds its new content.
    let r = recover_unfinished(
        &records.inner,
        "s",
        &f,
        &base.manifest(),
        &work.manifest(),
        &real,
    )
    .unwrap();
    assert_eq!(r, Recovery::Applied);
    let f = records.resume_facts("s").unwrap();
    assert_eq!(f.unfinished, None);
    assert_eq!(outcomes(&records.inner), ["applied-recovered"]);
}

#[test]
fn every_outcome_is_recorded_and_only_a_real_apply_attempt_has_an_intent() {
    let base = Tree::of(&[("a.rs", "1")]);
    let work = Tree::of(&[("a.rs", "2")]);
    let run = |mode, policy, work: &Tree, real: &mut Tree, approver: &mut dyn FnMut() -> bool| {
        let records = MemoryRecords::new(MemoryBlobs::new());
        struct Dyn<'a>(&'a mut dyn FnMut() -> bool);
        impl Approver for Dyn<'_> {
            fn ask(&mut self, _: &ChangeSet, _: &str) -> Decision {
                if (self.0)() {
                    Decision::Approve {
                        approved: BTreeSet::new(),
                    }
                } else {
                    Decision::Deny
                }
            }
        }
        let out = write_back_recorded(
            &records,
            "s",
            mode,
            policy,
            &base.manifest(),
            &work.manifest(),
            work,
            real,
            &mut Dyn(approver),
            &mut MemoryLedger::default(),
        );
        assert_eq!(out.record_error, None);
        (kinds(&records), outcomes(&records))
    };
    // Denied by the person: a done, no intent.
    let (k, o) = run(M, WritePolicy::Ask, &work, &mut base.clone(), &mut || false);
    assert_eq!(
        (k, o),
        (vec!["writeback.done".into()], vec!["denied".into()])
    );
    // Policy denies.
    let (k, o) = run(M, WritePolicy::Deny, &work, &mut base.clone(), &mut || true);
    assert_eq!(
        (k, o),
        (vec!["writeback.done".into()], vec!["policy-deny".into()])
    );
    // Wrong mode.
    let (k, o) = run(
        WorkspaceMode::Bind,
        WritePolicy::Ask,
        &work,
        &mut base.clone(),
        &mut || true,
    );
    assert_eq!(
        (k, o),
        (vec!["writeback.done".into()], vec!["not-copy-back".into()])
    );
    // Nothing to write back.
    let (k, o) = run(M, WritePolicy::Ask, &base, &mut base.clone(), &mut || true);
    assert_eq!(
        (k, o),
        (vec!["writeback.done".into()], vec!["empty".into()])
    );
    // The real tree changed under us: an intent was made (the apply was attempted), then a failed done.
    let (k, o) = run(
        M,
        WritePolicy::Ask,
        &work,
        &mut Tree::of(&[("a.rs", "someone else")]),
        &mut || true,
    );
    assert_eq!(
        k,
        ["writeback.intent", "writeback.done"],
        "an attempted apply is recorded"
    );
    assert!(o[0].starts_with("failed:"), "{o:?}");
}

#[test]
fn recovery_judges_each_path_and_records_only_what_it_is_sure_of() {
    let base = Tree::of(&[("a.rs", "1"), ("b.rs", "1")]);
    let work = Tree::of(&[("a.rs", "2"), ("b.rs", "2")]);
    let set_hash = diff(&base.manifest(), &work.manifest()).hash();
    let fresh = || {
        let r = MemoryRecords::new(MemoryBlobs::new());
        r.append_record(
            "s",
            &WorkspaceRecord::Intent {
                set_hash: set_hash.clone(),
                paths: vec!["a.rs".into(), "b.rs".into()],
            },
        )
        .unwrap();
        r
    };
    let settle = |r: &MemoryRecords, real: &Tree, work_m: &Manifest| {
        let f = r.resume_facts("s").unwrap();
        recover_unfinished(r, "s", &f, &base.manifest(), work_m, real).unwrap()
    };

    // Nothing written: not applied, recorded, safe to propose again.
    let r = fresh();
    assert_eq!(settle(&r, &base, &work.manifest()), Recovery::NotApplied);
    assert_eq!(outcomes(&r), ["failed:interrupted"]);
    assert_eq!(r.resume_facts("s").unwrap().unfinished, None);

    // Everything written: applied.
    let r = fresh();
    assert_eq!(settle(&r, &work, &work.manifest()), Recovery::Applied);
    assert_eq!(outcomes(&r), ["applied-recovered"]);

    // One written, one not: partial, nothing recorded, the intent stays unfinished.
    let r = fresh();
    let partial = Tree::of(&[("a.rs", "2"), ("b.rs", "1")]);
    assert_eq!(
        settle(&r, &partial, &work.manifest()),
        Recovery::Partial {
            written: vec!["a.rs".into()],
            untouched: vec!["b.rs".into()],
            other: vec![]
        }
    );
    assert!(outcomes(&r).is_empty());
    assert!(r.resume_facts("s").unwrap().unfinished.is_some());

    // A path holding neither version is also not certain.
    let r = fresh();
    let odd = Tree::of(&[("a.rs", "2"), ("b.rs", "edited by hand")]);
    assert!(matches!(
        settle(&r, &odd, &work.manifest()),
        Recovery::Partial { other, .. } if other == ["b.rs"]
    ));

    // The working copy no longer produces that change set: cannot judge, nothing recorded.
    let r = fresh();
    let changed = Tree::of(&[("a.rs", "3"), ("b.rs", "2")]);
    assert_eq!(
        settle(&r, &work, &changed.manifest()),
        Recovery::Unjudgeable
    );
    assert!(outcomes(&r).is_empty());

    // No unfinished intent: nothing to do.
    let r = MemoryRecords::new(MemoryBlobs::new());
    assert_eq!(
        settle(&r, &base, &work.manifest()),
        Recovery::NothingPending
    );
}

#[test]
fn check_resume_says_what_it_found_in_words_and_settles_only_what_is_certain() {
    let base = Tree::of(&[("a.rs", "1"), ("b.rs", "1")]);
    let work = Tree::of(&[("a.rs", "2"), ("b.rs", "2")]);
    let set_hash = diff(&base.manifest(), &work.manifest()).hash();
    let with_intent = || {
        let r = MemoryRecords::new(MemoryBlobs::new());
        r.append_record(
            "s",
            &WorkspaceRecord::Intent {
                set_hash: set_hash.clone(),
                paths: vec!["a.rs".into(), "b.rs".into()],
            },
        )
        .unwrap();
        r
    };

    // A clean, finished session says nothing.
    let r = MemoryRecords::new(MemoryBlobs::new());
    let rep = check_resume(&r, "s", &base.manifest(), &work.manifest(), &base).unwrap();
    assert_eq!(rep.recovery, Recovery::NothingPending);
    assert!(rep.notes.is_empty() && rep.facts.is_intact());

    // Applied: recorded, so the facts come back with nothing unfinished.
    let r = with_intent();
    let rep = check_resume(&r, "s", &base.manifest(), &work.manifest(), &work).unwrap();
    assert_eq!(rep.recovery, Recovery::Applied);
    assert_eq!(
        rep.facts.unfinished, None,
        "facts are read after the verdict"
    );
    assert!(rep.notes[0].contains("already applied"), "{:?}", rep.notes);

    // Not applied: safe to propose again.
    let r = with_intent();
    let rep = check_resume(&r, "s", &base.manifest(), &work.manifest(), &base).unwrap();
    assert_eq!(rep.recovery, Recovery::NotApplied);
    assert!(rep.notes[0].contains("safe to propose again"));

    // Partial: said loudly, not retried, still unfinished.
    let r = with_intent();
    let part = Tree::of(&[("a.rs", "2"), ("b.rs", "1")]);
    let rep = check_resume(&r, "s", &base.manifest(), &work.manifest(), &part).unwrap();
    assert!(matches!(rep.recovery, Recovery::Partial { .. }));
    assert!(
        rep.notes[0].contains("PARTLY applied") && rep.notes[0].contains("a person must decide")
    );
    assert!(rep.facts.unfinished.is_some());

    // Unjudgeable: the work copy changed.
    let r = with_intent();
    let other = Tree::of(&[("a.rs", "9"), ("b.rs", "2")]);
    let rep = check_resume(&r, "s", &base.manifest(), &other.manifest(), &work).unwrap();
    assert_eq!(rep.recovery, Recovery::Unjudgeable);
    assert!(rep.notes[0].contains("could not be checked"));
}

#[test]
fn check_resume_reports_damaged_missing_and_unreadable_records() {
    let base = Tree::of(&[("a.rs", "1")]);
    let blobs = MemoryBlobs::new();
    let r = MemoryRecords::new(blobs.clone());
    use agent_loop::KeyedBlobs;
    let key = |n: u64| format!("records/s_s/{n:020}");
    r.append("s", "{\"v\":2,\"kind\":\"workspace.opened\"}")
        .unwrap(); // 1: unreadable
    assert!(blobs.create_if_absent(&key(2), b"junk".to_vec()).unwrap()); // 2: torn
    r.append("s", "{\"v\":1,\"kind\":\"x\"}").unwrap(); // 3: readable, an unknown kind
    // 4 is missing; 5 is torn too, so the gap is real.
    assert!(blobs.create_if_absent(&key(5), b"junk".to_vec()).unwrap());
    let rep = check_resume(&r, "s", &base.manifest(), &base.manifest(), &base).unwrap();
    assert!(!rep.facts.is_intact());
    let all = rep.notes.join(" | ");
    assert!(
        all.contains("damaged") && all.contains("format this version cannot read"),
        "{all}"
    );
}
