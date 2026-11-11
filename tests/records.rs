// SPDX-License-Identifier: MIT OR Apache-2.0
//! The run record: sequence numbers, hash-checked values, torn values and gaps reported (never skipped), the fold a resumed session
//! reads, and (with the `persistant` feature) the same log over a real PersistAnt `fs` store.
use agent_loop::{
    KeyedBlobs, MemoryBlobs, MemoryRecords, RecordLog, Records, ResumeFacts, WorkspaceRecord,
};

/// A single gap range (an array of one range trips clippy's single_range_in_vec_init).
fn one(r: std::ops::RangeInclusive<u64>) -> Vec<std::ops::RangeInclusive<u64>> {
    std::iter::once(r).collect()
}

fn opened() -> WorkspaceRecord {
    WorkspaceRecord::Opened {
        mode: "copy-back".into(),
        root: "C:/p".into(),
        policy_hash: "ph".into(),
        base: Some("bh".into()),
    }
}
fn intent(h: &str) -> WorkspaceRecord {
    WorkspaceRecord::Intent {
        set_hash: h.into(),
        paths: vec!["a.rs".into()],
    }
}
fn done(h: &str, outcome: &str) -> WorkspaceRecord {
    WorkspaceRecord::Done {
        set_hash: h.into(),
        outcome: outcome.into(),
        approved: vec![],
    }
}

/// The key of record `seq` of session "s", as the log names it (the test reaches under the log to damage a value).
fn key(seq: u64) -> String {
    format!("records/s_s/{seq:020}")
}

#[test]
fn records_take_consecutive_numbers_per_session_and_come_back_in_order() {
    let r = MemoryRecords::new(MemoryBlobs::new());
    assert_eq!(r.append("s", "{\"a\":1}").unwrap(), 1);
    assert_eq!(r.append("s", "{\"a\":2}").unwrap(), 2);
    assert_eq!(r.append("other", "{\"a\":9}").unwrap(), 1, "per session");
    let l = r.list("s").unwrap();
    assert_eq!(
        l.records
            .iter()
            .map(|x| (x.seq, x.body.as_str()))
            .collect::<Vec<_>>(),
        [(1, "{\"a\":1}"), (2, "{\"a\":2}")]
    );
    assert!(l.torn.is_empty() && l.gaps.is_empty());
    assert!(r.list("never-used").unwrap().records.is_empty());
    assert!(matches!(
        r.append("", "x"),
        Err(agent_loop::RecordsError::BadSession)
    ));
    // Session names that differ only by case or punctuation do not share records.
    r.append("S", "{}").unwrap();
    assert_eq!(r.list("S").unwrap().records.len(), 1);
    assert_eq!(r.list("s").unwrap().records.len(), 2);
}

#[test]
fn a_resumed_log_over_the_same_blobs_continues_the_numbering() {
    let blobs = MemoryBlobs::new();
    let first = MemoryRecords::new(blobs.clone());
    first.append("s", "{}").unwrap();
    first.append("s", "{}").unwrap();
    drop(first);
    assert_eq!(RecordLog::new(blobs).append("s", "{}").unwrap(), 3);
}

#[test]
fn a_torn_value_is_reported_and_never_trusted() {
    let blobs = MemoryBlobs::new();
    let r = RecordLog::new(blobs.clone());
    r.append("s", "{\"n\":1}").unwrap();
    // Record 2 is torn: a half-written value (the hash line and part of the body), as a crash can leave on `fs`.
    assert!(blobs.create_if_absent(&key(2), b"0000".to_vec()).unwrap());
    // Record 3 has a well-formed hash line for a different body.
    assert!(
        blobs
            .create_if_absent(
                &key(3),
                format!("{}\n{{\"n\":3}}", "a".repeat(64)).into_bytes()
            )
            .unwrap()
    );
    r.append("s", "{\"n\":4}").unwrap();
    let l = r.list("s").unwrap();
    assert_eq!(l.torn, [2, 3]);
    assert_eq!(
        l.records.iter().map(|x| x.seq).collect::<Vec<_>>(),
        [1, 4],
        "torn values are not among the records"
    );
    assert!(l.gaps.is_empty(), "a torn value is present, not a gap");
}

