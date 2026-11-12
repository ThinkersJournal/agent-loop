// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `network` rule family of the policy file (`docs/SANDBOX.md` 5.1), as a [`Hook`].
//!
//! ```toml
//! [[network]]
//! allow = ["crates.io", "*.crates.io"]      # or: deny = ["169.254.169.254", "*.internal.example"]
//! justification = "builds fetch from the registry only"
//! match = ["curl https://evil.example/x"]
//! not_match = ["curl https://static.crates.io/x"]
//! ```
//!
//! [`NetworkPolicy`] reads every URL (`scheme://authority`) in the text of a tool call's arguments and denies the call when a
//! rule refuses its host. A rule has exactly one of `deny` (a listed host is refused) or `allow` (a host not listed is refused).
//! A pattern is a host (`crates.io`) or `*.suffix`, which matches subdomains but not the bare suffix. Hosts are compared
//! lowercase, without user-info, port, brackets or a trailing dot. Like the other policies it can only deny (the hook seam has no
//! `Ask`) and a call with no URL is allowed here.
//!
//! **This is a reading of the text, not a firewall.** It does not see a host written without a scheme (`curl example.com`), an
//! address in another notation (`http://2130706433/`, which an `allow` rule still refuses and a `deny` rule does not see), a URL
//! built by the command at run time, or a program that connects on its own. Real enforcement is the launcher's network setting
//! (`network = off` for the session tier); this hook stops the shapes you name before the call runs.
//!
//! **Examples are tests.** `match` lines must each be denied by the rule that lists them and `not_match` lines must not be denied
//! by it; a file whose examples disagree with its patterns does not load, and at least one `match` example is required.

use serde::Deserialize;

use crate::argtext::all_strings;
use crate::hook::{Hook, ToolCall, ToolDecision};

#[derive(Debug, PartialEq, Eq)]
pub enum NetworkPolicyError {
    Toml(String),
    /// A rule is not usable, or one of its examples disagrees with it (the message says which and why).
    Rule(String),
}

impl std::fmt::Display for NetworkPolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Toml(m) => write!(f, "policy file: {m}"),
            Self::Rule(m) => write!(f, "policy network rule: {m}"),
        }
    }
}
impl std::error::Error for NetworkPolicyError {}

// Other tables belong to other readers; inside `[[network]]` an unknown key is an error, so a typo cannot weaken a rule.
#[derive(Deserialize)]
struct File {
    #[serde(default)]
    network: Vec<RuleDef>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleDef {
    deny: Option<Vec<String>>,
    allow: Option<Vec<String>>,
    justification: String,
    #[serde(default, rename = "match")]
    matches: Vec<String>,
    #[serde(default)]
    not_match: Vec<String>,
}

enum Pattern {
    Exact(String),
    /// `*.suffix`: the stored string is `.suffix`.
    Under(String),
}

impl Pattern {
    fn parse(p: &str) -> Option<Self> {
        let p = p.trim().to_lowercase();
        let p = p.strip_suffix('.').unwrap_or(&p);
        let valid = |h: &str| {
            !h.is_empty()
                && !h.contains('*')
                && !h.contains(|c: char| c.is_whitespace() || "/@:?#\\".contains(c))
        };
        match p.strip_prefix("*.") {
            Some(rest) if valid(rest) => Some(Self::Under(format!(".{rest}"))),
            None if valid(p) => Some(Self::Exact(p.to_string())),
            _ => None,
        }
    }
    fn matches(&self, host: &str) -> bool {
        match self {
            Self::Exact(h) => host == h,
            Self::Under(s) => host.ends_with(s.as_str()),
        }
    }
}

struct Rule {
    patterns: Vec<Pattern>,
    /// `true` for `allow` (refuse hosts NOT listed), `false` for `deny`.
    allow: bool,
    shown: String,
    justification: String,
}

/// Denies tool calls whose text holds a URL to a refused host. Install with `Driver::set_hook`.
pub struct NetworkPolicy {
    rules: Vec<Rule>,
}

/// The lowercase hosts of every `scheme://authority` in `text`.
fn hosts(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("://") {
        let before = &rest[..i];
        let after = &rest[i + 3..];
        let scheme_ok = before
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphabetic());
        let end = after
            .find(|c: char| c.is_whitespace() || "/?#\\\"'<>`)(;|&,".contains(c))
            .unwrap_or(after.len());
        let authority = &after[..end];
        rest = &after[end..];
        if !scheme_ok {
            continue;
        }
        let hostport = authority.rsplit('@').next().unwrap_or(authority);
        let host = match hostport.strip_prefix('[') {
            Some(v6) => v6.split(']').next().unwrap_or(v6),
            None => hostport.split(':').next().unwrap_or(hostport),
        };
        let host = host.trim_end_matches('.').to_lowercase();
        // An empty host (`file:///x`, `http://`) is not a network address.
        if !host.is_empty() {
            out.push(host);
        }
    }
    out
}

