// SPDX-License-Identifier: MIT OR Apache-2.0

//! agent-loop core: our own [`Model`], [`ToolHost`] and [`SessionStore`] traits, and a [`Driver`] that
//! runs rig-agent's sans-I/O `AgentRun` behind them (CireSnave, board 373 Q1 = "A").
//!
//! Slices 1-3: the seams, an in-memory store, a driver that snapshots at step boundaries, the idempotency
//! [`Journal`], a [`RunControl`] handle (soft stop, inject, durable inject through an [`Inbox`]), and typed
//! [`Event`]s with a [`Status`] fold, a [`Hook`] (stop, allow/patch/deny a tool, rewrite its outcome, wrap-up), and a [`ContextPolicy`] (what each call sends).
//! [`BlobStore`] and [`BlobInbox`] (over PersistAnt) and the power-loss-careful [`FileStore`] and [`FileInbox`] are here. Not yet here: summarising dropped turns.
//! The run handle ([`Run`]) and its settings ([`RunConfig`]) are our own types; the `Model`, `ToolHost` and `ContextPolicy` traits
//! still name rig-core types (`docs/WRAP.md`, stages 2 and 3, waiting for a consumer). A saved session is the engine's: a different
//! engine cannot resume it.

mod antspawn;
mod argtext;
mod blobinbox;
mod blobstore;
mod changeset;
mod commandpolicy;
mod context;
mod control;
mod driver;
mod events;
mod file;
mod hook;
mod inbox;
mod journal;
mod networkpolicy;
mod pathpolicy;
mod records;
mod run;
mod secretpolicy;
mod store;
mod workspace;
mod writeback;

pub use antspawn::{
    AntSpawnSandbox, AntSpawnSession, Applied, ChildLines, ExecResult, Lines, PathWhy,
    SessionError, Staged, StagedFile,
};
pub use blobinbox::BlobInbox;
pub use blobstore::BlobStore;
pub use changeset::{
    ApplyError, Change, ChangeSet, Conflict, FileStamp, Kind, Manifest, RealTree, Reason, WorkCopy,
    apply, diff, hash_bytes,
};
pub use commandpolicy::{CommandPolicy, CommandPolicyError};
pub use context::{ContextPolicy, KeepRecent, Summarise};
pub use control::RunControl;
pub use driver::{Driver, DriverError, End, Model, ToolHost, ToolOutcome};
pub use events::{Activity, Event, EventLog, EventSink, Status, Step};
pub use file::{FileInbox, FileStore};
pub use hook::{Hook, ModelDecision, StopReason, ToolCall, ToolDecision};
pub use inbox::{Inbox, InboxError, MemoryInbox, Queued};
pub use journal::Journal;
pub use networkpolicy::{NetworkPolicy, NetworkPolicyError};
pub use pathpolicy::{PathPolicy, PathPolicyError};
pub use records::{
    KeyedBlobs, Listing, MemoryBlobs, MemoryRecords, RecordLog, Records, RecordsError, ResumeFacts,
    Stored, WorkspaceRecord,
};
#[cfg(feature = "persistant")]
pub use records::{PersistBlobs, records_needs};
pub use run::{Run, RunConfig};
pub use secretpolicy::{SecretPolicy, SecretPolicyError};
pub use store::{MemoryStore, SessionStore, StoreError};
pub use workspace::{
    ApprovalStore, MemoryApprovals, PolicyError, Sandbox, SandboxError, Selection, Why, Workspace,
    WorkspaceMode, open_workspace, policy_hash, requested_mode, select_mode,
};
pub use writeback::{
    Approver, Decision, Ledger, LedgerLine, MemoryLedger, Recorded, RecordedWriteBackTool,
    Recovery, ResumeReport, TOOL_NAME, WriteBack, WriteBackTool, WritePolicy, check_resume,
    recover_unfinished, write_back, write_back_recorded,
};
