// SPDX-License-Identifier: MIT OR Apache-2.0
//! B2 write-back as a tool call (`docs/SANDBOX.md` 5.6, "B2 write-back" steps 3-7 and "What the model sees").
//!
//! The model asks for the boundary with `workspace_writeback`. The harness diffs the working copy, **asks a person**
//! ([`Approver`]) about the exact change set, applies it only if answered yes, and writes one [`Ledger`] line for the set and one
//! per approved held path. Apply is `Ask` by default and there is no "allow without asking" setting: [`WritePolicy`] has only
//! `Ask` and `Deny`. The tool exists only in `CopyBack`; under `Bind` it is not offered and a direct call is refused and ledgered.
//!
//! The tool name has an underscore, not the design's dot, because common model providers reject dots in tool names.

use std::collections::BTreeSet;
use std::sync::Arc;

use rig_core::completion::ToolDefinition;
use rig_core::message::ToolName;

use crate::changeset::{ApplyError, ChangeSet, Manifest, RealTree, WorkCopy, apply, diff};
use crate::driver::{ToolHost, ToolOutcome};
use crate::records::{Records, RecordsError, ResumeFacts, WorkspaceRecord};
use crate::workspace::WorkspaceMode;

/// The tool's name as the model sees it.
pub const TOOL_NAME: &str = "workspace_writeback";

/// What the person answered about one change set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Deny,
    /// Apply the set. `approved` names the held paths (protected, delete, build script) the person approved one by one.
    Approve {
        approved: BTreeSet<String>,
    },
}

/// The Ask. Implemented by the consent channel; the loop never answers for a person.
pub trait Approver {
    fn ask(&mut self, set: &ChangeSet, set_hash: &str) -> Decision;
}

/// Policy for the write-back tool. There is deliberately no `Allow`: reaching the real tree always needs a person.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WritePolicy {
    #[default]
    Ask,
    Deny,
}

/// One audit line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerLine {
    /// `writeback.set` for the whole set, `writeback.path` for one approved held path, `writeback.refused` for a call that was
    /// not allowed to ask.
    pub event: String,
    pub set_hash: String,
    pub path: Option<String>,
    /// `applied`, `denied`, `empty`, `failed:<why>`, `policy-deny`, `not-copy-back`, `approved`.
    pub outcome: String,
}

pub trait Ledger {
    fn record(&mut self, line: LedgerLine);
}

/// In-memory ledger, for tests.
#[derive(Default)]
pub struct MemoryLedger {
    pub lines: Vec<LedgerLine>,
}
impl Ledger for MemoryLedger {
    fn record(&mut self, line: LedgerLine) {
        self.lines.push(line);
    }
}

