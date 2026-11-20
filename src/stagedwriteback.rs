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
use std::sync::Arc;

use crate::antspawn::{AntSpawnSession, Lines, SessionError, Staged};
use crate::changeset::{ApplyError, Change, ChangeSet, Conflict, Kind, Reason, holds_for, plain};
use crate::driver::{ToolHost, ToolOutcome};
use crate::records::{Records, WorkspaceRecord};
use crate::workspace::WorkspaceMode;
use crate::writeback::{
    Approver, Decision, Ledger, LedgerLine, Recorded, TOOL_NAME, WriteBack, WritePolicy,
    tool_definitions,
};

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
    run_staged(session, policy, approver, ledger, None).outcome
}

/// [`write_back_staged`] with the run record kept (`docs/RUN-RECORD.md` 3): a `writeback.intent` is appended **before** the
/// session's `apply` is sent, and a `writeback.done` after every outcome. **Fail closed:** if the intent cannot be recorded,
/// `apply` is not sent and the outcome is `Refused`.
///
/// The intent names AntSpawn's `set_hash` and the paths of the staged set. Recovery after an interruption
/// ([`crate::recover_unfinished`]) recomputes a change set from a working copy this crate can read; here AntSpawn holds it, so
/// an unfinished staged intent is not judged by this crate and a person must check the real tree.
pub fn write_back_staged_recorded<L: Lines>(
    records: &dyn Records,
    record_session: &str,
    session: &mut AntSpawnSession<L>,
    policy: WritePolicy,
    approver: &mut impl Approver,
    ledger: &mut impl Ledger,
) -> Recorded {
    run_staged(
        session,
        policy,
        approver,
        ledger,
        Some((records, record_session)),
    )
}

fn run_staged<L: Lines>(
    session: &mut AntSpawnSession<L>,
    policy: WritePolicy,
    approver: &mut impl Approver,
    ledger: &mut impl Ledger,
    rec: Option<(&dyn Records, &str)>,
) -> Recorded {
    let line = |event: &str, set_hash: &str, path: Option<&str>, outcome: String| LedgerLine {
        event: event.into(),
        set_hash: set_hash.into(),
        path: path.map(str::to_string),
        outcome,
    };
    // Appends one record; a failure is carried, never swallowed.
    let record = |r: WorkspaceRecord| -> Option<String> {
        let (records, record_session) = rec?;
        records
            .append_record(record_session, &r)
            .err()
            .map(|e| e.to_string())
    };
    // Closes the record with `outcome` and returns `out`.
    let finish =
        |out: WriteBack, set_hash: &str, outcome: String, approved: Vec<String>| Recorded {
            outcome: out,
            record_error: record(WorkspaceRecord::Done {
                set_hash: set_hash.into(),
                outcome,
                approved,
            }),
        };
    if session.mode() != WorkspaceMode::CopyBack {
        ledger.record(line("writeback.refused", "-", None, "not-copy-back".into()));
        return finish(
            WriteBack::Refused("write-back exists only in copy-back mode"),
            "-",
            "not-copy-back".into(),
            vec![],
        );
    }
    if policy == WritePolicy::Deny {
        ledger.record(line("writeback.refused", "-", None, "policy-deny".into()));
        return finish(
            WriteBack::Refused("policy denies write-back"),
            "-",
            "policy-deny".into(),
            vec![],
        );
    }
    let staged = match session.changes() {
        Ok(s) => s,
        Err(e) => {
            let error = session_error(e);
            let word = format!("failed:{error}");
            ledger.record(line("writeback.set", "-", None, word.clone()));
            return finish(
                WriteBack::Failed {
                    set_hash: "-".into(),
                    error,
                },
                "-",
                word,
                vec![],
            );
        }
    };
    let set = change_set_from_staged(&staged);
    let set_hash = staged.set_hash.clone();
    if set.is_empty() {
        ledger.record(line("writeback.set", &set_hash, None, "empty".into()));
        return finish(WriteBack::Empty, &set_hash, "empty".into(), vec![]);
    }
    if !set.refused.is_empty() {
        let error = ApplyError::Refused(set.refused.clone());
        let word = format!("failed:{error}");
        ledger.record(line("writeback.set", &set_hash, None, word.clone()));
        return finish(
            WriteBack::Failed {
                set_hash: set_hash.clone(),
                error,
            },
            &set_hash,
            word,
            vec![],
        );
    }
    let approved = match approver.ask(&set, &set_hash) {
        Decision::Deny => {
            ledger.record(line("writeback.set", &set_hash, None, "denied".into()));
            return finish(
                WriteBack::Denied {
                    set_hash: set_hash.clone(),
                },
                &set_hash,
                "denied".into(),
                vec![],
            );
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
        let word = format!("failed:{error}");
        ledger.record(line("writeback.set", &set_hash, None, word.clone()));
        return finish(
            WriteBack::Failed {
                set_hash: set_hash.clone(),
                error,
            },
            &set_hash,
            word,
            vec![],
        );
    }
    // Fail closed: the intent is durable before `apply` is sent.
    if rec.is_some() {
        let intent = WorkspaceRecord::Intent {
            set_hash: set_hash.clone(),
            paths: set.changes.iter().map(|c| c.path.clone()).collect(),
        };
        if let Some(e) = record(intent) {
            ledger.record(line(
                "writeback.refused",
                &set_hash,
                None,
                "record-failed".into(),
            ));
            return Recorded {
                outcome: WriteBack::Refused(
                    "the intent could not be recorded; nothing was written",
                ),
                record_error: Some(e),
            };
        }
    }
    // Only held paths the person approved by name are sent; deletes and the rest are kept apart as AntSpawn's `apply` wants.
    let mut deletes = BTreeSet::new();
    let mut protected = BTreeSet::new();
    let held_approved: Vec<String> = set
        .needing_approval()
        .iter()
        .map(|c| c.path.clone())
        .collect();
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
            let word = format!("failed:{error}");
            ledger.record(line("writeback.set", &set_hash, None, word.clone()));
            finish(
                WriteBack::Failed {
                    set_hash: set_hash.clone(),
                    error,
                },
                &set_hash,
                word,
                vec![],
            )
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
            finish(
                WriteBack::Applied {
                    set_hash: set_hash.clone(),
                    paths: applied.applied,
                },
                &set_hash,
                "applied".into(),
                held_approved,
            )
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
            let word = format!("failed:{error}");
            ledger.record(line("writeback.set", &set_hash, None, word.clone()));
            finish(
                WriteBack::Failed {
                    set_hash: set_hash.clone(),
                    error,
                },
                &set_hash,
                word,
                vec![],
            )
        }
    }
}

