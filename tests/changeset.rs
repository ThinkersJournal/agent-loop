// SPDX-License-Identifier: MIT OR Apache-2.0
//! B2 change set: diff, holds (protected, delete, build script), refused paths, and an apply that checks everything first.
use std::collections::{BTreeMap, BTreeSet};

use agent_loop::{ApplyError, Kind, Manifest, RealTree, Reason, WorkCopy, apply, diff, hash_bytes};

#[derive(Clone, Default)]
struct Tree {
    files: BTreeMap<String, Vec<u8>>,
    fail_on: Option<String>,
    writes: Vec<String>,
}
impl Tree {
    fn of(files: &[(&str, &str)]) -> Self {
        Self {
            files: files
                .iter()
                .map(|(p, b)| (p.to_string(), b.as_bytes().to_vec()))
                .collect(),
            ..Self::default()
        }
    }
    fn manifest(&self) -> Manifest {
        Manifest::from_files(self.files.iter().map(|(p, b)| (p.as_str(), b.as_slice())))
    }
}
impl RealTree for Tree {
    fn hash_of(&self, path: &str) -> Option<String> {
        self.files.get(path).map(|b| hash_bytes(b))
    }
    fn write(&mut self, path: &str, bytes: &[u8]) -> Result<(), String> {
        if self.fail_on.as_deref() == Some(path) {
            return Err("disk full".into());
        }
        self.writes.push(path.to_string());
        self.files.insert(path.to_string(), bytes.to_vec());
        Ok(())
    }
    fn remove(&mut self, path: &str) -> Result<(), String> {
        self.files.remove(path);
        Ok(())
    }
}
impl WorkCopy for Tree {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        self.files.get(path).cloned()
    }
}

fn none() -> BTreeSet<String> {
    BTreeSet::new()
}
fn only(p: &str) -> BTreeSet<String> {
    [p.to_string()].into()
}

#[test]
fn diff_finds_add_modify_delete_and_skips_unchanged() {
    let base = Tree::of(&[("a.rs", "1"), ("b.rs", "2"), ("c.rs", "3")]);
    let work = Tree::of(&[("a.rs", "1"), ("b.rs", "22"), ("d.rs", "4")]);
    let cs = diff(&base.manifest(), &work.manifest());
    let got: Vec<(&str, Kind)> = cs
        .changes
        .iter()
        .map(|c| (c.path.as_str(), c.kind))
        .collect();
    assert_eq!(
        got,
        [
            ("b.rs", Kind::Modify),
            ("c.rs", Kind::Delete),
            ("d.rs", Kind::Add)
        ]
    );
    assert!(cs.refused.is_empty());
    // Control: identical trees give an empty set.
    assert!(diff(&base.manifest(), &base.manifest()).is_empty());
}

#[test]
fn plain_edits_need_no_approval_and_apply() {
    let base = Tree::of(&[("src/a.rs", "1")]);
    let work = Tree::of(&[("src/a.rs", "2"), ("src/new.rs", "n")]);
    let cs = diff(&base.manifest(), &work.manifest());
    assert!(cs.needing_approval().is_empty());
    let mut real = base.clone();
    let done = apply(&cs, &cs.hash(), &none(), &work, &mut real).unwrap();
    assert_eq!(done, ["src/a.rs", "src/new.rs"]);
    assert_eq!(real.manifest(), work.manifest());
}

#[test]
fn protected_deletes_and_build_scripts_are_held_until_approved_by_name() {
    let base = Tree::of(&[
        (".github/workflows/ci.yml", "x"),
        ("CLAUDE.md", "x"),
        ("build.rs", "x"),
        ("old.rs", "x"),
        ("sub/CLAUDE.md", "x"),
    ]);
    let work = Tree::of(&[
        (".github/workflows/ci.yml", "y"),
        ("CLAUDE.md", "y"),
        ("build.rs", "y"),
        ("sub/CLAUDE.md", "y"),
    ]);
    let cs = diff(&base.manifest(), &work.manifest());
    let holds = |p: &str| {
        cs.changes
            .iter()
            .find(|c| c.path == p)
            .map(|c| c.holds.iter().copied().collect::<Vec<_>>())
            .unwrap()
    };
    assert_eq!(holds(".github/workflows/ci.yml"), [Reason::Protected]);
    assert_eq!(holds("CLAUDE.md"), [Reason::Protected]);
    assert_eq!(holds("build.rs"), [Reason::BuildScript]);
    assert_eq!(holds("old.rs"), [Reason::Delete]);
    // Only the root CLAUDE.md is protected; the control is that a nested one is a plain edit.
    assert!(holds("sub/CLAUDE.md").is_empty());

    let mut real = base.clone();
    let err = apply(&cs, &cs.hash(), &only("build.rs"), &work, &mut real).unwrap_err();
    assert_eq!(
        err,
        ApplyError::Unapproved(vec![
            ".github/workflows/ci.yml".into(),
            "CLAUDE.md".into(),
            "old.rs".into()
        ])
    );
    assert!(real.writes.is_empty(), "nothing written on a refusal");
    assert_eq!(real.manifest(), base.manifest());

    let all: BTreeSet<String> = cs
        .needing_approval()
        .iter()
        .map(|c| c.path.clone())
        .collect();
    apply(&cs, &cs.hash(), &all, &work, &mut real).unwrap();
    assert_eq!(real.manifest(), work.manifest());
}

