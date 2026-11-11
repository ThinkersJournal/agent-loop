// SPDX-License-Identifier: MIT OR Apache-2.0
//! `BlobStore`: the optimistic-sequence `SessionStore` rules over any `KeyedBlobs`, hash-checked snapshots with fallback to the one
//! before, a damaged claim that fails closed until `clear_torn`, and (with the `persistant` feature) the same over a real `fs` store.
use std::sync::Arc;

use agent_loop::{
    BlobStore, Driver, End, KeyedBlobs, MemoryBlobs, Model, RecordsError, SessionStore, StoreError,
    ToolHost, ToolOutcome,
};
use rig_core::completion::{CompletionRequest, ProviderCapabilities, ToolDefinition};
use rig_core::error::ProviderError;
use rig_core::streaming::CompletionStream;
use rig_core::test_utils::{MockCompletionModel, MockStreamEvent as E};

/// The key of snapshot `seq` of session "s" (the test reaches under the store to damage a value).
fn key(seq: u64) -> String {
    format!("snap/s_s/{seq:020}")
}

fn damaged(blobs: &impl KeyedBlobs, seq: u64) {
    // Not a hash line and a body: what a crash part-way through an in-place write can leave.
    assert!(
        blobs
            .create_if_absent(&key(seq), b"half a snap".to_vec())
            .unwrap()
    );
}

#[test]
fn a_new_session_loads_nothing_and_saves_take_consecutive_numbers() {
    let mut s = BlobStore::new(MemoryBlobs::new());
    assert_eq!(s.load("s").unwrap(), None);
    assert_eq!(s.save("s", 0, "one").unwrap(), 1);
    assert_eq!(s.save("s", 1, "two").unwrap(), 2);
    assert_eq!(s.load("s").unwrap(), Some((2, "two".into())));
    // Sessions are separate, and names that differ only by case do not share snapshots.
    assert_eq!(s.load("other").unwrap(), None);
    assert_eq!(s.save("S", 0, "upper").unwrap(), 1);
    assert_eq!(s.load("s").unwrap(), Some((2, "two".into())));
    assert!(matches!(s.save("", 0, "x"), Err(StoreError::Backend(_))));
}

#[test]
fn of_two_writers_that_read_the_same_sequence_the_second_is_refused() {
    let blobs = MemoryBlobs::new();
    let (mut a, mut b) = (BlobStore::new(blobs.clone()), BlobStore::new(blobs));
    assert_eq!(a.save("s", 0, "a1").unwrap(), 1);
    assert_eq!(
        b.save("s", 0, "b1"),
        Err(StoreError::Conflict {
            expected: 0,
            actual: 1
        })
    );
    // A writer ahead of the store is a conflict too, and the stored snapshot is untouched.
    assert_eq!(
        b.save("s", 5, "b5"),
        Err(StoreError::Conflict {
            expected: 5,
            actual: 1
        })
    );
    assert_eq!(b.load("s").unwrap(), Some((1, "a1".into())));
}

#[test]
fn only_the_newest_two_snapshots_are_kept() {
    let blobs = MemoryBlobs::new();
    let mut s = BlobStore::new(blobs.clone());
    for i in 0..4u64 {
        s.save("s", i, &format!("v{i}")).unwrap();
    }
    let mut left = blobs.list("snap/s_s/").unwrap();
    left.sort();
    assert_eq!(left, [key(3), key(4)]);
}

#[test]
fn a_damaged_newest_snapshot_falls_back_and_a_damaged_claim_fails_closed_until_cleared() {
    let blobs = MemoryBlobs::new();
    let mut s = BlobStore::new(blobs.clone());
    s.save("s", 0, "one").unwrap();
    s.save("s", 1, "two").unwrap();
    // A crash left snapshot 3 half written.
    damaged(&blobs, 3);
    assert_eq!(s.torn("s").unwrap(), [3]);
    assert_eq!(
        s.load("s").unwrap(),
        Some((2, "two".into())),
        "falls back to the newest verified snapshot"
    );
    // The next save would take the damaged name: refused, nothing deleted, the error says what to do.
    let err = s.save("s", 2, "three").unwrap_err();
    assert!(
        matches!(&err, StoreError::Backend(m) if m.contains("clear_torn") && m.contains('3')),
        "{err}"
    );
    assert_eq!(s.torn("s").unwrap(), [3], "fail closed: not deleted");
    // The host knows no other writer exists.
    assert_eq!(s.clear_torn("s").unwrap(), 1);
    assert_eq!(s.save("s", 2, "three").unwrap(), 3);
    assert_eq!(s.load("s").unwrap(), Some((3, "three".into())));
    assert_eq!(s.clear_torn("s").unwrap(), 0);
}

