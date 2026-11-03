# Tool interface (layer D): design note

Status: DRAFT for the PM gate. Nothing here is built. Author: the `agent-loop` lane. Task: PM `[TASK]` 2026-11-03 (notes D, E, F
after C and G). Map: `C:\Projects\AGENT-STACK.md` (row D; seams C|D and D|E). Companions: `docs/LOOP-CORE.md`,
`docs/HARNESS-ADAPTER.md`. Read-only facts carry a file or URL and a ref or date (2026-11-03 UTC unless stated).

## 1. The intended hello-world (ten lines)

What a consumer writes to give the loop tools. Target API, not final names.

```rust
let tools = ToolHost::builder()
    .native(rig_tools::Read::default())                              // in-process tool
    .mcp_stdio("synapse", "synapse-claude-channel", ["--role", "kiss"])?   // MCP server we start
    .mcp_http("serena", "http://localhost:9962")?                    // MCP server someone else runs
    .policy(my_policy)                                               // layer E decides every call, section 4.3
    .build().await?;
let defs  = tools.definitions();                                     // what the model is offered: synapse__send, serena__find_symbol, read ...
let out   = tools.call(&call, &ctx).await?;                          // Allowed -> run -> ToolOutcome { text, structured, is_error }
```

Properties this implies: one call shape for native and MCP tools (C|D seam: tool call in, result out); every call passes the
policy first (D|E seam); tool names are namespaced so two servers cannot collide; servers come from a config file the
fleet already writes.

## 2. What D is, and is not

STACK row D: MCP client, native tools, tool-result handling. STACK's own revision (PM reading, 2026-10-31) splits D in two:
**D1 the tool host inside a harness** (this note) and **D2 the tool providers** (MCP servers, mostly not ours to write).

Not D: deciding whether a call is allowed (E), the loop that asks for calls (C), or what the model is offered when the
context is full (C/F). MCP is **not** at the model gateway (B|C); OverMind passes schemas and runs no tools.

## 3. What exists today

### 3.1 In our fleet (requirements, not designs to copy)

| Item | What it is | Evidence |
|---|---|---|
| Lane MCP config | `C:\Projects\.lane-state\<role>.mcp.json`, Claude Code's `mcpServers` shape: per server `command`, `args`, `env`. This lane's file names one server, `synapse`, a stdio binary `synapse-claude-channel.exe` | read 2026-11-03 (`agent-loop.mcp.json`); `agentlife` `SUPERVISION-INPUTS.md` section 1.1 |
| Synapse as a D2 provider | Stdio MCP adapter over `synapsed`; tools `ack fetch list send set_summary whoami`. README: "Mail text is untrusted input." | `synapse` `crates/synapse-mcp-server/README.md` at `ffe9964d`; tool list as seen in this session |
| Serena | One shared server at `http://localhost:9962`; lanes must never start their own or call `activate_project` | `C:\Projects\CLAUDE.md` section 1. Protocol and tool list **not read** here |
| What Claude Code does here | Starts the configured servers, offers their tools to the model, runs calls, feeds results back. Internals closed; behaviour only |  |

### 3.2 Candidates to ADOPT

Method as in `LOOP-CORE.md`: `gh api`, crates.io API and repo files at named commits, 2026-11-03. Licence = root LICENSE text
at the commit.