/// How a write-back call ended.
#[derive(Debug, PartialEq, Eq)]
pub enum WriteBack {
    /// Nothing differs from the base.
    Empty,
    /// The policy or mode did not allow asking. The real tree was not touched.
    Refused(&'static str),
    Denied {
        set_hash: String,
    },
    Applied {
        set_hash: String,
        paths: Vec<String>,
    },
    Failed {
        set_hash: String,
        error: ApplyError,
    },
}

/// Run one write-back. Never touches `real` unless `approver` said yes and `apply` passed every check.
#[allow(clippy::too_many_arguments)]
pub fn write_back(
    mode: WorkspaceMode,
    policy: WritePolicy,
    base: &Manifest,
    work_manifest: &Manifest,
    work: &impl WorkCopy,
    real: &mut impl RealTree,
    approver: &mut impl Approver,
    ledger: &mut impl Ledger,
) -> WriteBack {
    run(
        mode,
        policy,
        base,
        work_manifest,
        work,
        real,
        approver,
        ledger,
        None,
    )
    .outcome
}

/// A write-back call and what happened to its record (`docs/RUN-RECORD.md` 3).
#[derive(Debug, PartialEq, Eq)]
pub struct Recorded {
    pub outcome: WriteBack,
    /// A record could not be written. When the intent could not, `outcome` is `Refused` and nothing was written. When the closing
    /// `done` could not, `outcome` is still what really happened (an apply that succeeded is still `Applied`), and a resumed
    /// session will see an unfinished intent.
    pub record_error: Option<String>,
}

/// [`write_back`] with the run record kept: a `writeback.intent` is appended **before** `apply` touches the real tree, and a
/// `writeback.done` after every outcome. **Fail closed:** if the intent cannot be recorded, the real tree is not touched.
#[allow(clippy::too_many_arguments)]
pub fn write_back_recorded(
    records: &dyn Records,
    session: &str,
    mode: WorkspaceMode,
    policy: WritePolicy,
    base: &Manifest,
    work_manifest: &Manifest,
    work: &impl WorkCopy,
    real: &mut impl RealTree,
    approver: &mut impl Approver,
    ledger: &mut impl Ledger,
) -> Recorded {
    run(
        mode,
        policy,
        base,
        work_manifest,
        work,
        real,
        approver,
        ledger,
        Some((records, session)),
    )
}

#[allow(clippy::too_many_arguments)]
fn run(
    mode: WorkspaceMode,
    policy: WritePolicy,
    base: &Manifest,
    work_manifest: &Manifest,
    work: &impl WorkCopy,
    real: &mut impl RealTree,
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
        let (records, session) = rec?;
        records
            .append_record(session, &r)
            .err()
            .map(|e| e.to_string())
    };
    let done = |set_hash: &str, outcome: String, approved: Vec<String>| WorkspaceRecord::Done {
        set_hash: set_hash.into(),
        outcome,
        approved,
    };
    if mode != WorkspaceMode::CopyBack {
        ledger.record(line("writeback.refused", "-", None, "not-copy-back".into()));
        let record_error = record(done("-", "not-copy-back".into(), vec![]));
        return Recorded {
            outcome: WriteBack::Refused("write-back exists only in copy-back mode"),
            record_error,
        };
    }
    if policy == WritePolicy::Deny {
        ledger.record(line("writeback.refused", "-", None, "policy-deny".into()));
        let record_error = record(done("-", "policy-deny".into(), vec![]));
        return Recorded {
            outcome: WriteBack::Refused("policy denies write-back"),
            record_error,
        };
    }
    let set = diff(base, work_manifest);
    let set_hash = set.hash();
    if set.is_empty() {
        ledger.record(line("writeback.set", &set_hash, None, "empty".into()));
        let record_error = record(done(&set_hash, "empty".into(), vec![]));
        return Recorded {
            outcome: WriteBack::Empty,
            record_error,
        };
    }
    let approved = match approver.ask(&set, &set_hash) {
        Decision::Deny => {
            ledger.record(line("writeback.set", &set_hash, None, "denied".into()));
            let record_error = record(done(&set_hash, "denied".into(), vec![]));
            return Recorded {
                outcome: WriteBack::Denied { set_hash },
                record_error,
            };
        }
        // Approving a path that is not held changes nothing: only held paths consult this set.
        Decision::Approve { approved } => approved,
    };
    // Fail closed: the intent is durable before the first byte of the real tree changes.
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
    let held_approved: Vec<String> = set
        .needing_approval()
        .iter()
        .filter(|c| approved.contains(&c.path))
        .map(|c| c.path.clone())
        .collect();
    match apply(&set, &set_hash, &approved, work, real) {
        Ok(paths) => {
            for c in set.needing_approval() {
                ledger.record(line(
                    "writeback.path",
                    &set_hash,
                    Some(&c.path),
                    "approved".into(),
                ));
            }
            ledger.record(line("writeback.set", &set_hash, None, "applied".into()));
            let record_error = record(done(&set_hash, "applied".into(), held_approved));
            Recorded {
                outcome: WriteBack::Applied { set_hash, paths },
                record_error,
            }
        }
        Err(error) => {
            ledger.record(line(
                "writeback.set",
                &set_hash,
                None,
                format!("failed:{error}"),
            ));
            let record_error = record(done(&set_hash, format!("failed:{error}"), vec![]));
            Recorded {
                outcome: WriteBack::Failed { set_hash, error },
                record_error,
            }
        }
    }
}

