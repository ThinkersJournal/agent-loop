// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `command` deny rule family of the policy file (`docs/SANDBOX.md` 5.1), as a [`Hook`].
//!
//! ```toml
//! [[command]]
//! prefix = ["git", "push"]
//! decision = "deny"
//! justification = "publishing is the PM's step"
//! match = ["git push origin main", "cd x && git push"]
//! not_match = ["git pull", "git status"]
//! ```
//!
//! [`CommandPolicy`] denies a tool call whose command line starts, in any shell segment, with a rule's `prefix`. Like
//! [`PathPolicy`](crate::PathPolicy) it can only deny: the hook seam has no `Ask`, so `decision` must be `"deny"` (`"allow"` and
//! `"ask"` are refused at load rather than silently ignored), and a command that matches no rule is allowed here.
//!
//! **Examples are tests.** `match` lines must each be denied by the rule that lists them and `not_match` lines must not be denied
//! by it; a file whose examples disagree with its prefix does not load, and at least one `match` example is required.
//!
//! **How a command line is read.** It is split into segments at `;`, `&&`, `||`, `|`, `&`, a newline, a backtick and the
//! characters `( ) { }`, so `echo ok; git push` and `$(git push)` are both seen. In a segment, words split at whitespace outside quotes (a quoted program path may contain spaces), quote characters are dropped,
//! leading `NAME=value` assignments are skipped, and the program name is lowercased with any directory and a trailing `.exe`,
//! `.cmd`, `.bat` or `.com` removed (`/usr/bin/Git.EXE` is `git`). Tokens after the program are compared as written, ignoring
//! case. This is a reading of the text, not a shell parser: it **fails closed on separators inside quotes** (`echo "a; git push"`
//! is denied) and **does not see through** options placed between the words of a prefix (`git -C dir push` does not match
//! `["git", "push"]`), variable expansion, `eval` or a script file the command runs. Name the shapes you need to stop as rules.

use serde::Deserialize;

use crate::hook::{Hook, ToolCall, ToolDecision};

#[derive(Debug, PartialEq, Eq)]
pub enum CommandPolicyError {
    Toml(String),
    /// A rule is not usable, or one of its examples disagrees with it (the message says which and why).
    Rule(String),
}

impl std::fmt::Display for CommandPolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Toml(m) => write!(f, "policy file: {m}"),
            Self::Rule(m) => write!(f, "policy command rule: {m}"),
        }
    }
}
impl std::error::Error for CommandPolicyError {}

