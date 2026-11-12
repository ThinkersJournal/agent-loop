// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `secret` rule family of the policy file (`docs/SANDBOX.md` 5.1), as a [`Hook`].
//!
//! ```toml
//! [[secret]]
//! names = ["GITHUB_TOKEN", "AWS_SECRET_ACCESS_KEY"]
//! justification = "a run never reads or prints a credential"
//! match = ["echo $GITHUB_TOKEN", "printenv github_token"]
//! not_match = ["echo $GITHUB_TOKEN_FILE"]
//! ```
//!
//! Secrets are **names only** (as AntSpawn's job spec): the policy never holds a value. [`SecretPolicy`] denies a tool call when
//! any text in its arguments (a string value or an object key, at any depth) mentions a listed name as a whole identifier,
//! ignoring case: `$NAME`, `${NAME}`, `%NAME%`, `$env:NAME`, `printenv NAME` and an `env` map with the key `NAME` are all seen,
//! while `NAME_FILE` and `MY_NAME` are different names and are not. It can only deny (the hook seam has no `Ask`).
//!
//! **A reading of the text, not a guarantee.** It does not see a name built at run time (`${!var}`, `"GITHUB_"+"TOKEN"`), a
//! dump of the whole environment (`env`, `set`, `Get-ChildItem Env:`), or a program that reads the variable itself. Keeping the
//! secret out of the sandbox's environment is the launcher's job; this hook stops the calls that name it.
//!
//! **Examples are tests.** `match` lines must each be denied by the rule that lists them and `not_match` lines must not be denied
//! by it; a file whose examples disagree with its names does not load, and at least one `match` example is required.

use serde::Deserialize;

use crate::argtext::all_strings;
use crate::hook::{Hook, ToolCall, ToolDecision};

#[derive(Debug, PartialEq, Eq)]
pub enum SecretPolicyError {
    Toml(String),
    /// A rule is not usable, or one of its examples disagrees with it (the message says which and why).
    Rule(String),
}

impl std::fmt::Display for SecretPolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Toml(m) => write!(f, "policy file: {m}"),
            Self::Rule(m) => write!(f, "policy secret rule: {m}"),
        }
    }
}
impl std::error::Error for SecretPolicyError {}

