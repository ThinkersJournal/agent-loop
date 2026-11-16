kind: chore: portfolio workflow pinned to CodeRipper 0.47.5
---
`.github/workflows/ci.yml` now calls `ciresnave/coderipper/.github/workflows/portfolio.yml` at `c72a6e03479d914de8ea8a73609ba73e93a0de92` (was `33cc3b7e4ab9305b5244092842fc7f6f6b686983`), which installs CodeRipper 0.47.5. The rule-id migration tool (`tools/migrate-rule-ids.py` at coderipper `c72a6e0`) reports "nothing to migrate": `.coderipper.toml` has no `check =` or `blocking =` entries and no workflow passes `--skip-rule`. No code change.