impl Rule {
    /// The first host in `text` this rule refuses.
    fn refuses(&self, text: &str) -> Option<String> {
        hosts(text).into_iter().find(|h| {
            let listed = self.patterns.iter().any(|p| p.matches(h));
            if self.allow { !listed } else { listed }
        })
    }
}

impl NetworkPolicy {
    /// Read the `[[network]]` rules of a policy file and check every rule's examples.
    pub fn from_toml(text: &str) -> Result<Self, NetworkPolicyError> {
        let file: File =
            toml::from_str(text).map_err(|e| NetworkPolicyError::Toml(e.to_string()))?;
        let mut rules = Vec::new();
        for def in file.network {
            let (allow, listed) = match (def.deny, def.allow) {
                (Some(d), None) => (false, d),
                (None, Some(a)) => (true, a),
                _ => {
                    return Err(NetworkPolicyError::Rule(
                        "a rule has exactly one of `deny` or `allow`".into(),
                    ));
                }
            };
            let shown = listed.join(", ");
            let err = |why: String| NetworkPolicyError::Rule(format!("[{shown}]: {why}"));
            if def.justification.trim().is_empty() {
                return Err(err("a rule needs a justification".into()));
            }
            if listed.is_empty() && !allow {
                return Err(err("`deny` lists no hosts".into()));
            }
            let mut patterns = Vec::new();
            for p in &listed {
                patterns.push(Pattern::parse(p).ok_or_else(|| {
                    err(format!(
                        "{p:?} is not a host or `*.suffix` (no scheme, port, path or other `*`)"
                    ))
                })?);
            }
            if def.matches.is_empty() {
                return Err(err("at least one `match` example is required".into()));
            }
            let rule = Rule {
                patterns,
                allow,
                shown: shown.clone(),
                justification: def.justification,
            };
            for ex in &def.matches {
                if rule.refuses(ex).is_none() {
                    return Err(err(format!(
                        "example {ex:?} is listed in `match` but is not denied"
                    )));
                }
            }
            for ex in &def.not_match {
                if rule.refuses(ex).is_some() {
                    return Err(err(format!(
                        "example {ex:?} is listed in `not_match` but is denied"
                    )));
                }
            }
            rules.push(rule);
        }
        Ok(Self { rules })
    }

    /// Why `text` (a command line, a URL argument) is denied, or `None` if it is allowed.
    pub fn check_text(&self, text: &str) -> Option<String> {
        self.rules.iter().find_map(|r| {
            r.refuses(text).map(|host| {
                format!(
                    "host {host:?} in {text:?} is denied by policy ({}): {}",
                    r.shown, r.justification
                )
            })
        })
    }
}

impl Hook for NetworkPolicy {
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

    const DENY: &str = r#"
[[network]]
deny = ["169.254.169.254", "*.internal.example"]
justification = "metadata service and the intranet"
match = ["curl http://169.254.169.254/latest/meta-data", "wget https://db.internal.example:8443/x"]
not_match = ["curl https://internal.example/x", "curl https://crates.io/a"]
"#;

    const ALLOW: &str = r#"
[[network]]
allow = ["crates.io", "*.crates.io"]
justification = "registry only"
match = ["curl https://evil.example/x"]
not_match = ["cargo fetch https://static.crates.io/a", "ls -la"]
"#;

    fn call(p: &mut NetworkPolicy, args: serde_json::Value) -> ToolDecision {
        p.before_tool(ToolCall {
            id: "1",
            name: "bash",
            args: &args,
        })
    }

    #[test]
    fn deny_rule_reads_hosts_in_any_argument_and_in_keys_free_text() {
        let mut p = NetworkPolicy::from_toml(DENY).unwrap();
        assert!(matches!(
            call(&mut p, json!({"command": "curl http://169.254.169.254/x"})),
            ToolDecision::Deny(_)
        ));
        assert!(matches!(
            call(
                &mut p,
                json!({"opts": [{"url": "https://A.Internal.Example./x"}]})
            ),
            ToolDecision::Deny(_)
        ));
        assert_eq!(
            call(&mut p, json!({"command": "curl https://crates.io/a"})),
            ToolDecision::Allow
        );
        // Positive control for the allow cases above: the same call shape is denied for the listed host.
        assert_eq!(
            call(&mut p, json!({"command": "echo no url here"})),
            ToolDecision::Allow
        );
    }

