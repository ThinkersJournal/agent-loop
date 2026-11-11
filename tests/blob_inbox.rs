// SPDX-License-Identifier: MIT OR Apache-2.0
//! `BlobInbox`: the `FileInbox` behaviour (order, acknowledgement, ids never reused, prune) as one suite run against both, plus what is
//! new here: a damaged message is skipped and listed, a damaged marker does not acknowledge, and (with the `persistant` feature) the
//! same over a real `fs` store.
use agent_loop::{BlobInbox, FileInbox, Inbox, InboxError, KeyedBlobs, MemoryBlobs};

/// The behaviour every inbox must have. `reopen` makes a second instance over the same storage (a restart).
fn shared_suite<I: Inbox>(open: &dyn Fn() -> I) {
    let ib = open();
    assert!(ib.pending("s").unwrap().is_empty());
    let a = ib.append("s", "first").unwrap();
    let b = ib.append("s", "second\nwith a newline").unwrap();
    assert!(b > a);
    assert_eq!(
        ib.append("t", "other session").unwrap(),
        1,
        "ids are per session"
    );
    let p = ib.pending("s").unwrap();
    assert_eq!(
        p.iter().map(|q| q.text.as_str()).collect::<Vec<_>>(),
        ["first", "second\nwith a newline"]
    );
    ib.ack("s", &[a, 9999]).unwrap();
    assert_eq!(
        ib.pending("s").unwrap().len(),
        1,
        "an unknown id is ignored"
    );
    ib.ack("s", &[a]).unwrap(); // acknowledging twice is harmless
    ib.ack("s", &[b]).unwrap();
    assert!(ib.pending("s").unwrap().is_empty());
    drop(ib);
    let again = open();
    let c = again.append("s", "third").unwrap();
    assert!(c > b, "{c} after {b}");
    assert_eq!(again.pending("s").unwrap().len(), 1);
    assert!(matches!(again.append("", "x"), Err(InboxError::Backend(_))));
}