#[test]
fn a_value_with_a_wrong_hash_is_damaged_even_when_it_looks_well_formed() {
    let blobs = MemoryBlobs::new();
    let s = BlobStore::new(blobs.clone());
    assert!(
        blobs
            .create_if_absent(&key(1), format!("{}\nsnap", "a".repeat(64)).into_bytes())
            .unwrap()
    );
    assert_eq!(s.torn("s").unwrap(), [1]);
    // Nothing verifies: loading says so instead of returning nothing.
    assert!(matches!(s.load("s"), Err(StoreError::Backend(m)) if m.contains("damaged")));
}

#[test]
fn a_new_session_over_leftover_damage_is_refused_not_overwritten() {
    let blobs = MemoryBlobs::new();
    damaged(&blobs, 1);
    let mut s = BlobStore::new(blobs);
    let err = s.save("s", 0, "x").unwrap_err();
    assert!(
        matches!(&err, StoreError::Backend(m) if m.contains("clear_torn")),
        "{err}"
    );
    assert_eq!(s.clear_torn("s").unwrap(), 1);
    assert_eq!(s.save("s", 0, "x").unwrap(), 1);
}

/// A backend written before `delete` existed: it takes the default, which answers "cannot delete".
struct NoDelete(MemoryBlobs);
impl KeyedBlobs for NoDelete {
    fn create_if_absent(&self, k: &str, b: Vec<u8>) -> Result<bool, RecordsError> {
        self.0.create_if_absent(k, b)
    }
    fn list(&self, p: &str) -> Result<Vec<String>, RecordsError> {
        self.0.list(p)
    }
    fn read(&self, k: &str) -> Result<Option<Vec<u8>>, RecordsError> {
        self.0.read(k)
    }
}

#[test]
fn a_backend_that_cannot_delete_still_saves_and_clear_torn_says_it_cannot() {
    let inner = MemoryBlobs::new();
    let mut s = BlobStore::new(NoDelete(inner.clone()));
    for i in 0..3u64 {
        // Pruning is best effort: an old snapshot is left behind, the save still succeeds.
        assert_eq!(s.save("s", i, "v").unwrap(), i + 1);
    }
    assert_eq!(inner.list("snap/s_s/").unwrap().len(), 3);
    damaged(&inner, 4);
    assert!(
        matches!(s.clear_torn("s"), Err(StoreError::Backend(m)) if m.contains("cannot delete"))
    );
}

#[test]
fn the_journal_is_kept_per_sequence_the_newest_two_survive_and_a_damaged_one_is_skipped() {
    let blobs = MemoryBlobs::new();
    let mut s = BlobStore::new(blobs.clone());
    assert_eq!(s.load_journal("s").unwrap(), None);
    for i in 0..4u64 {
        let seq = s.save("s", i, "snap").unwrap();
        s.save_journal("s", seq, &format!("{{\"v\":{seq}}}"))
            .unwrap();
    }
    assert_eq!(s.load_journal("s").unwrap(), Some((4, "{\"v\":4}".into())));
    assert_eq!(
        blobs.list("journal/s_s/").unwrap().len(),
        2,
        "newest two kept"
    );
    // A retry of the same text is fine; different text for a taken sequence is refused and nothing is overwritten.
    s.save_journal("s", 4, "{\"v\":4}").unwrap();
    assert!(matches!(
        s.save_journal("s", 4, "other"),
        Err(StoreError::Backend(m)) if m.contains("different content")
    ));
    assert_eq!(s.load_journal("s").unwrap(), Some((4, "{\"v\":4}".into())));
    // A crash that left a damaged newest blob: load falls back, a save over it fails closed, clear_torn removes it.
    assert!(
        blobs
            .create_if_absent("journal/s_s/00000000000000000005", b"half".to_vec())
            .unwrap()
    );
    assert_eq!(s.load_journal("s").unwrap(), Some((4, "{\"v\":4}".into())));
    assert!(matches!(
        s.save_journal("s", 5, "{}"),
        Err(StoreError::Backend(m)) if m.contains("damaged journal")
    ));
    assert_eq!(s.clear_torn("s").unwrap(), 1);
    s.save_journal("s", 5, "{}").unwrap();
    assert_eq!(s.load_journal("s").unwrap(), Some((5, "{}".into())));
    assert_eq!(
        s.load_journal("other").unwrap(),
        None,
        "sessions are separate"
    );
}

