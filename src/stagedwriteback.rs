// SPDX-License-Identifier: MIT OR Apache-2.0
//! Write-back for an AntSpawn copy-back session (`docs/SANDBOX.md` 5.6): [`Staged`] becomes the [`ChangeSet`] a person is asked
//! about, and their answer becomes the `deletes`/`protected` lists of `apply`.
//!
//! This is the other half of [`crate::write_back`]. There the harness diffs a working copy it can read and applies it itself;
//! here AntSpawn holds the working copy, stages the set under its own `set_hash` and writes it into the real project, so this
//! module never reads a file or writes one. Three consequences, each deliberate:
//!
//! * The hash the person sees and the ledger records is AntSpawn's `set_hash`, the one `apply` must be given. It is not
//!   [`ChangeSet::hash`] (a different byte format).
//! * A [`Staged`] carries no base hashes, so the [`ChangeSet`] built here has `base: None` on every change. It is for the
//!   person's review and the hold rules; it is never passed to [`crate::apply`]. The real tree's conflict check is AntSpawn's,
//!   and a conflict it reports comes back as [`ApplyError::Conflict`] with `expected` and `actual` both `None` (it gives the
//!   path and a reason, not hashes).
//! * Approvals are sent as AntSpawn expects: an approved delete goes in `deletes`, any other approved held path in `protected`.
//!   A path the person approved that is not held is not sent. Nothing is sent that the person did not name.
//!
//! **Not run against a real `spawn`.** The behaviour is read from AntSpawn's `docs/SESSION-PLAN.md` and tested against a scripted
//! fake.

use std::collections::BTreeSet;

use crate::antspawn::{AntSpawnSession, Lines, SessionError, Staged};
use crate::changeset::{ApplyError, Change, ChangeSet, Conflict, Kind, Reason, holds_for, plain};
use crate::workspace::WorkspaceMode;
use crate::writeback::{Approver, Decision, Ledger, LedgerLine, WriteBack, WritePolicy};

/// The [`ChangeSet`] a person is asked about for `staged`.
///
/// Held paths are the union of this crate's rules (`.git`, build scripts, deletes) and AntSpawn's own `refused` list (it refuses
/// those unless the apply names them). A refused path that is in none of `added`, `modified` or `deleted` is shown as a
/// `Modify` with no hash and a `Protected` hold, so the person sees it. A path that is not a plain relative path goes in
/// `refused` and is never approvable.
pub fn change_set_from_staged(staged: &Staged) -> ChangeSet {
    let mut listed: Vec<(&str, Kind, Option<&str>)> = Vec::new();
    for f in &staged.added {
        listed.push((&f.path, Kind::Add, Some(&f.sha256)));
    }
    for f in &staged.modified {
        listed.push((&f.path, Kind::Modify, Some(&f.sha256)));
    }
    for p in &staged.deleted {
        listed.push((p, Kind::Delete, None));
    }
    for r in &staged.refused {
        if !listed.iter().any(|(p, _, _)| *p == r.path) {
            listed.push((&r.path, Kind::Modify, None));
        }
    }
    let mut changes = Vec::new();
    let mut refused = Vec::new();
    for (path, kind, new) in listed {
        if !plain(path) {
            refused.push(path.to_string());
            continue;
        }
        let mut holds = holds_for(path, kind);
        // AntSpawn lists a delete in `refused` too; that hold is already `Delete`.
        if kind != Kind::Delete && staged.refused.iter().any(|r| r.path == path) {
            holds.insert(Reason::Protected);
        }
        changes.push(Change {
            path: path.to_string(),
            kind,
            base: None,
            new: new.map(str::to_string),
            holds,
        });
    }
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    ChangeSet { changes, refused }
}

fn session_error(e: SessionError) -> ApplyError {
    ApplyError::Io {
        path: "<session>".into(),
        msg: e.to_string(),
        done: vec![],
    }
}