/// What a resumed session found when it checked an unfinished write-back against the real tree.
#[derive(Debug, PartialEq, Eq)]
pub enum Recovery {
    /// There was no unfinished intent.
    NothingPending,
    /// Every path in the set already holds its new content: recorded as `applied-recovered`.
    Applied,
    /// Every path still holds its base content: recorded as `failed:interrupted`. Safe to propose again.
    NotApplied,
    /// Some paths were written and some were not (or differ from both). **Nothing is recorded and nothing is retried**: the
    /// person is told.
    Partial {
        written: Vec<String>,
        untouched: Vec<String>,
        other: Vec<String>,
    },
    /// The working copy no longer produces the change set the intent named, so the paths cannot be judged. Nothing is recorded.
    Unjudgeable,
}

/// Settle an unfinished `writeback.intent` (`docs/RUN-RECORD.md` 3, point 3) by recomputing, per path, whether the real tree
/// holds the change set's new content, its base content, or neither. Needs the base manifest and the working copy's manifest the
/// intent was made from. Records the verdict only when it is certain (`Applied`, `NotApplied`).
pub fn recover_unfinished(
    records: &dyn Records,
    session: &str,
    facts: &ResumeFacts,
    base: &Manifest,
    work_manifest: &Manifest,
    real: &impl RealTree,
) -> Result<Recovery, RecordsError> {
    let Some(WorkspaceRecord::Intent { set_hash, .. }) = &facts.unfinished else {
        return Ok(Recovery::NothingPending);
    };
    let set = diff(base, work_manifest);
    if set.hash() != *set_hash {
        return Ok(Recovery::Unjudgeable);
    }
    let (mut written, mut untouched, mut other) = (vec![], vec![], vec![]);
    for c in &set.changes {
        let actual = real.hash_of(&c.path);
        // A path whose base and new content are equal cannot be told apart; it counts as written (nothing to redo).
        if actual == c.new {
            written.push(c.path.clone());
        } else if actual == c.base {
            untouched.push(c.path.clone());
        } else {
            other.push(c.path.clone());
        }
    }
    let verdict = if untouched.is_empty() && other.is_empty() {
        Recovery::Applied
    } else if written.is_empty() && other.is_empty() {
        Recovery::NotApplied
    } else {
        return Ok(Recovery::Partial {
            written,
            untouched,
            other,
        });
    };
    let outcome = match verdict {
        Recovery::Applied => "applied-recovered",
        _ => "failed:interrupted",
    };
    records.append_record(
        session,
        &WorkspaceRecord::Done {
            set_hash: set_hash.clone(),
            outcome: outcome.into(),
            approved: vec![],
        },
    )?;
    Ok(verdict)
}

/// What a resumed session learned about its workspace, ready to show a person or put in the model's context.
#[derive(Debug, PartialEq, Eq)]
pub struct ResumeReport {
    /// The facts **after** any recovery that was recorded.
    pub facts: ResumeFacts,
    pub recovery: Recovery,
    /// Plain-language lines, one per thing the person should know. Empty means nothing is wrong that the record can show; it does
    /// not rule out a record lost whole at the end (`docs/RUN-RECORD.md` 4).
    pub notes: Vec<String>,
}

