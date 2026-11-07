// SPDX-License-Identifier: MIT OR Apache-2.0

//! agent-loop core: our own [`Model`], [`ToolHost`] and [`SessionStore`] traits, and a [`Driver`] that
//! runs rig-agent's sans-I/O `AgentRun` behind them (CireSnave, board 373 Q1 = "A").
//!
//! Slices 1-3: the seams, an in-memory store, a driver that snapshots at step boundaries, the idempotency
//! [`Journal`], and a [`RunControl`] handle (soft stop, inject). Not yet here: an event stream, a write-through
//! inject queue, hooks and compaction.
//! The traits still name rig-core request and stream types; wrapping them is a later slice.

mod changeset;
mod control;
mod driver;
mod journal;
mod store;
mod workspace;

pub use changeset::{
    ApplyError, Change, ChangeSet, Conflict, FileStamp, Kind, Manifest, RealTree, Reason, WorkCopy,
    apply, diff, hash_bytes,
};
pub use control::RunControl;
pub use driver::{Driver, DriverError, End, Model, ToolHost, ToolOutcome};
pub use journal::Journal;
pub use store::{MemoryStore, SessionStore, StoreError};
pub use workspace::{
    ApprovalStore, MemoryApprovals, PolicyError, Sandbox, SandboxError, Selection, Why, Workspace,
    WorkspaceMode, open_workspace, policy_hash, requested_mode, select_mode,
};
