// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `path deny` rule family of the policy file (`docs/SANDBOX.md` 5.1 and 5.6), as a [`Hook`].
//!
//! ```toml
//! [[path]]
//! deny = [".git", ".github", "CLAUDE.md"]
//! justification = "a write here changes what runs later, outside the sandbox"
//! ```
//!
//! [`PathPolicy`] denies a tool call when any path it names falls under a denied entry. It applies in both workspace modes. It
//! can only deny: the hook seam has no `Ask`, so a path that matches no rule is allowed here and whatever else governs the call
//! (the write-back approval, the sandbox) still applies.
//!
//! **Which arguments are paths** is a convention, not something the loop can know: by default the string values (or arrays of
//! strings) under the argument names `path`, `file_path`, `paths`, `from` and `to`, found at any depth. [`PathPolicy::with_keys`]
//! changes the list. A tool that names a path under another argument name is not covered until that name is added.
//!
//! **Fail closed on shape.** Paths are workspace-relative. A path that is absolute (a leading `/` or `\`, or a drive letter) or
//! that has a `..` component is denied outright, as outside the workspace, whatever the rules say. Matching is on whole path
//! components, ignores case (the real trees are on Windows) and treats `\` as `/`; `src/.gitx` does not match `.git`.
//! A malformed file is an error, never an empty rule set: unknown keys, an empty `deny`, an empty entry or a missing
//! justification are all refused.

use serde::Deserialize;

use crate::hook::{Hook, ToolCall, ToolDecision};

#[derive(Debug, PartialEq, Eq)]
pub enum PathPolicyError {
    Toml(String),
    /// A rule is not usable (the message says which and why).
    Rule(String),
}

impl std::fmt::Display for PathPolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Toml(m) => write!(f, "policy file: {m}"),
            Self::Rule(m) => write!(f, "policy path rule: {m}"),
        }
    }
}
impl std::error::Error for PathPolicyError {}

// Other tables (workspace, commands, network) belong to other readers, so unknown top-level tables are ignored here; inside a
// `[[path]]` entry an unknown key is an error, so a typo cannot silently weaken a rule.
#[derive(Deserialize)]
struct File {
    #[serde(default)]
    path: Vec<RuleDef>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleDef {
    deny: Vec<String>,
    justification: String,
}

struct Rule {
    /// The denied entry as lowercase components.
    components: Vec<String>,
    shown: String,
    justification: String,
}

/// Denies tool calls that name a denied path. Install with `Driver::set_hook`.
pub struct PathPolicy {
    rules: Vec<Rule>,
    keys: Vec<String>,
}

/// Split a workspace-relative path into lowercase components; `None` if it is not a plain relative path.
fn components(path: &str) -> Option<Vec<String>> {
    let p = path.replace('\\', "/");
    let bytes = p.as_bytes();
    if p.starts_with('/')
        || (bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic())
    {
        return None;
    }
    let mut out = Vec::new();
    for part in p.split('/') {
        match part {
            "" | "." => {}
            ".." => return None,
            other => out.push(other.to_lowercase()),
        }
    }
    Some(out)
}

impl PathPolicy {
    /// Read the `[[path]]` rules of a policy file.
    pub fn from_toml(text: &str) -> Result<Self, PathPolicyError> {
        let file: File = toml::from_str(text).map_err(|e| PathPolicyError::Toml(e.to_string()))?;
        let mut rules = Vec::new();
        for def in file.path {
            if def.justification.trim().is_empty() {
                return Err(PathPolicyError::Rule("a rule needs a justification".into()));
            }
            if def.deny.is_empty() {
                return Err(PathPolicyError::Rule("`deny` lists no paths".into()));
            }
            for entry in def.deny {
                let comps = components(&entry)
                    .filter(|c| !c.is_empty())
                    .ok_or_else(|| {
                        PathPolicyError::Rule(format!(
                            "{entry:?} is empty, absolute or has `..`; entries are workspace-relative"
                        ))
                    })?;
                rules.push(Rule {
                    components: comps,
                    shown: entry,
                    justification: def.justification.clone(),
                });
            }
        }
        Ok(Self {
            rules,
            keys: ["path", "file_path", "paths", "from", "to"]
                .map(String::from)
                .to_vec(),
        })
    }

    /// Replace the argument names that hold paths.
    pub fn with_keys(mut self, keys: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.keys = keys.into_iter().map(Into::into).collect();
        self
    }

    /// Why `path` is denied, or `None` if it is allowed.
    pub fn check_path(&self, path: &str) -> Option<String> {
        let Some(comps) = components(path) else {
            return Some(format!(
                "path {path:?} is outside the workspace (absolute or has `..`)"
            ));
        };
        self.rules
            .iter()
            .find(|r| comps.starts_with(&r.components))
            .map(|r| {
                format!(
                    "path {path:?} is denied by policy ({}): {}",
                    r.shown, r.justification
                )
            })
    }

    fn collect<'a>(&self, v: &'a serde_json::Value, out: &mut Vec<&'a str>) {
        match v {
            serde_json::Value::Object(m) => {
                for (k, val) in m {
                    if self.keys.contains(k) {
                        match val {
                            serde_json::Value::String(s) => out.push(s),
                            serde_json::Value::Array(a) => {
                                out.extend(a.iter().filter_map(|x| x.as_str()));
                            }
                            _ => {}
                        }
                    } else {
                        self.collect(val, out);
                    }
                }
            }
            serde_json::Value::Array(a) => a.iter().for_each(|x| self.collect(x, out)),
            _ => {}
        }
    }
}

impl Hook for PathPolicy {
    fn before_tool(&mut self, call: ToolCall<'_>) -> ToolDecision {
        let mut paths = Vec::new();
        self.collect(call.args, &mut paths);
        for p in paths {
            if let Some(why) = self.check_path(p) {
                return ToolDecision::Deny(why);
            }
        }
        ToolDecision::Allow
    }
}
