// SPDX-License-Identifier: MIT OR Apache-2.0
//! B2 change set (`docs/SANDBOX.md` 5.6, "B2 write-back"): a base manifest, the diff against the working copy, and an
//! apply that only writes when the real tree still matches the base.
//!
//! Pure logic over in-memory manifests and two small traits; no file I/O lives here. Rules:
//! - a path that is not a plain relative path is **refused outright** and can never be approved;
//! - protected paths, deletes and build scripts are **held until a person approves that path by name**;
//! - apply checks everything first (approval, then the real tree against the base hash) and writes nothing on any failure.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use sha2::{Digest, Sha256};

/// Hex SHA-256 of some bytes.
pub fn hash_bytes(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Size and content hash of one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileStamp {
    pub size: u64,
    pub hash: String,
}

impl FileStamp {
    pub fn of(bytes: &[u8]) -> Self {
        Self {
            size: bytes.len() as u64,
            hash: hash_bytes(bytes),
        }
    }
}

/// Path -> stamp. Paths are `/`-separated and relative to the project root.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Manifest(pub BTreeMap<String, FileStamp>);

impl Manifest {
    pub fn from_files<'a>(files: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> Self {
        Self(
            files
                .into_iter()
                .map(|(p, b)| (p.to_string(), FileStamp::of(b)))
                .collect(),
        )
    }
}

/// Why a path is held for a person's approval.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reason {
    /// `.git`, `.github`, `.claude`, `CLAUDE.md`, hook files: a write changes what runs later outside the sandbox.
    Protected,
    /// A delete.
    Delete,
    /// `build.rs`, a `Makefile`, CI or package scripts: approving it enables something that runs on a later step.
    BuildScript,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Add,
    Modify,
    Delete,
}

/// One changed path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    pub kind: Kind,
    /// Hash in the base manifest (`None` for an add).
    pub base: Option<String>,
    /// Hash in the working copy (`None` for a delete).
    pub new: Option<String>,
    /// Empty means no approval is needed.
    pub holds: BTreeSet<Reason>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeSet {
    pub changes: Vec<Change>,
    /// Paths that are not plain relative paths. Never applied, never approvable.
    pub refused: Vec<String>,
}

/// A plain relative path: non-empty, no root or drive, no `.` or `..` or empty segment, no backslash or NUL.
fn plain(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', '\0'])
        && !path.starts_with('/')
        && !path
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
        && path.as_bytes().get(1) != Some(&b':')
}

/// Matching is case-insensitive: the real tree may be on a case-insensitive filesystem.
fn holds_for(path: &str, kind: Kind) -> BTreeSet<Reason> {
    let lower = path.to_ascii_lowercase();
    let segs: Vec<&str> = lower.split('/').collect();
    let name = *segs.last().unwrap_or(&"");
    let mut h = BTreeSet::new();
    if segs
        .iter()
        .any(|s| matches!(*s, ".git" | ".github" | ".claude" | "hooks"))
        || (segs.len() == 1 && name == "claude.md")
        || name == ".gitmodules"
        || name == ".gitattributes"
    {
        h.insert(Reason::Protected);
    }
    if matches!(
        name,
        "build.rs" | "makefile" | "gnumakefile" | "package.json" | "justfile" | "build.gradle"
    ) || (name.ends_with(".sh") && segs.contains(&"scripts"))
        || name.starts_with(".gitlab-ci")
    {
        h.insert(Reason::BuildScript);
    }
    if kind == Kind::Delete {
        h.insert(Reason::Delete);
    }
    h
}

/// The change set from `base` to `work`. Deterministic: changes are in path order.
pub fn diff(base: &Manifest, work: &Manifest) -> ChangeSet {
    let mut changes = Vec::new();
    let mut refused = Vec::new();
    let paths: BTreeSet<&String> = base.0.keys().chain(work.0.keys()).collect();
    for p in paths {
        let (b, w) = (base.0.get(p), work.0.get(p));
        let kind = match (b, w) {
            (Some(b), Some(w)) if b.hash == w.hash => continue,
            (Some(_), Some(_)) => Kind::Modify,
            (None, Some(_)) => Kind::Add,
            (Some(_), None) => Kind::Delete,
            (None, None) => continue,
        };
        if !plain(p) {
            refused.push(p.clone());
            continue;
        }
        changes.push(Change {
            path: p.clone(),
            kind,
            base: b.map(|s| s.hash.clone()),
            new: w.map(|s| s.hash.clone()),
            holds: holds_for(p, kind),
        });
    }
    ChangeSet { changes, refused }
}