/// The host-side resume step (`docs/RUN-RECORD.md` 6, item 3): read the records, settle an unfinished write-back against the real
/// tree with [`recover_unfinished`], and say in words what was found. Damage is reported, never trusted; a partly applied
/// write-back is reported and not retried.
pub fn check_resume(
    records: &dyn Records,
    session: &str,
    base: &Manifest,
    work_manifest: &Manifest,
    real: &impl RealTree,
) -> Result<ResumeReport, RecordsError> {
    let before = records.resume_facts(session)?;
    let recovery = recover_unfinished(records, session, &before, base, work_manifest, real)?;
    let facts = match recovery {
        // A verdict was recorded: read again so `unfinished` is the truth now.
        Recovery::Applied | Recovery::NotApplied => records.resume_facts(session)?,
        _ => before,
    };
    let mut notes = Vec::new();
    if !facts.torn.is_empty() {
        notes.push(format!(
            "record(s) {:?} are damaged and were not trusted",
            facts.torn
        ));
    }
    for g in &facts.gaps {
        notes.push(format!("record(s) {}..={} are missing", g.start(), g.end()));
    }
    if !facts.unknown.is_empty() {
        notes.push(format!(
            "record(s) {:?} are in a format this version cannot read",
            facts.unknown
        ));
    }
    match &recovery {
        Recovery::NothingPending => {}
        Recovery::Applied => notes.push(
            "an interrupted write-back was found already applied to the real tree".into(),
        ),
        Recovery::NotApplied => notes.push(
            "an interrupted write-back had not been applied; it is safe to propose again".into(),
        ),
        Recovery::Partial {
            written,
            untouched,
            other,
        } => notes.push(format!(
            "an interrupted write-back is PARTLY applied (written {written:?}, untouched {untouched:?}, changed otherwise {other:?}); it was not retried, a person must decide"
        )),
        Recovery::Unjudgeable => notes.push(
            "an unfinished write-back could not be checked, because the working copy no longer matches it; it was not retried".into(),
        ),
    }
    Ok(ResumeReport {
        facts,
        recovery,
        notes,
    })
}

/// The tool as a [`ToolHost`]: offered only in `CopyBack` and not when the policy denies.
pub struct WriteBackTool<W, R, A, L> {
    pub mode: WorkspaceMode,
    pub policy: WritePolicy,
    pub base: Manifest,
    pub work: W,
    pub real: R,
    pub approver: A,
    pub ledger: L,
    /// Reads the working copy's current manifest.
    pub manifest_of: fn(&W) -> Manifest,
}

impl<W: WorkCopy, R: RealTree, A: Approver, L: Ledger> ToolHost for WriteBackTool<W, R, A, L> {
    fn definitions(&self) -> Vec<Arc<ToolDefinition>> {
        if self.mode != WorkspaceMode::CopyBack || self.policy == WritePolicy::Deny {
            return vec![];
        }
        vec![Arc::new(ToolDefinition::new(
            ToolName::new(TOOL_NAME).expect("non-empty"),
            "Ask a person to write your staged edits to the real project. Until then edits are only proposed.",
            serde_json::json!({"type":"object","properties":{}}),
        ))]
    }

    async fn call(&mut self, _id: &str, name: &str, _args: &serde_json::Value) -> ToolOutcome {
        if name != TOOL_NAME {
            return ToolOutcome::error(format!("unknown tool {name}"));
        }
        let manifest = (self.manifest_of)(&self.work);
        match write_back(
            self.mode,
            self.policy,
            &self.base,
            &manifest,
            &self.work,
            &mut self.real,
            &mut self.approver,
            &mut self.ledger,
        ) {
            WriteBack::Empty => ToolOutcome::ok("nothing to write back"),
            WriteBack::Refused(why) => ToolOutcome::error(why),
            WriteBack::Denied { .. } => ToolOutcome::error("the person declined the write-back"),
            WriteBack::Applied { paths, .. } => {
                // The real tree now matches the work copy for these paths: the next write-back diffs from here.
                for p in &paths {
                    match self.work.read(p) {
                        Some(b) => {
                            self.base
                                .0
                                .insert(p.clone(), crate::changeset::FileStamp::of(&b));
                        }
                        None => {
                            self.base.0.remove(p);
                        }
                    }
                }
                ToolOutcome::ok(format!("written: {}", paths.join(", ")))
            }
            WriteBack::Failed { error, .. } => ToolOutcome::error(error.to_string()),
        }
    }
}