| Candidate | Licence | Covers | Lacks / risk | Evidence |
|---|---|---|---|---|
| **rmcp** (modelcontextprotocol/rust-sdk), official Rust MCP SDK | Workspace `Cargo.toml` says `Apache-2.0`; root LICENSE is the MIT-to-Apache transition notice (new code Apache-2.0, older MIT-only contributions stay MIT). Commit `b166e815` | Client and server. Features: `client`; transports `transport-child-process` (spawn a stdio server), `transport-streamable-http-client` (+ `-reqwest`), unix-socket variant, `transport-io`; `elicitation`; `request-state` helper. **MSRV 1.88** (`rust-version = "1.88"`), equal to our seed. 3.8.0, 43,886,156 downloads | A client library, not a tool host: no namespacing, approval, limits or truncation. SSE-only transport not listed in features read (not checked beyond the feature table) | `Cargo.toml:12-16` (workspace); `crates/rmcp/Cargo.toml` `[features]`; crates.io API |
| **rig-rmcp** (0xPlaygrounds/rig), MCP client glue | MIT (`4908f428`) | `McpConnection` owns an rmcp client; `McpTool` converts to a rig dynamic tool; `with_name` renames to avoid collisions; per-call `McpMeta` forwarded as `_meta`; raw `CallToolResult` (`structuredContent`, `_meta`) published to hooks; resources readable as agent context; `McpClientHandler` keeps a registry in sync with `notifications/tools/list_changed`; depends on `rig-core` only. 0.46.0, 77,543 downloads | Native only (wasm build is a `compile_error!`). Tied to rig's tool registry types; useful if C adopts rig-agent, otherwise a reference for what we must do ourselves | `crates/rig-rmcp/README.md`, `Cargo.toml:19-36` |
| **rig-tools** (rig) | MIT | `read`, `write` (atomic), `edit` (several replacements, all or nothing), `search` (regex, gitignore-aware), `list`, `shell` (`sh -c` in its own process group, timeout, output limit with spill file), `delete`, `move`. Every tool acts on a `Workspace` (`Local`, or `Remote` over ssh). Interrupt stops remote commands too. 0.46.0, only 2,619 downloads | `shell` is `sh -c` and the crate has `cfg(unix)` dependencies: **Windows behaviour not verified** and Windows is our primary platform. Low adoption | `crates/rig-tools/README.md`, `Cargo.toml:36` |
| **rig mcp.json design** (`plugins/rig-mcp-client`) | MIT | Servers from `~/.rig/mcp.json` and `<project>/.rig/mcp.json`; stdio (`command`,`args`) or streamable HTTP (`url`,`headers`); tools offered as `<server>__<tool>`, names sanitised so they never collide; **a cloned repo's project config never starts a program or contacts a server until the user approves it**; no OAuth, no SSE transport | A plugin of rig's own coding agent, not a library we can lean on; the approval design is what is worth taking | `plugins/rig-mcp-client/README.md` |
| **codex-rs** `rmcp-client`, `tools`, `codex-mcp`, `execpolicy` | Apache-2.0 (`f58acd6e`) | A full tool stack with a policy engine | Crates not found on crates.io under the names `codex-rmcp-client` (control: `rmcp` found), i.e. workspace-internal as far as checked. Reference only | `codex-rs/` listing; crates.io API |
| **goose-mcp** (aaif-goose/goose) | Apache-2.0 (`cfb67d72`) | Goose's built-in MCP extensions | Not on crates.io under that name. Reference only; not read | `crates/` listing |
| **MCP specification** (modelcontextprotocol/modelcontextprotocol) | Transition notice as above (`a40e90ca`) | Spec versions on main: `2024-11-05`, `2025-03-26`, `2025-06-18`, `2025-11-25`, `2026-07-28`, plus `draft` | The protocol, not an implementation | `schema/` listing |

### Reading of the survey (recommendation, not ruling)

- **Adopt `rmcp` as the only MCP protocol implementation.** It is the official SDK, the most used by an order of magnitude,
  and its MSRV equals ours. Do not write a protocol client.
- **Write the thin host ourselves (`ToolHost`)** because what we need on top of rmcp is policy, not protocol: namespacing,
  approval of untrusted configs, limits and truncation, the D|E hook. If C adopts rig-agent, `rig-rmcp` already does the
  glue for that registry and should be used instead of re-doing it; decide with C's spike.
- **Native tools:** use `rig-tools` only after a Windows check. If `sh -c` is the shell on Windows we need Git Bash
  semantics anyway (this fleet runs Git Bash). Otherwise write the six basic tools as MCP-shaped native tools; they are
  small and the design (atomic write, all-or-nothing edit, output spill) is documented in `rig-tools`.
- **D2 needs nothing new:** Synapse and Serena are already MCP servers. The Hierarchical-file edit tool (STACK
  "a project for later") would be a D2 provider, not part of this layer.

## 4. Design

