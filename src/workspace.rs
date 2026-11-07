// SPDX-License-Identifier: MIT OR Apache-2.0
//! Workspace modes B1 and B2 (`docs/SANDBOX.md` 5.6; CireSnave, board 373 Q2: "B1 and B2 ... make it easy to choose one or the other").
//!
//! One setting picks the mode. **B2 (`CopyBack`) is the default.** B1 (`Bind`) is used only when a policy file asks for it
//! **and a person has approved that exact file for that root**. A missing, malformed or unapproved setting means B2, and
//! a sandbox that cannot give the selected mode refuses to open; it is never retried in the other mode.
//!
//! The approval store is read-only from here: nothing in the loop can record an approval.

use std::collections::HashSet;
use std::fmt;
use std::str::FromStr;

use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Where the sandboxed agent's edits land.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum WorkspaceMode {
    /// B2: a working copy in the session; edits reach the real tree only through a reviewed write-back.
    #[default]
    CopyBack,
    /// B1: the sandbox account is granted rights on the real folder; edits are in place.
    Bind,
}

impl WorkspaceMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CopyBack => "copy-back",
            Self::Bind => "bind",
        }
    }
}

impl fmt::Display for WorkspaceMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for WorkspaceMode {
    type Err = PolicyError;
    fn from_str(s: &str) -> Result<Self, PolicyError> {
        match s {
            "copy-back" => Ok(Self::CopyBack),
            "bind" => Ok(Self::Bind),
            other => Err(PolicyError::UnknownMode(other.to_string())),
        }
    }
}

/// A policy file could not be read as a workspace setting.
#[derive(Debug, PartialEq, Eq)]
pub enum PolicyError {
    Toml(String),
    UnknownMode(String),
}

impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Toml(m) => write!(f, "policy file: {m}"),
            Self::UnknownMode(m) => {
                write!(
                    f,
                    "unknown workspace mode {m:?} (expected \"copy-back\" or \"bind\")"
                )
            }
        }
    }
}
impl std::error::Error for PolicyError {}

// Other policy sections (commands, paths, network) belong to later slices, so unknown top-level tables are
// ignored here; inside `[workspace]` an unknown key is an error, so a typo cannot silently mean "default".
#[derive(Deserialize)]
struct PolicyFile {
    workspace: Option<WorkspaceSection>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceSection {
    mode: Option<String>,
}

/// The mode a policy file asks for, or `None` if it does not say.
pub fn requested_mode(policy_text: &str) -> Result<Option<WorkspaceMode>, PolicyError> {
    let file: PolicyFile =
        toml::from_str(policy_text).map_err(|e| PolicyError::Toml(e.to_string()))?;
    file.workspace
        .and_then(|w| w.mode)
        .map(|m| m.parse())
        .transpose()
}

/// Hash of the exact bytes of a policy file. An approval is bound to it, so any edit voids the approval.
pub fn policy_hash(policy_text: &str) -> String {
    Sha256::digest(policy_text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// What a person has approved. Read-only here: approvals come from the consent channel, never from the loop.
pub trait ApprovalStore {
    /// Has a person approved `mode` for `root` under the policy file with this hash?
    fn is_approved(&self, root: &str, mode: WorkspaceMode, policy_hash: &str) -> bool;
}

/// In-memory approvals, for tests and for the consent layer to fill in.
#[derive(Default)]
pub struct MemoryApprovals {
    set: HashSet<(String, WorkspaceMode, String)>,
}

impl MemoryApprovals {
    pub fn new() -> Self {
        Self::default()
    }
    /// Record a person's approval. Called by the consent channel, not by the agent loop.
    pub fn approve(&mut self, root: &str, mode: WorkspaceMode, policy_hash: &str) {
        self.set
            .insert((root.to_string(), mode, policy_hash.to_string()));
    }
}

impl ApprovalStore for MemoryApprovals {
    fn is_approved(&self, root: &str, mode: WorkspaceMode, policy_hash: &str) -> bool {
        self.set
            .contains(&(root.to_string(), mode, policy_hash.to_string()))
    }
}

/// Why a mode was chosen; goes to the audit line.
#[derive(Debug, PartialEq, Eq)]
pub enum Why {
    /// No policy file, or it has no `[workspace] mode`.
    Default,
    /// The policy asks for `copy-back`, the safe mode; no approval is needed.
    Policy,
    /// The policy asks for `bind` and a person approved this file for this root.
    Approved,
    /// The policy asks for `bind` but no approval matches this root and file: fell back to `copy-back`.
    Unapproved,
    /// The policy could not be read: fell back to `copy-back`.
    Malformed(String),
}

#[derive(Debug, PartialEq, Eq)]
pub struct Selection {
    pub mode: WorkspaceMode,
    pub why: Why,
}

/// Choose the workspace mode for a session. Only `Why::Approved` yields `Bind`; every other path yields `CopyBack`.
pub fn select_mode(
    policy_text: Option<&str>,
    root: &str,
    approvals: &dyn ApprovalStore,
) -> Selection {
    let safe = |why| Selection {
        mode: WorkspaceMode::CopyBack,
        why,
    };
    let Some(text) = policy_text else {
        return safe(Why::Default);
    };
    match requested_mode(text) {
        Err(e) => safe(Why::Malformed(e.to_string())),
        Ok(None) => safe(Why::Default),
        Ok(Some(WorkspaceMode::CopyBack)) => safe(Why::Policy),
        Ok(Some(WorkspaceMode::Bind)) => {
            if approvals.is_approved(root, WorkspaceMode::Bind, &policy_hash(text)) {
                Selection {
                    mode: WorkspaceMode::Bind,
                    why: Why::Approved,
                }
            } else {
                safe(Why::Unapproved)
            }
        }
    }
}

/// A project root and the mode it is opened in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workspace {
    pub root: String,
    pub mode: WorkspaceMode,
}

/// The sandbox could not give what was asked.
#[derive(Debug, PartialEq, Eq)]
pub enum SandboxError {
    /// The unmet need, named.
    Unmet(String),
}

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unmet(m) => write!(f, "sandbox cannot provide: {m}"),
        }
    }
}
impl std::error::Error for SandboxError {}

/// The E2 seam, as far as this slice needs it. The AntSpawn-backed implementation is a later slice.
pub trait Sandbox {
    fn open_session(&mut self, id: &str, workspace: &Workspace) -> Result<(), SandboxError>;
}

/// Open the session's workspace in the selected mode. **A refusal is final: the other mode is never tried.**
pub fn open_workspace<S: Sandbox>(
    sandbox: &mut S,
    id: &str,
    root: &str,
    selection: &Selection,
) -> Result<Workspace, SandboxError> {
    let workspace = Workspace {
        root: root.to_string(),
        mode: selection.mode,
    };
    sandbox.open_session(id, &workspace)?;
    Ok(workspace)
}