impl ChangeSet {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty() && self.refused.is_empty()
    }

    /// Hash of the whole set (what the person approves and the ledger records). Covers every path, kind and both hashes.
    pub fn hash(&self) -> String {
        let mut s = String::new();
        for c in &self.changes {
            s.push_str(&format!(
                "{:?}\t{}\t{}\t{}\n",
                c.kind,
                c.path,
                c.base.as_deref().unwrap_or("-"),
                c.new.as_deref().unwrap_or("-")
            ));
        }
        for r in &self.refused {
            s.push_str(&format!("refused\t{r}\n"));
        }
        hash_bytes(s.as_bytes())
    }

    /// Paths that need a person's approval before apply.
    pub fn needing_approval(&self) -> Vec<&Change> {
        self.changes
            .iter()
            .filter(|c| !c.holds.is_empty())
            .collect()
    }
}

/// The real tree, as write-back sees it. Implemented by the harness, which runs as the user.
pub trait RealTree {
    /// Hash of the current file, `None` if absent.
    fn hash_of(&self, path: &str) -> Option<String>;
    fn write(&mut self, path: &str, bytes: &[u8]) -> Result<(), String>;
    fn remove(&mut self, path: &str) -> Result<(), String>;
}

/// The working copy.
pub trait WorkCopy {
    fn read(&self, path: &str) -> Option<Vec<u8>>;
}

/// The real file no longer matches what the session started from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub path: String,
    pub expected: Option<String>,
    pub actual: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ApplyError {
    /// The set holds paths that are not plain relative paths.
    Refused(Vec<String>),
    /// These held paths were not approved by name.
    Unapproved(Vec<String>),
    /// The real tree changed under us. Nothing was written.
    Conflict(Vec<Conflict>),
    /// The working copy does not hold a file the change set names (or it differs from the recorded hash).
    WorkCopy(String),
    /// A write failed part-way. `done` were applied before it; the tree is partially written and the person must be told.
    Io {
        path: String,
        msg: String,
        done: Vec<String>,
    },
}

impl fmt::Display for ApplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(p) => write!(f, "refused paths: {p:?}"),
            Self::Unapproved(p) => write!(f, "not approved: {p:?}"),
            Self::Conflict(c) => write!(f, "{} path(s) changed in the real tree", c.len()),
            Self::WorkCopy(p) => write!(f, "working copy does not match the change set at {p}"),
            Self::Io { path, msg, done } => {
                write!(f, "write failed at {path}: {msg} ({} applied)", done.len())
            }
        }
    }
}
impl std::error::Error for ApplyError {}

/// Apply `cs`. `approved` is the set of paths a person approved by name; `expected_hash` is the change-set hash they saw.
///
/// Order: refused paths, then the change-set hash, then approvals, then conflicts, then the working copy's bytes; only
/// if all pass is anything written. Deletes happen after writes so a failed write leaves files in place.
pub fn apply(
    cs: &ChangeSet,
    expected_hash: &str,
    approved: &BTreeSet<String>,
    work: &impl WorkCopy,
    real: &mut impl RealTree,
) -> Result<Vec<String>, ApplyError> {
    if !cs.refused.is_empty() {
        return Err(ApplyError::Refused(cs.refused.clone()));
    }
    if cs.hash() != expected_hash {
        return Err(ApplyError::WorkCopy(
            "change set differs from the one approved".into(),
        ));
    }
    let unapproved: Vec<String> = cs
        .changes
        .iter()
        .filter(|c| !c.holds.is_empty() && !approved.contains(&c.path))
        .map(|c| c.path.clone())
        .collect();
    if !unapproved.is_empty() {
        return Err(ApplyError::Unapproved(unapproved));
    }
    let conflicts: Vec<Conflict> = cs
        .changes
        .iter()
        .filter_map(|c| {
            let actual = real.hash_of(&c.path);
            (actual != c.base).then(|| Conflict {
                path: c.path.clone(),
                expected: c.base.clone(),
                actual,
            })
        })
        .collect();
    if !conflicts.is_empty() {
        return Err(ApplyError::Conflict(conflicts));
    }
    let mut payload = Vec::new();
    for c in cs.changes.iter().filter(|c| c.kind != Kind::Delete) {
        let bytes = work
            .read(&c.path)
            .filter(|b| Some(hash_bytes(b)) == c.new)
            .ok_or_else(|| ApplyError::WorkCopy(c.path.clone()))?;
        payload.push((&c.path, bytes));
    }
    let mut done = Vec::new();
    for (path, bytes) in payload {
        real.write(path, &bytes).map_err(|msg| ApplyError::Io {
            path: path.clone(),
            msg,
            done: done.clone(),
        })?;
        done.push(path.clone());
    }
    for c in cs.changes.iter().filter(|c| c.kind == Kind::Delete) {
        real.remove(&c.path).map_err(|msg| ApplyError::Io {
            path: c.path.clone(),
            msg,
            done: done.clone(),
        })?;
        done.push(c.path.clone());
    }
    Ok(done)
}
