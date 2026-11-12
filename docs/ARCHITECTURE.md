# Architecture declaration and ARC/MOD findings

The declaration is the `[architecture]` table in `.coderipper.toml`: 21 components (the top-level modules of the one crate)
in four layers, bottom first: `foundation` (argtext, changeset, context, events, hook, inbox, run, store, workspace),
`stores` (file, records, blobstore, blobinbox), `core` (control, driver), `extensions` (the four policy hooks, journal,
writeback). A component may depend on its own layer or one below. Measured 2026-11-12 at main 3b3c7e9 from `crate::<top>::`
paths, not a type-resolved graph. No waivers.

## Findings from the 2026-11-12 census (coderipper 0.43.59 build, rules that need no declaration)

| Rule | Finding | Disposition |
| :--- | :--- | :--- |
| MOD-002 | `driver` and `hook` referred to each other (`hook` named `driver::ToolOutcome`) | FIXED: `ToolOutcome` now lives in `src/hook.rs`; `src/driver.rs` re-exports it, so `agent_loop::ToolOutcome` and `driver::ToolOutcome` are unchanged |
| MOD-005 | `Driver` has 30 members (threshold 30) | SCHEDULED, not urgent: split run-setup builders from the loop when the next feature adds a member |
| MOD-005 | `src/lib.rs` names 20 modules (threshold 14) | DECLINED: it is the crate's public facade; its job is to name every module and re-export |