// Other tables belong to other readers, so unknown top-level tables are ignored; inside `[[command]]` an unknown key is an error.
#[derive(Deserialize)]
struct File {
    #[serde(default)]
    command: Vec<RuleDef>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleDef {
    prefix: Vec<String>,
    decision: String,
    justification: String,
    #[serde(default, rename = "match")]
    matches: Vec<String>,
    #[serde(default)]
    not_match: Vec<String>,
}

struct Rule {
    /// Program (normalized) followed by the argument words, all lowercase.
    prefix: Vec<String>,
    shown: String,
    justification: String,
}

/// Denies tool calls whose command line starts with a denied prefix. Install with `Driver::set_hook`.
pub struct CommandPolicy {
    rules: Vec<Rule>,
    keys: Vec<String>,
}

fn is_separator(c: char) -> bool {
    matches!(
        c,
        ';' | '&' | '|' | '\n' | '\r' | '`' | '(' | ')' | '{' | '}'
    )
}

fn program_name(token: &str) -> String {
    let t = token.replace('\\', "/").to_lowercase();
    let base = t.rsplit('/').next().unwrap_or(&t);
    for ext in [".exe", ".cmd", ".bat", ".com"] {
        if let Some(stem) = base.strip_suffix(ext) {
            return stem.to_string();
        }
    }
    base.to_string()
}

fn is_assignment(token: &str) -> bool {
    match token.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && !name.starts_with(|c: char| c.is_ascii_digit())
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

/// Split a segment into words at whitespace outside quotes; the quote characters are dropped, an unbalanced quote runs to the
/// end, and an empty word (`""`) is dropped.
fn words(seg: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in seg.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => quote = Some(c),
            (None, c) if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The normalized word lists of each segment of a command line (program first); empty segments are dropped.
fn segments(command: &str) -> Vec<Vec<String>> {
    command
        .split(is_separator)
        .filter_map(|seg| {
            let mut words = words(seg);
            let skip = words.iter().take_while(|w| is_assignment(w)).count();
            words.drain(..skip);
            let first = words.first_mut()?;
            *first = program_name(first);
            for w in words.iter_mut().skip(1) {
                *w = w.to_lowercase();
            }
            Some(words)
        })
        .collect()
}

impl CommandPolicy {
    /// Read the `[[command]]` rules of a policy file and check every rule's examples.
    pub fn from_toml(text: &str) -> Result<Self, CommandPolicyError> {
        let file: File =
            toml::from_str(text).map_err(|e| CommandPolicyError::Toml(e.to_string()))?;
        let mut rules = Vec::new();
        for def in file.command {
            let shown = def.prefix.join(" ");
            let err = |why: String| CommandPolicyError::Rule(format!("[{shown}]: {why}"));
            if def.decision != "deny" {
                return Err(err(format!(
                    "decision {:?} is not supported: a hook can only deny, so use \"deny\"",
                    def.decision
                )));
            }
            if def.justification.trim().is_empty() {
                return Err(err("a rule needs a justification".into()));
            }
            if def.prefix.is_empty() || def.prefix.iter().any(|w| w.trim().is_empty()) {
                return Err(err("`prefix` needs non-empty words".into()));
            }
            if def.prefix.iter().any(|w| w.contains(char::is_whitespace)) {
                return Err(err("each `prefix` word is one word, without spaces".into()));
            }
            if def.matches.is_empty() {
                return Err(err("at least one `match` example is required".into()));
            }
            let mut prefix: Vec<String> = def.prefix.iter().map(|w| w.to_lowercase()).collect();
            prefix[0] = program_name(&prefix[0]);
            let rule = Rule {
                prefix,
                shown: shown.clone(),
                justification: def.justification,
            };
            for ex in &def.matches {
                if !rule.denies(ex) {
                    return Err(err(format!(
                        "example {ex:?} is listed in `match` but is not denied"
                    )));
                }
            }
            for ex in &def.not_match {
                if rule.denies(ex) {
                    return Err(err(format!(
                        "example {ex:?} is listed in `not_match` but is denied"
                    )));
                }
            }
            rules.push(rule);
        }
        Ok(Self {
            rules,
            keys: ["command", "cmd"].map(String::from).to_vec(),
        })
    }

    /// Replace the argument names that hold a command line.
    pub fn with_keys(mut self, keys: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.keys = keys.into_iter().map(Into::into).collect();
        self
    }

    /// Why `command` is denied, or `None` if it is allowed.
    pub fn check_command(&self, command: &str) -> Option<String> {
        let segs = segments(command);
        self.rules.iter().find(|r| r.matches_any(&segs)).map(|r| {
            format!(
                "command {command:?} is denied by policy ({}): {}",
                r.shown, r.justification
            )
        })
    }

    fn collect<'a>(&self, v: &'a serde_json::Value, out: &mut Vec<&'a str>) {
        match v {
            serde_json::Value::Object(m) => {
                for (k, val) in m {
                    match val {
                        serde_json::Value::String(s) if self.keys.contains(k) => out.push(s),
                        _ => self.collect(val, out),
                    }
                }
            }
            serde_json::Value::Array(a) => a.iter().for_each(|x| self.collect(x, out)),
            _ => {}
        }
    }
}

impl Rule {
    fn matches_any(&self, segs: &[Vec<String>]) -> bool {
        segs.iter().any(|s| s.starts_with(&self.prefix))
    }
    fn denies(&self, command: &str) -> bool {
        self.matches_any(&segments(command))
    }
}

impl Hook for CommandPolicy {
    fn before_tool(&mut self, call: ToolCall<'_>) -> ToolDecision {
        let mut commands = Vec::new();
        self.collect(call.args, &mut commands);
        for c in commands {
            if let Some(why) = self.check_command(c) {
                return ToolDecision::Deny(why);
            }
        }
        ToolDecision::Allow
    }
}
