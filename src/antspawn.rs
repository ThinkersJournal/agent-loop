// SPDX-License-Identifier: MIT OR Apache-2.0
//! The AntSpawn-backed session (`docs/SANDBOX.md` 5.6, slice 4): a client for `spawn session`'s JSON lines.
//!
//! `spawn session --launcher account --workspace copy|bind --root R` is one long-lived process: one JSON request per
//! line in, one JSON reply per line out (AntSpawn `docs/SESSION-PLAN.md`, at `3c9461b`). The session ends when that
//! process ends, so this client owns the child for the session's life.
//!
//! The line transport is a trait ([`Lines`]) so everything here is tested against a scripted fake; [`ChildLines`] is
//! the real one. **Nothing here has run against the real account**: the protocol is read from AntSpawn's docs and
//! tested against replies written from those docs, so the first real run may differ.
//!
//! The reviewed set comes back with AntSpawn's own `set_hash` (a different byte format from [`crate::ChangeSet::hash`]);
//! `apply` must be given that hash. This module does not decide what a person must approve: it surfaces AntSpawn's
//! `refused` and `later_step` lists and sends back only the `deletes`/`protected` paths the caller passes. Bridging a
//! [`Staged`] into the write-back tool's `ChangeSet` and `Ask` is a later slice.

use std::collections::BTreeSet;
use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

use crate::workspace::{Sandbox, SandboxError, Workspace, WorkspaceMode};

/// One JSON line out, one JSON line back.
pub trait Lines {
    fn send(&mut self, line: &str) -> Result<(), String>;
    /// The next reply line, `None` at end of output.
    fn recv(&mut self) -> Result<Option<String>, String>;
}

/// PathWhy a call failed.
#[derive(Debug, PartialEq, Eq)]
pub enum SessionError {
    /// The process or pipe failed.
    Transport(String),
    /// AntSpawn answered `{"op":"error",...}`.
    Refused(String),
    /// A reply that does not match what was asked or the documented shape.
    Protocol(String),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(m) => write!(f, "spawn session transport: {m}"),
            Self::Refused(m) => write!(f, "spawn session refused: {m}"),
            Self::Protocol(m) => write!(f, "spawn session protocol: {m}"),
        }
    }
}
impl std::error::Error for SessionError {}

/// An `exec` result. `stdout` and `stderr` are decoded from base64.
#[derive(Debug, PartialEq, Eq)]
pub struct ExecResult {
    pub status: String,
    pub exit_code: Option<i64>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// One file in a staged set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// A path AntSpawn's policy refuses unless an apply names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathWhy {
    pub path: String,
    pub why: String,
}

/// The reply to `changes`: what the session changed, staged by AntSpawn under `set_hash`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Staged {
    pub added: Vec<StagedFile>,
    pub modified: Vec<StagedFile>,
    pub deleted: Vec<String>,
    pub later_step: Vec<String>,
    pub refused: Vec<PathWhy>,
    pub set_hash: String,
}

impl Staged {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.modified.is_empty() && self.deleted.is_empty()
    }

    /// Every path the set touches, in no particular order.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.added
            .iter()
            .chain(&self.modified)
            .map(|f| f.path.as_str())
            .chain(self.deleted.iter().map(String::as_str))
    }
}

/// The reply to `apply`. Non-empty `refused` or `conflicts` means nothing was written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    pub set_hash: String,
    pub applied: Vec<String>,
    pub refused: Vec<PathWhy>,
    pub conflicts: Vec<PathWhy>,
}

impl Applied {
    /// True only if the set was written: nothing refused, nothing in conflict.
    pub fn written(&self) -> bool {
        self.refused.is_empty() && self.conflicts.is_empty()
    }
}

/// One open `spawn session`.
pub struct AntSpawnSession<L: Lines> {
    lines: L,
    mode: WorkspaceMode,
    next_id: u64,
    closed: bool,
}

impl<L: Lines> AntSpawnSession<L> {
    /// Wrap an already-open session. `mode` decides which ops are even sent: `changes` and `apply` exist only in copy-back.
    pub fn new(lines: L, mode: WorkspaceMode) -> Self {
        Self {
            lines,
            mode,
            next_id: 1,
            closed: false,
        }
    }

    /// The transport, for a caller that needs to inspect it (tests read what was sent).
    pub fn lines(&self) -> &L {
        &self.lines
    }

