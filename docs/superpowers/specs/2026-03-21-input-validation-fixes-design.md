# Squad Input Validation & Bug Fixes

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix all P0 input validation bugs and P1 data/doc issues identified in the project audit.

**Architecture:** Minimal changes to existing code. Add a validation layer at CLI entry points (main.rs), strengthen store-level checks (store.rs), add DB index, fix docs.

**Tech Stack:** Rust, rusqlite, anyhow

---

## Context

A comprehensive project audit identified 26 issues. This spec covers the 8 highest-priority items that directly affect correctness and first-use experience.

## Issues Addressed

### P0 — Must Fix

| # | Issue | Root Cause |
|---|-------|-----------|
| 1 | README says `--wait` default is 120s | Doc out of sync with code (actual: 3600s) |
| 2 | `--timeout abc` silently falls back to 120 | `unwrap_or(120)` — no error, wrong fallback value |
| 3 | `squad send ghost worker "hi"` succeeds | `send_message_checked` only validates `to`, not `from` |
| 4 | `squad receive wrker --wait` hangs forever | No check that agent ID exists before entering wait loop |
| 5 | Can register `@all` as agent ID | No ID validation in `register_agent` |
| 6 | `squad join --help` registers `--help` as agent | Same — no ID validation |

### P1 — Should Fix

| # | Issue | Root Cause |
|---|-------|-----------|
| 13 | `--wait` does full table scan every 500ms | No index on `(to_agent, read)` |
| 20 | Reply hint `<your response>` gets copied by AI | Placeholder too realistic |

### Cleanup

| Item | Issue |
|------|-------|
| serde_json | Listed in Cargo.toml but never used in src/ |

## Design

### 1. Agent ID Validation

Add `validate_agent_id(id: &str) -> Result<()>` in `main.rs`:

```rust
fn validate_agent_id(id: &str) -> Result<()> {
    if id.is_empty() {
        bail!("Agent ID cannot be empty.");
    }
    if id == "@all" {
        bail!("'@all' is reserved for broadcast. Choose a different ID.");
    }
    if id.starts_with('-') {
        bail!("Agent ID cannot start with '-' (looks like a flag). Choose a different ID.");
    }
    Ok(())
}
```

Call sites — every command that takes an agent ID as input:
- `cmd_join` — validate `id` before `register_agent`
- `cmd_leave` — validate `id`
- `cmd_send` — validate `from` and `to` (except `@all` for `to`)
- `cmd_receive` — validate `id`

This catches issues #5 and #6. Existing `send_message_checked` stays as-is (it validates recipient exists in DB); the new validation is a pre-check on the string format.

### 2. Sender Verification

Modify `send_message_checked` in `store.rs` to also verify the sender is registered:

```rust
pub fn send_message_checked(&self, from: &str, to: &str, content: &str) -> Result<()> {
    if !self.agent_exists(from)? {
        anyhow::bail!("{from} is not registered. Run 'squad join {from}' first.");
    }
    if !self.agent_exists(to)? {
        let names = self.agent_names()?;
        anyhow::bail!(
            "{to} does not exist. Online agents: {}",
            names.join(", ")
        );
    }
    self.send_message(from, to, content)
}
```

Broadcast path (`cmd_send` when `to == "@all"`) should also check sender exists. Add the check in `cmd_send` before calling `broadcast_message`.

### 3. Receive Agent Existence Check

In `cmd_receive` (main.rs), before entering the wait loop or calling `receive_messages`, verify the agent exists:

```rust
fn cmd_receive(agent: &str, wait: bool, timeout_secs: u64) -> Result<()> {
    let workspace = find_workspace()?;
    let store = open_store(&workspace)?;

    if !store.agent_exists(agent)? {
        bail!("{agent} is not registered. Run 'squad join {agent}' first.");
    }

    // ... rest unchanged
}
```

This catches issue #4 — typo in ID gets immediate feedback instead of infinite hang.

### 4. `--timeout` Parse Error

Replace silent fallback with explicit error in `cmd_receive` argument parsing (main.rs):

```rust
"--timeout" => {
    if let Some(val) = extra.get(i + 1) {
        timeout_secs = val.parse().with_context(|| {
            format!("invalid timeout value: '{val}'. Expected a number in seconds.")
        })?;
    }
    i += 2;
}
```

This catches issue #2. No more `unwrap_or(120)`.

### 5. Database Index

Add index creation in `Store::open` (store.rs), after table creation:

```sql
CREATE INDEX IF NOT EXISTS idx_messages_unread
ON messages(to_agent, read);
```

This optimizes the `has_unread_messages` query that runs every 500ms during `--wait`.

### 6. Reply Hint

In `print_messages` (main.rs), change:

```
→ Reply: squad send {id} {from} "<your response>"
```

to:

```
→ Reply: squad send {id} {from} "..."
```

Reduces risk of AI agents literally copying `<your response>` into messages.

### 7. README Fix

Both `README.md` (line 44) and `README.zh-CN.md`: change `default 120s` to `default 3600s` in the `--wait` description.

### 8. Remove serde_json

Delete `serde_json = "1"` from `[dependencies]` in `Cargo.toml`. No code references it.

## Files Modified

| File | Changes |
|------|---------|
| `src/main.rs` | Add `validate_agent_id`, call it in join/leave/send/receive; fix `--timeout` parsing; fix reply hint; add sender existence check in broadcast path |
| `src/store.rs` | Add index creation; add sender check in `send_message_checked` |
| `README.md` | Fix timeout default value |
| `README.zh-CN.md` | Fix timeout default value |
| `Cargo.toml` | Remove `serde_json` |

## Testing Strategy

New tests to add:

1. **ID validation** — `@all`, `--help`, empty string all rejected by join
2. **Sender verification** — unregistered sender gets error from send
3. **Receive unknown agent** — error message, not hang
4. **--timeout invalid value** — error message, not silent fallback

Existing 49 tests must continue to pass.

## Out of Scope

These items from the audit are intentionally deferred:

- #7 `squad clean --yes` confirmation — acceptable for CLI tools
- #8 clean while agent is --wait — edge case, low impact
- #9 stale agent cleanup — separate feature (`--refresh`)
- #10 --wait vs AI tool timeout — by design
- #11 manager.md hardcodes "manager" — correct behavior
- #12 init modifies CLAUDE.md — correct behavior, doc improvement only
- #14-19 UX improvements — v0.3 scope
- #21-26 documentation gaps — separate doc pass