#[test]
fn a_missing_record_in_the_middle_is_a_gap_and_a_lost_last_one_is_not_detectable() {
    let blobs = MemoryBlobs::new();
    let r = RecordLog::new(blobs.clone());
    // Seqs 1, 4, 5 present; 2 and 3 missing; and nothing starts at the very front if 1 is absent.
    for n in [1u64, 4, 5] {
        let body = format!("{{\"n\":{n}}}");
        assert!(
            blobs
                .create_if_absent(&key(n), seal_for_test(&body))
                .unwrap()
        );
    }
    let l = r.list("s").unwrap();
    assert_eq!(l.gaps, one(2..=3));
    assert!(l.torn.is_empty());

    // Missing from the front: 1 absent.
    let b2 = MemoryBlobs::new();
    assert!(b2.create_if_absent(&key(3), seal_for_test("{}")).unwrap());
    assert_eq!(RecordLog::new(b2).list("s").unwrap().gaps, one(1..=2));

    // Control for the limit: the last record lost whole leaves a clean-looking log.
    let b3 = MemoryBlobs::new();
    let r3 = RecordLog::new(b3.clone());
    r3.append("s", "{}").unwrap();
    r3.append("s", "{}").unwrap();
    let before = r3.list("s").unwrap();
    // Simulate the loss by listing a log that never got record 2.
    let b4 = MemoryBlobs::new();
    assert!(b4.create_if_absent(&key(1), seal_for_test("{}")).unwrap());
    let after = RecordLog::new(b4).list("s").unwrap();
    assert_eq!(before.records.len(), 2);
    assert!(after.torn.is_empty() && after.gaps.is_empty());
    assert_eq!(
        after.records.len(),
        1,
        "indistinguishable from a session that stopped earlier"
    );
}

/// The stored form, built independently of the library: `<sha256 hex>\n<body>`.
fn seal_for_test(body: &str) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let h: String = Sha256::digest(body.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("{h}\n{body}").into_bytes()
}

#[test]
fn the_stored_form_is_hash_newline_body() {
    // The format is part of the contract: a value written by the library verifies by an independent computation.
    let blobs = MemoryBlobs::new();
    RecordLog::new(blobs.clone())
        .append("s", "{\"x\":1}")
        .unwrap();
    let raw = blobs.read(&key(1)).unwrap().unwrap();
    assert_eq!(raw, seal_for_test("{\"x\":1}"));
}

#[test]
fn resume_facts_say_whether_a_writeback_finished_and_what_is_unknown() {
    let r = MemoryRecords::new(MemoryBlobs::new());
    // Nothing yet.
    assert_eq!(r.resume_facts("s").unwrap(), ResumeFacts::default());

    r.append_record("s", &opened()).unwrap();
    r.append_record("s", &intent("h1")).unwrap();
    // Intent without done: the state is unknown.
    let f = r.resume_facts("s").unwrap();
    assert_eq!(f.opened, Some(opened()));
    assert_eq!(f.unfinished, Some(intent("h1")));
    assert!(f.done.is_empty() && f.is_intact());

    // A done for a different set does not finish it.
    r.append_record("s", &done("other", "applied")).unwrap();
    assert_eq!(r.resume_facts("s").unwrap().unfinished, Some(intent("h1")));

    // Its own done does.
    r.append_record("s", &done("h1", "applied")).unwrap();
    let f = r.resume_facts("s").unwrap();
    assert_eq!(f.unfinished, None);
    assert_eq!(f.done, [done("other", "applied"), done("h1", "applied")]);

    // A later intent is unfinished again, and the latest opened wins.
    r.append_record("s", &intent("h2")).unwrap();
    let reopened = WorkspaceRecord::Opened {
        mode: "bind".into(),
        root: "C:/p".into(),
        policy_hash: "ph2".into(),
        base: None,
    };
    r.append_record("s", &reopened).unwrap();
    let f = r.resume_facts("s").unwrap();
    assert_eq!(f.unfinished, Some(intent("h2")));
    assert_eq!(f.opened, Some(reopened));
}