/// Run one write-back through an AntSpawn copy-back session: fetch and stage the changes, ask `approver` about the set under
/// AntSpawn's `set_hash`, and send `apply` with exactly the approved deletes and protected paths. Writes one ledger line for the
/// set and one per approved held path, as [`crate::write_back`] does.
///
/// The real tree is touched only by the session's `apply`, and only after the approver said yes and every held path was approved
/// by name. `Failed` carries [`ApplyError::Unapproved`] when a held path was not approved, [`ApplyError::Refused`] for paths
/// AntSpawn refused or that are not plain relative paths, [`ApplyError::Conflict`] when AntSpawn reports one (nothing written),
/// and [`ApplyError::Io`] with path `<session>` when the session itself failed.
pub fn write_back_staged<L: Lines>(
    session: &mut AntSpawnSession<L>,
    policy: WritePolicy,
    approver: &mut impl Approver,
    ledger: &mut impl Ledger,
) -> WriteBack {
    let line = |event: &str, set_hash: &str, path: Option<&str>, outcome: String| LedgerLine {
        event: event.into(),
        set_hash: set_hash.into(),
        path: path.map(str::to_string),
        outcome,
    };
    if session.mode() != WorkspaceMode::CopyBack {
        ledger.record(line("writeback.refused", "-", None, "not-copy-back".into()));
        return WriteBack::Refused("write-back exists only in copy-back mode");
    }
    if policy == WritePolicy::Deny {
        ledger.record(line("writeback.refused", "-", None, "policy-deny".into()));
        return WriteBack::Refused("policy denies write-back");
    }
    let staged = match session.changes() {
        Ok(s) => s,
        Err(e) => {
            let error = session_error(e);
            ledger.record(line("writeback.set", "-", None, format!("failed:{error}")));
            return WriteBack::Failed {
                set_hash: "-".into(),
                error,
            };
        }
    };
    let set = change_set_from_staged(&staged);
    let set_hash = staged.set_hash.clone();
    if set.is_empty() {
        ledger.record(line("writeback.set", &set_hash, None, "empty".into()));
        return WriteBack::Empty;
    }
    if !set.refused.is_empty() {
        let error = ApplyError::Refused(set.refused.clone());
        ledger.record(line(
            "writeback.set",
            &set_hash,
            None,
            format!("failed:{error}"),
        ));
        return WriteBack::Failed { set_hash, error };
    }
    let approved = match approver.ask(&set, &set_hash) {
        Decision::Deny => {
            ledger.record(line("writeback.set", &set_hash, None, "denied".into()));
            return WriteBack::Denied { set_hash };
        }
        Decision::Approve { approved } => approved,
    };
    let unapproved: Vec<String> = set
        .needing_approval()
        .iter()
        .filter(|c| !approved.contains(&c.path))
        .map(|c| c.path.clone())
        .collect();
    if !unapproved.is_empty() {
        let error = ApplyError::Unapproved(unapproved);
        ledger.record(line(
            "writeback.set",
            &set_hash,
            None,
            format!("failed:{error}"),
        ));
        return WriteBack::Failed { set_hash, error };
    }
    // Only held paths the person approved by name are sent; deletes and the rest are kept apart as AntSpawn's `apply` wants.
    let mut deletes = BTreeSet::new();
    let mut protected = BTreeSet::new();
    for c in set.needing_approval() {
        if c.kind == Kind::Delete {
            deletes.insert(c.path.clone());
        } else {
            protected.insert(c.path.clone());
        }
    }
    match session.apply(&set_hash, &deletes, &protected) {
        Err(e) => {
            let error = session_error(e);
            ledger.record(line(
                "writeback.set",
                &set_hash,
                None,
                format!("failed:{error}"),
            ));
            WriteBack::Failed { set_hash, error }
        }
        Ok(applied) if applied.written() => {
            for c in set.needing_approval() {
                ledger.record(line(
                    "writeback.path",
                    &set_hash,
                    Some(&c.path),
                    "approved".into(),
                ));
            }
            ledger.record(line("writeback.set", &set_hash, None, "applied".into()));
            WriteBack::Applied {
                set_hash,
                paths: applied.applied,
            }
        }
        Ok(applied) => {
            let error = if applied.refused.is_empty() {
                ApplyError::Conflict(
                    applied
                        .conflicts
                        .into_iter()
                        .map(|w| Conflict {
                            path: w.path,
                            expected: None,
                            actual: None,
                        })
                        .collect(),
                )
            } else {
                ApplyError::Refused(applied.refused.into_iter().map(|w| w.path).collect())
            };
            ledger.record(line(
                "writeback.set",
                &set_hash,
                None,
                format!("failed:{error}"),
            ));
            WriteBack::Failed { set_hash, error }
        }
    }
}