#[test]
fn file_inbox_passes_the_shared_suite() {
    let p = std::env::temp_dir().join(format!("agent-loop-blobinbox-file-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    shared_suite(&|| FileInbox::new(&p).unwrap());
    let _ = std::fs::remove_dir_all(&p);
}

#[test]
fn blob_inbox_passes_the_shared_suite() {
    let blobs = MemoryBlobs::new();
    shared_suite(&|| BlobInbox::new(blobs.clone()));
}

fn msg_key(id: u64) -> String {
    format!("inbox/s_s/{id:020}.msg")
}
fn ack_key(id: u64) -> String {
    format!("inbox/s_s/{id:020}.ack")
}

#[test]
fn prune_removes_acknowledged_messages_but_keeps_the_newest_and_the_unacknowledged() {
    let ib = BlobInbox::new(MemoryBlobs::new());
    let ids: Vec<u64> = (0..4)
        .map(|i| ib.append("s", &format!("m{i}")).unwrap())
        .collect();
    ib.ack("s", &[ids[0], ids[1], ids[3]]).unwrap();
    assert_eq!(
        ib.prune("s").unwrap(),
        2,
        "0 and 1 go; 2 is unacknowledged; 3 is the newest"
    );
    let p = ib.pending("s").unwrap();
    assert_eq!(p.iter().map(|q| q.id).collect::<Vec<_>>(), [ids[2]]);
    // The id of the newest was kept, so it is not reused after everything is acknowledged and pruned.
    ib.ack("s", &[ids[2]]).unwrap();
    ib.prune("s").unwrap();
    assert!(ib.append("s", "next").unwrap() > ids[3]);
}

#[test]
fn a_damaged_message_is_skipped_and_listed_and_its_id_is_not_reused() {
    let blobs = MemoryBlobs::new();
    let ib = BlobInbox::new(blobs.clone());
    let a = ib.append("s", "good").unwrap();
    // What a crash part-way through an in-place write leaves: a name with no valid hash line.
    assert!(
        blobs
            .create_if_absent(&msg_key(a + 1), b"half a mess".to_vec())
            .unwrap()
    );
    assert_eq!(
        ib.pending("s").unwrap().len(),
        1,
        "the damaged message is not delivered"
    );
    assert_eq!(ib.torn("s").unwrap(), [a + 1]);
    assert_eq!(
        ib.append("s", "after").unwrap(),
        a + 2,
        "the damaged id still counts"
    );
}

#[test]
fn a_damaged_marker_does_not_acknowledge_and_a_later_ack_replaces_it() {
    let blobs = MemoryBlobs::new();
    let ib = BlobInbox::new(blobs.clone());
    let a = ib.append("s", "m").unwrap();
    assert!(blobs.create_if_absent(&ack_key(a), b"x".to_vec()).unwrap());
    assert_eq!(
        ib.pending("s").unwrap().len(),
        1,
        "delivered again rather than lost"
    );
    ib.ack("s", &[a]).unwrap();
    assert!(ib.pending("s").unwrap().is_empty());
}

#[test]
fn prune_cleans_markers_whose_message_is_gone() {
    let blobs = MemoryBlobs::new();
    let ib = BlobInbox::new(blobs.clone());
    ib.append("s", "m").unwrap();
    assert!(blobs.create_if_absent(&ack_key(77), b"x".to_vec()).unwrap());
    ib.prune("s").unwrap();
    assert!(blobs.read(&ack_key(77)).unwrap().is_none());
}

/// A backend that cannot delete (the trait default).
struct NoDelete(MemoryBlobs);
impl KeyedBlobs for NoDelete {
    fn create_if_absent(&self, k: &str, b: Vec<u8>) -> Result<bool, agent_loop::RecordsError> {
        self.0.create_if_absent(k, b)
    }
    fn list(&self, p: &str) -> Result<Vec<String>, agent_loop::RecordsError> {
        self.0.list(p)
    }
    fn read(&self, k: &str) -> Result<Option<Vec<u8>>, agent_loop::RecordsError> {
        self.0.read(k)
    }
}

#[test]
fn a_backend_that_cannot_delete_still_appends_and_acks_and_prune_says_it_cannot() {
    let ib = BlobInbox::new(NoDelete(MemoryBlobs::new()));
    let a = ib.append("s", "m1").unwrap();
    let _ = ib.append("s", "m2").unwrap();
    ib.ack("s", &[a]).unwrap();
    assert_eq!(ib.pending("s").unwrap().len(), 1);
    assert!(matches!(ib.prune("s"), Err(InboxError::Backend(m)) if m.contains("cannot delete")));
}

#[cfg(feature = "persistant")]
mod persist {
    use super::*;
    use agent_loop::{PersistBlobs, records_needs};
    use persistant::{Config, blocking::Store};

    fn open(root: &std::path::Path) -> PersistBlobs {
        // No `atomic_write_dir`: create-if-absent needs it unset to be exclusive (see `records_needs`).
        PersistBlobs::new(
            Store::open(
                Config::Fs {
                    root: root.to_path_buf(),
                    atomic_write_dir: None,
                },
                records_needs(),
            )
            .unwrap(),
        )
    }

    #[test]
    fn the_inbox_passes_the_shared_suite_and_prunes_over_a_real_fs_store() {
        let root = std::env::temp_dir().join(format!(
            "agent-loop-blobinbox-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        shared_suite(&|| BlobInbox::new(open(&root)));
        let ib = BlobInbox::new(open(&root));
        for p in ib.pending("s").unwrap() {
            ib.ack("s", &[p.id]).unwrap();
        }
        let before = ib.pending("t").unwrap().len();
        assert!(ib.prune("s").is_ok());
        assert_eq!(
            ib.pending("t").unwrap().len(),
            before,
            "sessions are separate"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
