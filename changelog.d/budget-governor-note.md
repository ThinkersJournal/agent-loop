kind: docs: budget governor design note
---
Adds `docs/BUDGET-GOVERNOR.md`, a design note only (no code): what usage the loop already reports, an in-run token cap that stops through the existing soft-stop path, the fleet cap as the harness adapter's `cost` report plus `SoftStop`, four decisions for the PM, and gap-list rows for the budget governor, the harness adapter and the eval harness. Verified by reading `src/driver.rs`, `src/run.rs`, `src/events.rs` and capant's `docs/AGENT-STACK-GAPS.md` at `origin/main`; nothing was run.