    #[test]
    fn user_info_port_and_ipv6_do_not_hide_the_host() {
        let p = NetworkPolicy::from_toml(DENY).unwrap();
        assert!(p.check_text("http://crates.io@169.254.169.254/x").is_some());
        assert!(p.check_text("http://169.254.169.254:80/x").is_some());
        assert!(
            p.check_text("http://user:pw@db.internal.example/")
                .is_some()
        );
        assert_eq!(hosts("http://[::1]:8080/x"), vec!["::1"]);
        assert!(hosts("file:///etc/passwd").is_empty());
        assert!(hosts("no scheme example.com").is_empty());
        // `://` with no scheme letter before it is not a URL.
        assert!(hosts("x ://169.254.169.254").is_empty());
    }

    #[test]
    fn allow_rule_refuses_every_unlisted_host_and_odd_notations() {
        let mut p = NetworkPolicy::from_toml(ALLOW).unwrap();
        assert!(matches!(
            call(
                &mut p,
                json!({"command": "curl https://crates.io/x https://evil.example/"})
            ),
            ToolDecision::Deny(_)
        ));
        assert!(p.check_text("http://2130706433/").is_some());
        assert!(p.check_text("https://crates.io.evil.example/").is_some());
        assert!(p.check_text("https://crates.io/a").is_none());
        assert!(p.check_text("https://static.crates.io/a").is_none());
    }

    #[test]
    fn wildcard_does_not_match_the_bare_suffix() {
        let p = NetworkPolicy::from_toml(DENY).unwrap();
        assert!(p.check_text("https://internal.example/").is_none());
        assert!(p.check_text("https://notinternal.example/").is_none());
        assert!(p.check_text("https://x.internal.example/").is_some());
    }

    #[test]
    fn bad_rules_and_disagreeing_examples_do_not_load() {
        let bad = |t: &str| NetworkPolicy::from_toml(t).err().expect("must not load");
        let body = |extra: &str| {
            format!(
                "[[network]]\n{extra}\njustification = \"j\"\nmatch = [\"curl http://a.example/\"]\n"
            )
        };
        assert!(matches!(bad(&body("")), NetworkPolicyError::Rule(_)));
        assert!(matches!(
            bad(&body("deny = [\"a.example\"]\nallow = [\"b.example\"]")),
            NetworkPolicyError::Rule(_)
        ));
        assert!(matches!(
            bad(&body("deny = []")),
            NetworkPolicyError::Rule(_)
        ));
        assert!(matches!(
            bad(&body("deny = [\"https://a.example\"]")),
            NetworkPolicyError::Rule(_)
        ));
        assert!(matches!(
            bad(&body("deny = [\"*.a.*\"]")),
            NetworkPolicyError::Rule(_)
        ));
        assert!(matches!(
            bad(&body("deny = [\"a.example\"]\nbogus = 1")),
            NetworkPolicyError::Toml(_)
        ));
        // A `match` example the rule does not deny.
        let e = bad(
            "[[network]]\ndeny = [\"x.example\"]\njustification = \"j\"\nmatch = [\"curl http://a.example/\"]\n",
        );
        assert!(e.to_string().contains("not denied"), "{e}");
        // No match example at all.
        let e = bad("[[network]]\ndeny = [\"x.example\"]\njustification = \"j\"\n");
        assert!(e.to_string().contains("`match` example is required"), "{e}");
        // A `not_match` example the rule does deny.
        let e = bad(
            "[[network]]\ndeny = [\"a.example\"]\njustification = \"j\"\nmatch = [\"http://a.example/\"]\nnot_match = [\"http://a.example/y\"]\n",
        );
        assert!(e.to_string().contains("is denied"), "{e}");
        // Empty justification.
        let e = bad(
            "[[network]]\ndeny = [\"a.example\"]\njustification = \" \"\nmatch = [\"http://a.example/\"]\n",
        );
        assert!(e.to_string().contains("justification"), "{e}");
    }

    #[test]
    fn a_file_without_network_rules_allows_everything() {
        let mut p =
            NetworkPolicy::from_toml("[[path]]\ndeny = [\".git\"]\njustification = \"j\"\n")
                .unwrap();
        assert_eq!(
            call(&mut p, json!({"command": "curl http://169.254.169.254/"})),
            ToolDecision::Allow
        );
    }
}