    pub fn mode(&self) -> WorkspaceMode {
        self.mode
    }

    fn call(&mut self, mut req: Value) -> Result<Value, SessionError> {
        if self.closed {
            return Err(SessionError::Protocol("session is closed".into()));
        }
        let id = self.next_id;
        self.next_id += 1;
        req["id"] = json!(id);
        self.lines
            .send(&req.to_string())
            .map_err(SessionError::Transport)?;
        let line = self
            .lines
            .recv()
            .map_err(SessionError::Transport)?
            .ok_or_else(|| SessionError::Transport("spawn session ended without a reply".into()))?;
        let v: Value = serde_json::from_str(&line)
            .map_err(|e| SessionError::Protocol(format!("not JSON ({e}): {line}")))?;
        if v["id"].as_u64() != Some(id) {
            return Err(SessionError::Protocol(format!(
                "reply id {} is not the request id {id}",
                v["id"]
            )));
        }
        if v["op"] == "error" {
            return Err(SessionError::Refused(
                v["message"].as_str().unwrap_or("(no message)").to_string(),
            ));
        }
        Ok(v)
    }

    /// Run a command in the session.
    pub fn exec(&mut self, cmd: &[String]) -> Result<ExecResult, SessionError> {
        let v = self.call(json!({"op": "exec", "cmd": cmd}))?;
        expect_op(&v, "exec")?;
        let b64 = |k: &str| -> Result<Vec<u8>, SessionError> {
            STANDARD
                .decode(v[k].as_str().unwrap_or(""))
                .map_err(|e| SessionError::Protocol(format!("{k} is not base64: {e}")))
        };
        Ok(ExecResult {
            status: v["status"]
                .as_str()
                .ok_or_else(|| SessionError::Protocol("exec reply has no status".into()))?
                .to_string(),
            exit_code: v["exit_code"].as_i64(),
            stdout: b64("stdout")?,
            stderr: b64("stderr")?,
        })
    }

    /// Fetch and stage what the session changed. Copy-back only; a bind session is never asked.
    pub fn changes(&mut self) -> Result<Staged, SessionError> {
        self.need_copy_back("changes")?;
        let v = self.call(json!({"op": "changes"}))?;
        expect_op(&v, "changes")?;
        let set_hash = v["set_hash"].as_str().unwrap_or("");
        if set_hash.len() != 64 || !set_hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(SessionError::Protocol(format!(
                "set_hash is not 64 hex digits: {set_hash:?}"
            )));
        }
        Ok(Staged {
            added: files(&v["added"])?,
            modified: files(&v["modified"])?,
            deleted: strings(&v["deleted"])?,
            later_step: strings(&v["later_step"])?,
            refused: whys(&v["refused"])?,
            set_hash: set_hash.to_string(),
        })
    }

    /// Write exactly the staged set whose hash is `set_hash`. `deletes` and `protected` are the paths **a person approved
    /// by name**; this client never invents them. All or nothing: check [`Applied::written`].
    pub fn apply(
        &mut self,
        set_hash: &str,
        deletes: &BTreeSet<String>,
        protected: &BTreeSet<String>,
    ) -> Result<Applied, SessionError> {
        self.need_copy_back("apply")?;
        let v = self.call(json!({
            "op": "apply", "set_hash": set_hash, "deletes": deletes, "protected": protected,
        }))?;
        expect_op(&v, "applied")?;
        Ok(Applied {
            set_hash: v["set_hash"].as_str().unwrap_or("").to_string(),
            applied: strings(&v["applied"])?,
            refused: whys(&v["refused"])?,
            conflicts: whys(&v["conflicts"])?,
        })
    }

    /// End the session. Also ends it if this client is simply dropped (AntSpawn closes on end of input).
    pub fn close(&mut self) -> Result<(), SessionError> {
        if self.closed {
            return Ok(());
        }
        let r = self.call(json!({"op": "close"}));
        self.closed = true;
        r.map(|_| ())
    }

    fn need_copy_back(&self, op: &str) -> Result<(), SessionError> {
        if self.mode == WorkspaceMode::CopyBack {
            Ok(())
        } else {
            Err(SessionError::Refused(format!(
                "`{op}` exists only in copy-back; this session is {}",
                self.mode
            )))
        }
    }
}