### 4.1 Shape

One trait `ToolHost`: `definitions() -> Vec<ToolDef>`; `call(call, ctx) -> ToolOutcome`; and an event stream for
`tools_changed`. Native tools and MCP servers are two backends behind it. `ToolOutcome = { text, structured?, is_error,
meta }`: MCP's `isError` and `structuredContent` map straight through, and a **tool failure is a result the model sees,
not a loop error**; only a host failure (server gone, transport closed) is an `Err`.

### 4.2 Servers and names

- Config: read the `mcpServers` JSON the fleet already writes (`command`, `args`, `env`; add `url`, `headers`). No new
  config format.
- Names offered to the model: `<server>__<tool>`, sanitised to the charset providers accept (rig's rule: no `__` and not
  ending in `_` inside either part), so two servers' tools never collide. The MCP name is kept for the wire.
- `notifications/tools/list_changed` refreshes the table; a tool that disappears mid-run answers as an error result.

### 4.3 The D|E seam

**Every execution passes `Policy::check(&call, &ctx) -> Allow | Deny(reason) | Ask(request)` before it runs.** The host never
runs a call when the policy is absent or errors (fail closed). `Ask` is E's mechanism to reach a person (Consent, layer I).
rig's own loop has the same point as a hook: `DispatchAction` with a `Deny` that becomes a `Cancelled`-kind result
(`crates/rig-agent/src/agent/hook.rs:639-649, 704` at `4908f428`), so adopting rig-agent gives the hook for free and our
`Policy` plugs into it.

### 4.4 Untrusted input

Rules inherited from the fleet (`AGENT-ONBOARDING`, agentlife, synapse README): **tool results and mail text are untrusted
input**, and a **project-level server config is untrusted until approved** (rig's design). So:

1. A server named in a repo-local config does not start until a person approves it; approval is remembered per project
   outside the repo.
2. Tool results are data, never instructions; the host marks them as such in the transcript so C/F can keep that line.
3. Environment passed to a stdio server is an allowlist, not the parent's whole environment (secrets belong to the vault).

### 4.5 Limits and cancellation

Per-call timeout, output size cap with the head and tail kept and the rest spilled to a file (the `rig-tools` `OutputLimit`
idea), and a max message size for stdio (rig's reader ends a connection at 64 MiB). A cancelled run (C's soft/hard stop)
cancels in-flight calls through rmcp's cancellation and closes stdio servers' process groups; an unanswered call is closed
by C (`LOOP-CORE.md` section 4.1).

## 5. Seams

- **C|D:** `call` in, `ToolOutcome` out. C owns the transcript entries and the "untrusted" labelling decision.
- **D|E:** `Policy::check` before every execution (4.3). E's note defines sandboxing of what a tool may touch; this layer
  only guarantees the check happens.
- **D and G:** tool activity is an event (`tool_started`, `tool_finished{is_error}`) that the adapter folds into `activity`
  (`HARNESS-ADAPTER.md` section 5.2).

## 6. Open questions (for the PM, not decided)

1. Does C adopt rig-agent? If yes, D is mostly `rig-rmcp` plus our `Policy`; if goose-agent, `ToolOperation` over `rmcp` tools
   (its README) plays that role. D follows C's spike.
2. `rig-tools` on Windows: needs a one-hour check (does `shell` find a POSIX `sh`; do the `cfg(unix)` dependencies gate
   anything we need?). Not run.
3. Serena's transport and tool list: not read here. Needed before the hello-world's `mcp_http("serena", ...)` is a claim.
4. OAuth for remote MCP servers: out of scope (neither adoption candidate supports it); stated so it is not assumed.

## 7. What I did not verify

- Claude Code internals (closed).
- That `rmcp` 3.8.0 compiles and connects to `synapse-claude-channel` on this machine; nothing was built.
- The rmcp SSE transport's status (feature table only), rmcp's cancellation API (design assumes it exists; not read).
- `rig-tools` on Windows; `rig-rmcp` beyond its README and `Cargo.toml`.
- codex-rs and goose MCP crates beyond their directory listings.
- Serena (see 6.3).
