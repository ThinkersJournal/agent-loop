// SPDX-License-Identifier: MIT OR Apache-2.0
//! Disk-backed store and inbox: round trip, the sequence rule across two "processes", safe names, stray files, and a real
//! driver that stops, is dropped, and resumes from files with a durable message.
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use agent_loop::{
    Driver, End, FileInbox, FileStore, Inbox, Model, SessionStore, StoreError, ToolHost,
    ToolOutcome,
};
use rig_core::completion::{CompletionRequest, ProviderCapabilities, ToolDefinition};
use rig_core::error::ProviderError;
use rig_core::streaming::CompletionStream;
use rig_core::test_utils::{MockCompletionModel, MockStreamEvent as E};

/// A fresh directory under the system temp dir, removed on drop.
struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "agent-loop-file-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&p);
        Self(p)
    }
    fn sub(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
    fn files(&self, name: &str) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(self.sub(name))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        v.sort();
        v
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn store_round_trips_and_persists_across_instances() {
    let d = Dir::new();
    let mut a = FileStore::new(d.sub("s")).unwrap();
    assert_eq!(a.load("run-1").unwrap(), None, "a new session has nothing");
    assert_eq!(a.save("run-1", 0, "{\"v\":1}").unwrap(), 1);
    assert_eq!(a.save("run-1", 1, "{\"v\":2}").unwrap(), 2);
    drop(a);
    let b = FileStore::new(d.sub("s")).unwrap();
    assert_eq!(b.load("run-1").unwrap(), Some((2, "{\"v\":2}".into())));
    assert_eq!(b.load("run-2").unwrap(), None, "sessions are separate");
}

#[test]
fn of_two_writers_that_read_the_same_sequence_the_second_is_refused() {
    let d = Dir::new();
    // Two instances on one directory stand for two processes.
    let mut p1 = FileStore::new(d.sub("s")).unwrap();
    let mut p2 = FileStore::new(d.sub("s")).unwrap();
    assert_eq!(p1.save("x", 0, "from p1").unwrap(), 1);
    assert_eq!(
        p2.save("x", 0, "from p2"),
        Err(StoreError::Conflict {
            expected: 0,
            actual: 1
        })
    );
    // The loser wrote nothing: the stored snapshot is the winner's.
    assert_eq!(p2.load("x").unwrap(), Some((1, "from p1".into())));
    // After re-reading, the loser can write the next sequence.
    assert_eq!(p2.save("x", 1, "from p2").unwrap(), 2);
}

#[test]
fn only_the_newest_two_snapshots_are_kept_and_no_temp_files_remain() {
    let d = Dir::new();
    let mut s = FileStore::new(d.sub("s")).unwrap();
    for i in 0..5u64 {
        s.save("x", i, &format!("{i}")).unwrap();
    }
    let files = d.files("s");
    assert_eq!(files.len(), 2, "{files:?}");
    assert!(files.iter().all(|f| f.ends_with(".snap")), "{files:?}");
    assert_eq!(s.load("x").unwrap().unwrap().0, 5);
}

#[test]
fn session_ids_cannot_escape_the_directory_or_collide() {
    let d = Dir::new();
    let mut s = FileStore::new(d.sub("s")).unwrap();
    for (i, id) in ["a", "a.b", "../evil", "a/b", "A", "é"].iter().enumerate() {
        s.save(id, 0, &format!("payload-{i}")).unwrap();
    }
    for (i, id) in ["a", "a.b", "../evil", "a/b", "A", "é"].iter().enumerate() {
        assert_eq!(
            s.load(id).unwrap(),
            Some((1, format!("payload-{i}"))),
            "{id:?}"
        );
    }
    // Everything is inside `s`; nothing was created next to it.
    assert_eq!(d.files("s").len(), 6);
    let outside: Vec<_> = std::fs::read_dir(&d.0)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(outside.len(), 1, "{outside:?}");
    // Windows device names are ordinary sessions, not devices; and ids that differ only in case stay apart.
    for id in ["con", "nul", "aux", "CON", "Nul"] {
        s.save(id, 0, id).unwrap();
    }
    for id in ["con", "nul", "aux", "CON", "Nul"] {
        assert_eq!(s.load(id).unwrap(), Some((1, id.to_string())), "{id:?}");
    }
    assert_eq!(d.files("s").len(), 11);
    // An empty id is refused, not mapped to a file called ".00001.snap".
    assert!(matches!(s.save("", 0, "x"), Err(StoreError::Backend(_))));
}

#[test]
fn stray_and_half_written_files_are_ignored() {
    let d = Dir::new();
    let mut s = FileStore::new(d.sub("s")).unwrap();
    s.save("tmp", 0, "good").unwrap();
    // A crashed writer's temp file, and unrelated junk, sit beside the snapshots.
    std::fs::write(d.sub("s").join("tmp.99999.0"), "half written garbage").unwrap();
    std::fs::write(d.sub("s").join("tmp.notanumber.snap"), "junk").unwrap();
    std::fs::write(d.sub("s").join("readme.txt"), "junk").unwrap();
    assert_eq!(s.load("tmp").unwrap(), Some((1, "good".into())));
    assert_eq!(s.save("tmp", 1, "better").unwrap(), 2);
}

#[test]
fn inbox_orders_acknowledges_and_never_reuses_an_id() {
    let d = Dir::new();
    let ib = FileInbox::new(d.sub("i")).unwrap();
    assert!(ib.pending("s").unwrap().is_empty());
    let a = ib.append("s", "first").unwrap();
    let b = ib.append("s", "second\nwith a newline").unwrap();
    assert!(b > a);
    let other = ib.append("t", "other session").unwrap();
    assert_eq!(other, 1, "ids are per session");
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
    ib.ack("s", &[b]).unwrap();
    assert!(ib.pending("s").unwrap().is_empty());
    // Everything acknowledged, and a new instance (a restart) still does not reuse an id.
    drop(ib);
    let again = FileInbox::new(d.sub("i")).unwrap();
    let c = again.append("s", "third").unwrap();
    assert!(c > b, "{c} after {b}");
    assert_eq!(again.pending("s").unwrap().len(), 1);
    assert!(matches!(
        again.append("", "x"),
        Err(agent_loop::InboxError::Backend(_))
    ));
}

#[test]
fn prune_removes_acknowledged_messages_but_keeps_the_newest_so_ids_are_not_reused() {
    let d = Dir::new();
    let ib = FileInbox::new(d.sub("i")).unwrap();
    assert_eq!(ib.prune("none").unwrap(), 0, "no such session");
    let ids: Vec<u64> = ["a", "b", "c", "d"]
        .iter()
        .map(|t| ib.append("s", t).unwrap())
        .collect();
    // a, b and c acknowledged; d (the newest) is not. Pruning removes the three and never touches d.
    ib.ack("s", &ids[..3]).unwrap();
    assert_eq!(ib.prune("s").unwrap(), 3);
    let p = ib.pending("s").unwrap();
    assert_eq!(p.iter().map(|q| q.text.as_str()).collect::<Vec<_>>(), ["d"]);
    let sdir = std::fs::read_dir(d.sub("i"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.is_dir())
        .unwrap();
    let left = |dir: &std::path::Path| {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        v.sort();
        v
    };
    assert_eq!(left(&sdir), [format!("{:020}.msg", ids[3])]);

    // Acknowledge the newest too: it is kept, with its marker, so the next id is still higher than every earlier one.
    ib.ack("s", &ids[3..]).unwrap();
    assert_eq!(ib.prune("s").unwrap(), 0, "the newest is kept");
    assert!(ib.pending("s").unwrap().is_empty());
    let next = ib.append("s", "e").unwrap();
    assert!(next > ids[3], "{next} must not reuse {}", ids[3]);
    // A restart changes nothing, and an ack of a pruned id is ignored.
    let again = FileInbox::new(d.sub("i")).unwrap();
    again.ack("s", &[ids[0]]).unwrap();
    assert_eq!(again.pending("s").unwrap().len(), 1);
    assert_eq!(
        again.prune("s").unwrap(),
        1,
        "d is now prunable, e is newest"
    );
    assert_eq!(left(&sdir), [format!("{next:020}.msg")]);
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
fn spec() -> agent_loop::RunConfig {
    agent_loop::RunConfig::new().max_turns(4)
}
fn text_turn() -> Mock {
    Mock(MockCompletionModel::from_stream_turns([vec![
        E::text("done"),
        E::final_response_with_total_tokens(1),
    ]]))
}

#[tokio::test]
async fn a_driver_resumes_from_files_with_a_durable_message() {
    let d = Dir::new();
    // First "process": the supervisor queues a message and asks for a stop before the first model call, so the run is saved
    // holding the message and nothing else has happened.
    {
        let mut drv = Driver::new(
            "sess",
            text_turn(),
            NoTools,
            FileStore::new(d.sub("store")).unwrap(),
            spec(),
        )
        .with_inbox(Arc::new(FileInbox::new(d.sub("inbox")).unwrap()));
        drv.control().inject_durable("FROM-DISK").unwrap();
        drv.control().stop_soft();
        let mut run = drv.open("the task", 4).unwrap();
        assert_eq!(drv.drive(&mut run).await.unwrap(), End::Stopped);
    }
    // Second "process": new instances on the same directories finish the run, and the model sees the message.
    let m = text_turn();
    let inbox = Arc::new(FileInbox::new(d.sub("inbox")).unwrap());
    let mut drv = Driver::new(
        "sess",
        m.clone(),
        NoTools,
        FileStore::new(d.sub("store")).unwrap(),
        spec(),
    )
    .with_inbox(inbox.clone());
    let mut run = drv.open("ignored", 4).unwrap();
    assert_eq!(drv.drive(&mut run).await.unwrap(), End::Done("done".into()));
    let req = format!("{:?}", m.0.requests()[0]);
    assert!(
        req.contains("FROM-DISK") && req.contains("the task"),
        "{req}"
    );
    assert!(inbox.pending("sess").unwrap().is_empty());
}