#[test]
fn protection_is_case_insensitive() {
    let base = Tree::of(&[]);
    let work = Tree::of(&[
        (".GIT/config", "x"),
        ("Claude.MD", "x"),
        ("Hooks/pre-commit", "x"),
    ]);
    let cs = diff(&base.manifest(), &work.manifest());
    assert_eq!(cs.needing_approval().len(), 3, "{cs:?}");
}

#[test]
fn bad_paths_are_refused_and_cannot_be_approved() {
    let base = Tree::of(&[]);
    let work = Tree::of(&[
        ("../evil", "x"),
        ("/abs", "x"),
        ("C:/x", "x"),
        ("a\\b", "x"),
        ("a//b", "x"),
        ("./a", "x"),
        ("ok.rs", "x"),
    ]);
    let cs = diff(&base.manifest(), &work.manifest());
    assert_eq!(cs.changes.len(), 1);
    assert_eq!(cs.changes[0].path, "ok.rs");
    assert_eq!(cs.refused.len(), 6, "{:?}", cs.refused);
    let all: BTreeSet<String> = cs.refused.iter().cloned().collect();
    let mut real = base.clone();
    let err = apply(&cs, &cs.hash(), &all, &work, &mut real).unwrap_err();
    assert!(matches!(err, ApplyError::Refused(_)));
    assert!(real.files.is_empty());
}

#[test]
fn a_changed_real_file_is_a_conflict_and_nothing_is_written() {
    let base = Tree::of(&[("a.rs", "1"), ("b.rs", "1")]);
    let work = Tree::of(&[("a.rs", "2"), ("b.rs", "2"), ("c.rs", "2")]);
    let cs = diff(&base.manifest(), &work.manifest());
    // Someone edited a.rs and created c.rs in the real tree meanwhile.
    let mut real = Tree::of(&[("a.rs", "edited"), ("b.rs", "1"), ("c.rs", "theirs")]);
    let err = apply(&cs, &cs.hash(), &none(), &work, &mut real).unwrap_err();
    let ApplyError::Conflict(cf) = err else {
        panic!("{err:?}")
    };
    let paths: Vec<&str> = cf.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(paths, ["a.rs", "c.rs"]);
    assert_eq!(cf[1].expected, None);
    assert!(real.writes.is_empty(), "b.rs must not be written either");
    // Control: the same set applies cleanly to an untouched real tree.
    let mut clean = base.clone();
    apply(&cs, &cs.hash(), &none(), &work, &mut clean).unwrap();
}

#[test]
fn a_delete_conflicts_if_the_real_file_changed() {
    let base = Tree::of(&[("old.rs", "1")]);
    let cs = diff(&base.manifest(), &Tree::of(&[]).manifest());
    let mut real = Tree::of(&[("old.rs", "edited")]);
    let err = apply(&cs, &cs.hash(), &only("old.rs"), &Tree::of(&[]), &mut real).unwrap_err();
    assert!(matches!(err, ApplyError::Conflict(_)));
    assert!(real.files.contains_key("old.rs"));
}

#[test]
fn the_approved_hash_must_match_the_set_and_the_work_copy_must_match_it() {
    let base = Tree::of(&[("a.rs", "1")]);
    let work = Tree::of(&[("a.rs", "2")]);
    let cs = diff(&base.manifest(), &work.manifest());
    let mut real = base.clone();
    // A person approved some other set.
    assert!(matches!(
        apply(&cs, "deadbeef", &none(), &work, &mut real),
        Err(ApplyError::WorkCopy(_))
    ));
    // The working copy changed after the diff: its bytes no longer hash to the recorded value.
    let drifted = Tree::of(&[("a.rs", "3")]);
    assert_eq!(
        apply(&cs, &cs.hash(), &none(), &drifted, &mut real),
        Err(ApplyError::WorkCopy("a.rs".into()))
    );
    assert_eq!(real.manifest(), base.manifest());
    // The set hash changes when any hash in it does.
    let other = diff(&base.manifest(), &drifted.manifest());
    assert_ne!(cs.hash(), other.hash());
}

#[test]
fn a_failed_write_reports_what_was_applied_and_deletes_wait_until_after_writes() {
    let base = Tree::of(&[("a.rs", "1"), ("gone.rs", "1")]);
    let work = Tree::of(&[("a.rs", "2"), ("z.rs", "2")]);
    let cs = diff(&base.manifest(), &work.manifest());
    let mut real = base.clone();
    real.fail_on = Some("z.rs".into());
    let err = apply(&cs, &cs.hash(), &only("gone.rs"), &work, &mut real).unwrap_err();
    assert_eq!(
        err,
        ApplyError::Io {
            path: "z.rs".into(),
            msg: "disk full".into(),
            done: vec!["a.rs".into()]
        }
    );
    assert!(real.files.contains_key("gone.rs"), "the delete had not run");
}
