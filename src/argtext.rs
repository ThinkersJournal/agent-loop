// SPDX-License-Identifier: MIT OR Apache-2.0
//! Every piece of text in a tool call's arguments, for the policies that cannot know which argument name holds what
//! ([`NetworkPolicy`](crate::NetworkPolicy), [`SecretPolicy`](crate::SecretPolicy)): a URL or a secret's name can be in a command
//! line, a `url` argument, an `env` map's keys or anywhere else.

/// Push every string value and every object key found at any depth.
pub(crate) fn all_strings<'a>(v: &'a serde_json::Value, out: &mut Vec<&'a str>) {
    match v {
        serde_json::Value::String(s) => out.push(s),
        serde_json::Value::Object(m) => {
            for (k, val) in m {
                out.push(k);
                all_strings(val, out);
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| all_strings(x, out)),
        _ => {}
    }
}