fn expect_op(v: &Value, want: &str) -> Result<(), SessionError> {
    if v["op"] == want {
        Ok(())
    } else {
        Err(SessionError::Protocol(format!(
            "expected op `{want}`, got {}",
            v["op"]
        )))
    }
}

fn strings(v: &Value) -> Result<Vec<String>, SessionError> {
    v.as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|s| {
            s.as_str()
                .map(str::to_string)
                .ok_or_else(|| SessionError::Protocol(format!("not a string: {s}")))
        })
        .collect()
}

fn files(v: &Value) -> Result<Vec<StagedFile>, SessionError> {
    v.as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|f| {
            Ok(StagedFile {
                path: f["path"]
                    .as_str()
                    .ok_or_else(|| SessionError::Protocol(format!("file without path: {f}")))?
                    .to_string(),
                size: f["size"]
                    .as_u64()
                    .ok_or_else(|| SessionError::Protocol(format!("file without size: {f}")))?,
                sha256: f["sha256"]
                    .as_str()
                    .ok_or_else(|| SessionError::Protocol(format!("file without sha256: {f}")))?
                    .to_string(),
            })
        })
        .collect()
}

fn whys(v: &Value) -> Result<Vec<PathWhy>, SessionError> {
    v.as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|w| {
            Ok(PathWhy {
                path: w["path"]
                    .as_str()
                    .ok_or_else(|| SessionError::Protocol(format!("entry without path: {w}")))?
                    .to_string(),
                why: w["why"].as_str().unwrap_or("").to_string(),
            })
        })
        .collect()
}

/// The real transport: a `spawn session` child process.
pub struct ChildLines {
    child: Child,
    stdin: Option<ChildStdin>,
    out: BufReader<ChildStdout>,
}

impl ChildLines {
    /// Start `<spawn> session --launcher account --workspace <mode> --root <root>`.
    pub fn start(spawn: &Path, workspace: &Workspace) -> Result<Self, SandboxError> {
        let flag = match workspace.mode {
            WorkspaceMode::CopyBack => "copy",
            WorkspaceMode::Bind => "bind",
        };
        let mut child = Command::new(spawn)
            .args(["session", "--launcher", "account", "--workspace", flag])
            .arg("--root")
            .arg(&workspace.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| SandboxError::Unmet(format!("cannot start {}: {e}", spawn.display())))?;
        let stdin = child.stdin.take();
        let out = BufReader::new(child.stdout.take().expect("stdout was piped"));
        Ok(Self { child, stdin, out })
    }
}

impl Lines for ChildLines {
    fn send(&mut self, line: &str) -> Result<(), String> {
        let stdin = self.stdin.as_mut().ok_or("input already closed")?;
        stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.write_all(b"\n"))
            .and_then(|()| stdin.flush())
            .map_err(|e| e.to_string())
    }

    fn recv(&mut self) -> Result<Option<String>, String> {
        let mut s = String::new();
        match self.out.read_line(&mut s) {
            Ok(0) => Ok(None),
            Ok(_) => Ok(Some(s.trim_end().to_string())),
            Err(e) => Err(e.to_string()),
        }
    }
}

impl Drop for ChildLines {
    /// End of input is what tells `spawn session` to close; then reap, so no zombie outlives the session.
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.wait();
    }
}

/// The [`Sandbox`] over a `spawn` executable. `open_session` starts the child; a refusal to open (unmet capability, no
/// account) is not known until the first call, when it shows as a transport error (the child exited without a reply).
pub struct AntSpawnSandbox {
    spawn: std::path::PathBuf,
    /// The open session, if any: one at a time per sandbox value.
    pub session: Option<AntSpawnSession<ChildLines>>,
}

impl AntSpawnSandbox {
    pub fn new(spawn: impl Into<std::path::PathBuf>) -> Self {
        Self {
            spawn: spawn.into(),
            session: None,
        }
    }
}

impl Sandbox for AntSpawnSandbox {
    fn open_session(&mut self, _id: &str, workspace: &Workspace) -> Result<(), SandboxError> {
        if self.session.is_some() {
            return Err(SandboxError::Unmet("a session is already open".into()));
        }
        let lines = ChildLines::start(&self.spawn, workspace)?;
        self.session = Some(AntSpawnSession::new(lines, workspace.mode));
        Ok(())
    }
}
