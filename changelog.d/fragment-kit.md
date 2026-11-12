kind: changelog fragments adopted
---
Pull requests no longer edit `CHANGELOG.md`, a `Cargo.toml` version or `Cargo.lock`: each adds one `changelog.d/<topic>.md` fragment (format in `changelog.d/README.md`) and the gate stamps the version it allocates with `tools/stamp-version.py`. Adds the portfolio fragment-stamp kit unchanged (`tools/stamp-version.py`, `tools/check-fragment.py`) and the `Changelog fragment` workflow (`.github/workflows/changelog-fragment.yml`) that fails a PR with no fragment or with two. Verified with `python tools/stamp-version.py 0.1.16 --dry-run` on this branch.