// Other tables belong to other readers; inside `[[secret]]` an unknown key is an error, so a typo cannot weaken a rule.
#[derive(Deserialize)]
struct File {
    #[serde(default)]
    secret: Vec<RuleDef>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleDef {
    names: Vec<String>,
    justification: String,
    #[serde(default, rename = "match")]
    matches: Vec<String>,
    #[serde(default)]
    not_match: Vec<String>,
}

struct Rule {
    /// Lowercase names.
    names: Vec<String>,
    shown: String,
    justification: String,
}

/// Denies tool calls that name a listed secret. Install with `Driver::set_hook`.
pub struct SecretPolicy {
    rules: Vec<Rule>,
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether `name` (lowercase) occurs in `text` bounded by non-identifier characters or the ends.
fn mentions(text: &str, name: &str) -> bool {
    let t = text.to_lowercase();
    let mut from = 0;
    while let Some(i) = t[from..].find(name) {
        let start = from + i;
        let end = start + name.len();
        let left = t[..start].chars().next_back().is_none_or(|c| !is_ident(c));
        let right = t[end..].chars().next().is_none_or(|c| !is_ident(c));
        if left && right {
            return true;
        }
        // Step one character, not one byte, so the next slice starts on a boundary.
        from = start + t[start..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

impl Rule {
    fn names_in(&self, text: &str) -> Option<&str> {
        self.names
            .iter()
            .find(|n| mentions(text, n))
            .map(String::as_str)
    }
}

impl SecretPolicy {
    /// Read the `[[secret]]` rules of a policy file and check every rule's examples.
    pub fn from_toml(text: &str) -> Result<Self, SecretPolicyError> {
        let file: File =
            toml::from_str(text).map_err(|e| SecretPolicyError::Toml(e.to_string()))?;
        let mut rules = Vec::new();
        for def in file.secret {
            let shown = def.names.join(", ");
            let err = |why: String| SecretPolicyError::Rule(format!("[{shown}]: {why}"));
            if def.justification.trim().is_empty() {
                return Err(err("a rule needs a justification".into()));
            }
            if def.names.is_empty() {
                return Err(err("`names` lists no secrets".into()));
            }
            if let Some(bad) = def
                .names
                .iter()
                .find(|n| n.is_empty() || n.contains(|c: char| !is_ident(c)))
            {
                return Err(err(format!(
                    "{bad:?} is not a name (letters, digits and `_` only; names, never values)"
                )));
            }
            if def.matches.is_empty() {
                return Err(err("at least one `match` example is required".into()));
            }
            let rule = Rule {
                names: def.names.iter().map(|n| n.to_lowercase()).collect(),
                shown: shown.clone(),
                justification: def.justification,
            };
            for ex in &def.matches {
                if rule.names_in(ex).is_none() {
                    return Err(err(format!(
                        "example {ex:?} is listed in `match` but is not denied"
                    )));
                }
            }
            for ex in &def.not_match {
                if rule.names_in(ex).is_some() {
                    return Err(err(format!(
                        "example {ex:?} is listed in `not_match` but is denied"
                    )));
                }
            }
            rules.push(rule);
        }
        Ok(Self { rules })
    }

    /// Why `text` is denied, or `None` if it is allowed. The reason names the secret, never the text (which may hold a value).
    pub fn check_text(&self, text: &str) -> Option<String> {
        self.rules.iter().find_map(|r| {
            r.names_in(text).map(|n| {
                format!(
                    "a call naming secret {n:?} is denied by policy ({}): {}",
                    r.shown, r.justification
                )
            })
        })
    }
}

impl Hook for SecretPolicy {
    fn before_tool(&mut self, call: ToolCall<'_>) -> ToolDecision {
        let mut texts = Vec::new();
        all_strings(call.args, &mut texts);
        for t in texts {
            if let Some(why) = self.check_text(t) {
                return ToolDecision::Deny(why);
            }
        }
        ToolDecision::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const POLICY: &str = r#"
[[secret]]
names = ["GITHUB_TOKEN", "AWS_SECRET_ACCESS_KEY"]
justification = "a run never reads a credential"
match = ["echo $GITHUB_TOKEN", "printenv github_token", "echo %AWS_SECRET_ACCESS_KEY%"]
not_match = ["echo $GITHUB_TOKEN_FILE", "echo MY_GITHUB_TOKEN", "cargo build"]
"#;

    fn call(p: &mut SecretPolicy, args: serde_json::Value) -> ToolDecision {
        p.before_tool(ToolCall {
            id: "1",
            name: "bash",
            args: &args,
        })
    }

    #[test]
    fn every_way_of_writing_the_name_is_seen() {
        let p = SecretPolicy::from_toml(POLICY).unwrap();
        for t in [
            "echo $GITHUB_TOKEN",
            "echo ${GITHUB_TOKEN}",
            "echo %github_token%",
            "echo $env:GitHub_Token",
            "printenv GITHUB_TOKEN",
            "GITHUB_TOKEN",
        ] {
            assert!(p.check_text(t).is_some(), "{t}");
        }
        // Positive control: the same shapes with another name are not denied.
        for t in [
            "echo $HOME",
            "echo ${HOME}",
            "echo $GITHUB_TOKEN2",
            "GITHUB_TOKENS",
            "x.GITHUB_TOKEN_y",
        ] {
            assert!(p.check_text(t).is_none(), "{t}");
        }
    }

    #[test]
    fn found_in_values_and_keys_at_any_depth_and_the_reason_names_no_value() {
        let mut p = SecretPolicy::from_toml(POLICY).unwrap();
        assert!(matches!(
            call(&mut p, json!({"command": "echo $GITHUB_TOKEN"})),
            ToolDecision::Deny(_)
        ));
        let ToolDecision::Deny(why) =
            call(&mut p, json!({"env": {"GITHUB_TOKEN": "ghp_realvalue"}}))
        else {
            panic!("an env key naming a secret is denied");
        };
        assert!(
            why.contains("github_token") && !why.contains("ghp_realvalue"),
            "{why}"
        );
        assert!(matches!(
            call(
                &mut p,
                json!({"a": [{"b": ["x", "run ${aws_secret_access_key}"]}]})
            ),
            ToolDecision::Deny(_)
        ));
        assert_eq!(
            call(
                &mut p,
                json!({"command": "cargo build", "env": {"HOME": "x"}})
            ),
            ToolDecision::Allow
        );
    }

    #[test]
    fn non_ascii_text_does_not_panic_and_still_matches() {
        let p = SecretPolicy::from_toml(POLICY).unwrap();
        assert!(p.check_text("é$GITHUB_TOKEN").is_some());
        assert!(p.check_text("éé GITHUB_TOKENé").is_none());
        assert!(p.check_text("İ github_token").is_some());
    }

    #[test]
    fn bad_rules_and_disagreeing_examples_do_not_load() {
        let bad = |t: &str| SecretPolicy::from_toml(t).err().expect("must not load");
        let rule = |names: &str, rest: &str| {
            format!("[[secret]]\nnames = {names}\njustification = \"j\"\n{rest}")
        };
        assert!(
            bad(&rule("[]", "match = [\"$A\"]"))
                .to_string()
                .contains("no secrets")
        );
        assert!(
            bad(&rule("[\"A B\"]", "match = [\"$A\"]"))
                .to_string()
                .contains("not a name")
        );
        assert!(
            bad(&rule("[\"\"]", "match = [\"$A\"]"))
                .to_string()
                .contains("not a name")
        );
        assert!(
            bad(&rule("[\"A\"]", ""))
                .to_string()
                .contains("`match` example is required")
        );
        assert!(
            bad(&rule("[\"A\"]", "match = [\"echo B\"]"))
                .to_string()
                .contains("not denied")
        );
        assert!(
            bad(&rule(
                "[\"A\"]",
                "match = [\"echo $A\"]\nnot_match = [\"echo $A\"]"
            ))
            .to_string()
            .contains("is denied")
        );
        assert!(matches!(
            bad(&rule("[\"A\"]", "match = [\"$A\"]\nbogus = 1")),
            SecretPolicyError::Toml(_)
        ));
        assert!(
            bad("[[secret]]\nnames = [\"A\"]\njustification = \" \"\nmatch = [\"$A\"]\n")
                .to_string()
                .contains("justification")
        );
    }

    #[test]
    fn a_file_without_secret_rules_allows_everything() {
        let mut p = SecretPolicy::from_toml("[[path]]\ndeny = [\".git\"]\njustification = \"j\"\n")
            .unwrap();
        assert_eq!(
            call(&mut p, json!({"command": "echo $GITHUB_TOKEN"})),
            ToolDecision::Allow
        );
    }
}
