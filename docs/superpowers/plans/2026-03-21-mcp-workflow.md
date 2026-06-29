# MCP Workflow Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a standalone `squad-mcp` binary plus the MCP transport, daemon client, and workflow engine primitives needed for squad agent coordination.

**Architecture:** The crate exposes focused modules for config parsing, protocol envelopes, MCP request handling, and workflow progression. The MCP server uses a framed stdio JSON-RPC transport and delegates message operations to a daemon client that discovers the workspace root by locating `squad.yaml` and then connects to `.squad/squad.sock`.

**Tech Stack:** Rust, Tokio, Serde, Serde JSON, Serde YAML, Anyhow

---

## Chunk 1: Scaffolding

### Task 1: Create crate layout

**Files:**
- Create: `Cargo.toml`
- Create: `src/lib.rs`
- Create: `src/main.rs`
- Create: `src/bin/squad-mcp.rs`
- Create: `src/config/mod.rs`
- Create: `src/protocol/mod.rs`
- Create: `src/daemon/mod.rs`
- Create: `src/mcp/mod.rs`
- Create: `src/mcp/transport.rs`
- Create: `src/mcp/tools.rs`
- Create: `src/mcp/client.rs`
- Create: `src/workflow/mod.rs`
- Create: `src/workflow/engine.rs`
- Create: `tests/mcp_transport.rs`
- Create: `tests/mcp_tools.rs`
- Create: `tests/workflow_engine.rs`

- [ ] Write failing tests that reference the intended public API.
- [ ] Run the focused tests and confirm they fail for missing modules or behavior.
- [ ] Add the minimal module declarations and placeholder binaries needed for compilation.

## Chunk 2: MCP Surface

### Task 2: Add transport and tools

**Files:**
- Modify: `src/mcp/mod.rs`
- Modify: `src/mcp/transport.rs`
- Modify: `src/mcp/tools.rs`
- Modify: `src/mcp/client.rs`
- Modify: `src/protocol/mod.rs`
- Test: `tests/mcp_transport.rs`
- Test: `tests/mcp_tools.rs`

- [ ] Add a failing test for `initialize` capabilities framing.
- [ ] Run the single test and confirm it fails for missing transport behavior.
- [ ] Implement the minimal framed stdio JSON-RPC transport and request dispatch.
- [ ] Re-run the single test and confirm it passes.
- [ ] Repeat for `tools/list`, `send_message`, `check_inbox`, and `mark_done`.

## Chunk 3: Workflow Engine

### Task 3: Add workflow progression primitives

**Files:**
- Modify: `src/config/mod.rs`
- Modify: `src/workflow/mod.rs`
- Modify: `src/workflow/engine.rs`
- Modify: `src/daemon/mod.rs`
- Test: `tests/workflow_engine.rs`

- [ ] Add a failing test for workflow step advancement after `mark_done`.
- [ ] Run the workflow test and confirm it fails for missing state machine behavior.
- [ ] Implement the minimal workflow config parser, state machine, dispatcher trait, and engine logic.
- [ ] Re-run the workflow test and confirm it passes.

## Chunk 4: Verification

### Task 4: Validate the crate

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/**/*`
- Modify: `tests/**/*`

- [ ] Run `cargo fmt`.
- [ ] Run `cargo test`.
- [ ] Run `cargo build --bin squad-mcp`.
