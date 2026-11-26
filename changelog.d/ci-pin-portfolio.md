kind: chore: pin the advisory portfolio workflow to coderipper 0.48.1
---
Moved the `portfolio` job in `.github/workflows/ci.yml` to `ciresnave/coderipper/.github/workflows/portfolio.yml@467fd259d6d062cba4c5683d310f1a6f2b66e763` (CODERIPPER_VERSION 0.48.1). Workflow pin only, no code change; the job stays advisory and is verified by its check result on this PR.