#[test]
fn damage_and_records_this_version_cannot_read_are_reported_in_the_facts() {
    let blobs = MemoryBlobs::new();
    let r = RecordLog::new(blobs.clone());
    r.append_record("s", &opened()).unwrap(); // 1
    // 2: hash-valid, but a newer format version.
    r.append("s", "{\"v\":2,\"kind\":\"workspace.opened\"}")
        .unwrap();
    // 3: hash-valid, an unknown kind.
    r.append("s", "{\"v\":1,\"kind\":\"something.new\"}")
        .unwrap();
    // 4: torn. 5: missing. 6: fine.
    assert!(blobs.create_if_absent(&key(4), b"junk".to_vec()).unwrap());
    assert!(
        blobs
            .create_if_absent(
                &key(6),
                seal_for_test(
                    "{\"v\":1,\"kind\":\"writeback.intent\",\"set_hash\":\"h\",\"paths\":[]}"
                )
            )
            .unwrap()
    );
    let f = r.resume_facts("s").unwrap();
    assert_eq!(f.torn, [4]);
    assert_eq!(f.gaps, one(5..=5));
    assert_eq!(f.unknown, [2, 3]);
    assert!(!f.is_intact());
    assert_eq!(f.opened, Some(opened()), "readable records still count");
    assert_eq!(
        f.unfinished,
        Some(WorkspaceRecord::Intent {
            set_hash: "h".into(),
            paths: vec![]
        })
    );
}

#[cfg(feature = "persistant")]
mod persist {
    use super::*;
    use agent_loop::{PersistBlobs, records_needs};
    use persistant::{Config, blocking::Store};

    struct Dir(std::path::PathBuf);
    impl Dir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "agent-loop-records-test-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fs_store(d: &Dir) -> Store {
        // No `atomic_write_dir`: create_if_absent needs it unset to be exclusive (records_needs docs).
        Store::open(
            Config::Fs {
                root: d.0.clone(),
                atomic_write_dir: None,
            },
            records_needs(),
        )
        .unwrap()
    }

    #[test]
    fn the_log_works_over_a_real_fs_store_and_survives_reopening_it() {
        let d = Dir::new();
        let r = RecordLog::new(PersistBlobs::new(fs_store(&d)));
        r.append_record("s", &opened()).unwrap();
        r.append_record("s", &intent("h1")).unwrap();
        r.append_record("other", &opened()).unwrap();
        drop(r);

        // A new store over the same directory (a restart) sees the records and continues the numbering.
        let r = RecordLog::new(PersistBlobs::new(fs_store(&d)));
        let f = r.resume_facts("s").unwrap();
        assert_eq!(f.opened, Some(opened()));
        assert_eq!(f.unfinished, Some(intent("h1")));
        assert!(f.is_intact());
        assert_eq!(r.append_record("s", &done("h1", "applied")).unwrap(), 3);
        assert_eq!(r.resume_facts("s").unwrap().unfinished, None);
        assert_eq!(r.list("other").unwrap().records.len(), 1);
    }

    #[test]
    fn a_value_damaged_on_disk_is_reported_torn() {
        let d = Dir::new();
        let r = RecordLog::new(PersistBlobs::new(fs_store(&d)));
        r.append_record("s", &opened()).unwrap();
        r.append_record("s", &intent("h1")).unwrap();
        // Damage record 2 behind the log's back, as a crash part-way through a write would.
        let path = d.0.join("records").join("s_s").join(format!("{:020}", 2));
        assert!(path.exists(), "{}", path.display());
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
        let f = r.resume_facts("s").unwrap();
        assert_eq!(f.torn, [2]);
        assert_eq!(f.unfinished, None, "a torn intent is never trusted");
        assert_eq!(f.opened, Some(opened()));
    }

    #[test]
    fn a_store_that_does_not_declare_the_needs_fails_instead_of_losing_records() {
        use persistant::{Need, Needs};
        let d = Dir::new();
        let weak = Store::open(
            Config::Fs {
                root: d.0.clone(),
                atomic_write_dir: None,
            },
            Needs::new().with(Need::Read).with(Need::Write),
        )
        .unwrap();
        let r = RecordLog::new(PersistBlobs::new(weak));
        assert!(matches!(
            r.append("s", "{}"),
            Err(agent_loop::RecordsError::Backend(_))
        ));
    }
}
