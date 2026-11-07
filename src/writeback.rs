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
    let line = |event: &str, set_hash: &str, path: Option<&str>, outcome: String| LedgerLine {
        event: event.into(),
        set_hash: set_hash.into(),
        path: path.map(str::to_string),
        outcome,
    };
    if mode != WorkspaceMode::CopyBack {
        ledger.record(line("writeback.refused", "-", None, "not-copy-back".into()));
        return WriteBack::Refused("write-back exists only in copy-back mode");
    }
    if policy == WritePolicy::Deny {
        ledger.record(line("writeback.refused", "-", None, "policy-deny".into()));
        return WriteBack::Refused("policy denies write-back");
    }
    let set = diff(base, work_manifest);
    let set_hash = set.hash();
    if set.is_empty() {
        ledger.record(line("writeback.set", &set_hash, None, "empty".into()));
        return WriteBack::Empty;
    }
    let approved = match approver.ask(&set, &set_hash) {
        Decision::Deny => {
            ledger.record(line("writeback.set", &set_hash, None, "denied".into()));
            return WriteBack::Denied { set_hash };
        }
        // Approving a path that is not held changes nothing: only held paths consult this set.
        Decision::Approve { approved } => approved,
    };
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
            WriteBack::Applied { set_hash, paths }
        }
        Err(error) => {
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
