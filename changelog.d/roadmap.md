kind: docs: `ROADMAP.md` added
---
Adds `ROADMAP.md`: every row either done, naming the `cargo test` command that shows it, or open, naming its blocker, plus a table of each README headline claim with the command that shows it. Written at 225e369 after `cargo test --all-features` passed with exit 0 (every test binary `ok`, 0 failed, 0 ignored). No code change. Rows cite test files by name; whether a given test exercises the whole claim in its row was not audited test by test.