#[test]
fn a_real_journal_round_trips_through_the_store() {
    use agent_loop::Journal;
    let j = Journal::new(NoTools);
    let mut s = BlobStore::new(MemoryBlobs::new());
    let seq = s.save("s", 0, "snap").unwrap();
    s.save_journal("s", seq, &j.to_json()).unwrap();
    let (jseq, text) = s.load_journal("s").unwrap().unwrap();
    assert_eq!(jseq, seq);
    assert!(Journal::from_json(&text, NoTools).is_ok());
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
                "agent-loop-blobstore-test-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn open(d: &Dir) -> PersistBlobs {
        // No `atomic_write_dir`: create-if-absent needs it unset to be exclusive (see `records_needs`).
        PersistBlobs::new(
            Store::open(
                Config::Fs {
                    root: d.0.clone(),
                    atomic_write_dir: None,
                },
                records_needs(),
            )
            .unwrap(),
        )
    }

    #[test]
    fn the_store_works_over_a_real_fs_store_and_survives_reopening_it() {
        let d = Dir::new();
        let mut s = BlobStore::new(open(&d));
        for i in 0..4u64 {
            assert_eq!(s.save("s", i, &format!("v{i}")).unwrap(), i + 1);
        }
        drop(s);
        // A restart: a new store over the same directory sees the newest snapshot, and old ones were pruned.
        let mut s = BlobStore::new(open(&d));
        assert_eq!(s.load("s").unwrap(), Some((4, "v3".into())));
        let on_disk = std::fs::read_dir(d.0.join("snap").join("s_s"))
            .unwrap()
            .count();
        assert_eq!(on_disk, 2);
        // A second writer that read an older sequence is refused.
        let mut other = BlobStore::new(open(&d));
        assert_eq!(
            other.save("s", 3, "late"),
            Err(StoreError::Conflict {
                expected: 3,
                actual: 4
            })
        );
        assert_eq!(s.save("s", 4, "v4").unwrap(), 5);
    }

    #[test]
    fn a_snapshot_damaged_on_disk_falls_back_and_can_be_cleared() {
        let d = Dir::new();
        let mut s = BlobStore::new(open(&d));
        s.save("s", 0, "one").unwrap();
        s.save("s", 1, "two").unwrap();
        // Damage snapshot 2 behind the store's back: flip its last byte, so the hash line is intact and the length is unchanged,
        // and only the hash check can tell.
        let path = d.0.join("snap").join("s_s").join(format!("{:020}", 2));
        let mut bytes = std::fs::read(&path).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(s.torn("s").unwrap(), [2]);
        assert_eq!(s.load("s").unwrap(), Some((1, "one".into())));
        assert!(
            s.save("s", 1, "x").is_err(),
            "the damaged name blocks the save"
        );
        assert_eq!(s.clear_torn("s").unwrap(), 1);
        assert_eq!(s.save("s", 1, "two again").unwrap(), 2);
        assert_eq!(s.load("s").unwrap(), Some((2, "two again".into())));
    }
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
struct NoTools;
impl ToolHost for NoTools {
    fn definitions(&self) -> Vec<Arc<ToolDefinition>> {
        vec![]
    }
    async fn call(&mut self, _: &str, _: &str, _: &serde_json::Value) -> ToolOutcome {
        ToolOutcome::error("no tools")
    }
}
fn text_turn() -> Mock {
    Mock(MockCompletionModel::from_stream_turns([vec![
        E::text("done"),
        E::final_response_with_total_tokens(1),
    ]]))
}

#[tokio::test]
async fn a_driver_stops_and_resumes_from_a_blob_store() {
    let blobs = MemoryBlobs::new();
    let cfg = || agent_loop::RunConfig::new().max_turns(4);
    // First "process": stop before the first model call, so the run is saved and nothing else has happened.
    {
        let mut drv = Driver::new(
            "sess",
            text_turn(),
            NoTools,
            BlobStore::new(blobs.clone()),
            cfg(),
        );
        drv.control().stop_soft();
        let mut run = drv.open("the task", 4).unwrap();
        assert_eq!(drv.drive(&mut run).await.unwrap(), End::Stopped);
    }
    // Second "process": a new store over the same blobs resumes and finishes; the model sees the original task.
    let m = text_turn();
    let mut drv = Driver::new(
        "sess",
        m.clone(),
        NoTools,
        BlobStore::new(blobs.clone()),
        cfg(),
    );
    let mut run = drv.open("ignored", 4).unwrap();
    assert_eq!(drv.drive(&mut run).await.unwrap(), End::Done("done".into()));
    assert!(format!("{:?}", m.0.requests()[0]).contains("the task"));
    // The run's snapshot is hash-checked, and nothing is damaged.
    assert!(BlobStore::new(blobs).torn("sess").unwrap().is_empty());
}
