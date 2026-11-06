// SPDX-License-Identifier: MIT OR Apache-2.0

//! agent-loop core: our own [`Model`], [`ToolHost`] and [`SessionStore`] traits, and a [`Driver`] that
//! runs rig-agent's sans-I/O `AgentRun` behind them (CireSnave, board 373 Q1 = "A").
//!
//! Slice 1: the seams, an in-memory store, and a driver that snapshots at step boundaries. Not yet here:
//! the `Run` handle (events, stop, inject), the idempotency journal, hooks and compaction.
//! The traits still name rig-core request and stream types; wrapping them is a later slice.

mod driver;
mod store;

pub use driver::{Driver, DriverError, End, Model, ToolHost, ToolOutcome};
pub use store::{MemoryStore, SessionStore, StoreError};