/// The `workspace_writeback` tool over an AntSpawn copy-back session, as a [`ToolHost`]: offered only in `CopyBack` and not when
/// the policy denies. With `records` set it keeps the run record ([`write_back_staged_recorded`]): the intent is durable before
/// `apply` is sent, a failed intent means nothing is written and the model is told why, and a failed closing record is reported
/// with a warning the person should hear. Without it the call is [`write_back_staged`].
pub struct StagedWriteBackTool<L: Lines, A, G> {
    pub session: AntSpawnSession<L>,
    pub policy: WritePolicy,
    pub approver: A,
    pub ledger: G,
    /// The run record and the session name its records belong to; `None` keeps no record.
    pub records: Option<(Arc<dyn Records>, String)>,
}

impl<L: Lines, A: Approver, G: Ledger> ToolHost for StagedWriteBackTool<L, A, G> {
    fn definitions(&self) -> Vec<Arc<rig_core::completion::ToolDefinition>> {
        tool_definitions(self.session.mode(), self.policy)
    }

    async fn call(&mut self, _id: &str, name: &str, _args: &serde_json::Value) -> ToolOutcome {
        if name != TOOL_NAME {
            return ToolOutcome::error(format!("unknown tool {name}"));
        }
        let rec = match &self.records {
            Some((records, record_session)) => write_back_staged_recorded(
                &**records,
                record_session,
                &mut self.session,
                self.policy,
                &mut self.approver,
                &mut self.ledger,
            ),
            None => Recorded {
                outcome: write_back_staged(
                    &mut self.session,
                    self.policy,
                    &mut self.approver,
                    &mut self.ledger,
                ),
                record_error: None,
            },
        };
        let (is_error, mut text) = match &rec.outcome {
            WriteBack::Empty => (false, "nothing to write back".to_string()),
            WriteBack::Refused(why) => (true, (*why).to_string()),
            WriteBack::Denied { .. } => (true, "the person declined the write-back".into()),
            WriteBack::Applied { paths, .. } => (false, format!("written: {}", paths.join(", "))),
            WriteBack::Failed { error, .. } => (true, error.to_string()),
        };
        if let Some(e) = &rec.record_error {
            text.push_str(&format!(
                "
WARNING: the run record could not be written ({e}); tell the person"
            ));
        }
        if is_error {
            ToolOutcome::error(text)
        } else {
            ToolOutcome::ok(text)
        }
    }
}
