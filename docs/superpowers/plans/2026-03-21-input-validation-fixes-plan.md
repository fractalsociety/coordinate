# Input Validation & Bug Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix all P0 input validation bugs and P1 data/doc issues from the project audit.

**Architecture:** Minimal changes to main.rs, store.rs, docs, and Cargo.toml.

**Tech Stack:** Rust, rusqlite, anyhow

**Spec:** `docs/superpowers/specs/2026-03-21-input-validation-fixes-design.md`

---

## Important: Test Patterns

Before writing tests, follow the existing patterns in the codebase:

**`tests/cli_test.rs` pattern:**
```rust
use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn squad(workspace: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("squad").unwrap();
    cmd.current_dir(workspace);
    cmd
}

// Tests use: TempDir::new() + squad(tmp.path()) + predicate::str::contains (no 's')
```

**`tests/e2e_test.rs` pattern:**
```rust
fn setup_workspace() -> TempDir {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    tmp
}
// setup_workspace() only exists in e2e_test.rs, NOT in cli_test.rs
```

**`tests/store_test.rs` pattern:**
```rust
use squad::store::Store;
use tempfile::TempDir;

// Each test creates its own store inline:
let tmp = TempDir::new().unwrap();
let store = Store::open(&tmp.path().join("messages.db")).unwrap();
// There is NO create_store() helper function
```

**All CLI tests that need a workspace must call `squad(tmp.path()).arg("init")` first.**

---

### Task 1: Add agent ID validation

**Files:**
- Modify: `src/main.rs` (add `validate_agent_id` function, call in join/leave/send/receive)
- Test: `tests/cli_test.rs`

- [ ] **Step 1: Add `validate_agent_id` function to main.rs**

Add after the `// --- Helpers ---` comment, before `find_workspace`:

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

- [ ] **Step 2: Add validation calls to all command entry points**

In `cmd_join`: add `validate_agent_id(id)?;` as **first line** (before `find_workspace`).

In `cmd_leave`: add `validate_agent_id(id)?;` as **first line** (before `find_workspace`).

In `cmd_send`: add `validate_agent_id(from)?;` as **first line**. For `to`, validate only if `to != "@all"`:
```rust
if to != "@all" {
    validate_agent_id(to)?;
}
```

In `cmd_receive`: add `validate_agent_id(agent)?;` as **first line** (before `find_workspace`).

Note: validation runs before `find_workspace()`, so invalid IDs are caught even without a workspace. But tests should still `init` first so the error message assertion is unambiguous.

- [ ] **Step 3: Write tests for ID validation**

Add to `tests/cli_test.rs`:

```rust
#[test]
fn test_join_rejects_at_all() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "@all"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("reserved for broadcast"));
}

#[test]
fn test_join_rejects_flag_like_id() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "--help"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot start with '-'"));
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test`
Expected: all existing 49 tests pass + 2 new tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs tests/cli_test.rs
git commit -m "fix: reject invalid agent IDs (@all, --flags, empty)"
```

---

### Task 2: Add sender verification to send

**Files:**
- Modify: `src/main.rs` (`cmd_send` — add sender existence check before branch)
- Modify: `src/store.rs` (`send_message_checked` — add sender check)
- Test: `tests/cli_test.rs`, `tests/store_test.rs`

Design note: `cmd_send` checks sender existence once at the top, before the `@all` / direct branch. This covers both paths. `send_message_checked` also gets a sender check as a safety net for any future callers that bypass `cmd_send`.

- [ ] **Step 1: Modify `cmd_send` in main.rs to check sender exists**

Replace the current `cmd_send` body with:

```rust
fn cmd_send(from: &str, to: &str, content: &str) -> Result<()> {
    validate_agent_id(from)?;
    if to != "@all" {
        validate_agent_id(to)?;
    }
    let workspace = find_workspace()?;
    let store = open_store(&workspace)?;
    if !store.agent_exists(from)? {
        bail!("{from} is not registered. Run 'squad join {from}' first.");
    }
    if to == "@all" {
        let recipients = store.broadcast_message(from, content)?;
        println!(
            "Broadcast to {} agents: {}",
            recipients.len(),
            recipients.join(", ")
        );
    } else {
        store.send_message_checked(from, to, content)?;
        println!("Sent to {to}.");
    }
    Ok(())
}
```

- [ ] **Step 2: Add sender check in `send_message_checked` in store.rs**

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

- [ ] **Step 3: Write tests**

Add to `tests/cli_test.rs`:

```rust
#[test]
fn test_send_from_unregistered_fails() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "worker", "--role", "worker"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["send", "ghost", "worker", "hello"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not registered"));
}
```

Add to `tests/store_test.rs`:

```rust
#[test]
fn test_send_from_unregistered_agent_fails() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    store.register_agent("worker", "worker").unwrap();
    let result = store.send_message_checked("ghost", "worker", "hi");
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not registered"));
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test`
Expected: all pass. The existing `test_send_to_nonexistent` tests still pass because they register the sender first.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs src/store.rs tests/cli_test.rs tests/store_test.rs
git commit -m "fix: verify sender is registered before sending messages"
```

---

### Task 3: Add receive agent existence check

**Files:**
- Modify: `src/main.rs` (`cmd_receive`)
- Test: `tests/cli_test.rs`

- [ ] **Step 1: Add existence check in `cmd_receive`**

After `validate_agent_id(agent)?;` and `let store = open_store(&workspace)?;` (the first one, before the `if wait` branch), add:

```rust
if !store.agent_exists(agent)? {
    bail!("{agent} is not registered. Run 'squad join {agent}' first.");
}
```

- [ ] **Step 2: Write test**

Add to `tests/cli_test.rs`:

```rust
#[test]
fn test_receive_unknown_agent_fails() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["receive", "nobody"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not registered"));
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test`
Expected: all pass.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs tests/cli_test.rs
git commit -m "fix: error immediately when receiving for unregistered agent"
```

---

### Task 4: Fix `--timeout` parse error

**Files:**
- Modify: `src/main.rs` (`cmd_receive` argument parsing, line 65)
- Test: `tests/cli_test.rs`

- [ ] **Step 1: Replace `unwrap_or(120)` with proper error**

In main.rs, in the `cmd_receive` match arm for `"--timeout"`, change:

```rust
// Before:
timeout_secs = val.parse().unwrap_or(120);

// After:
timeout_secs = val.parse().with_context(|| {
    format!("invalid timeout value: '{val}'. Expected a number in seconds.")
})?;
```

Note: the `with_context` closure captures `val` which is `&String` from `extra.get(i + 1)`. This is fine since `extra` is still in scope.

- [ ] **Step 2: Write test**

Add to `tests/cli_test.rs`:

```rust
#[test]
fn test_receive_invalid_timeout_fails() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "a", "--role", "worker"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["receive", "a", "--timeout", "abc"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid timeout value"));
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test`
Expected: all pass.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs tests/cli_test.rs
git commit -m "fix: report error on invalid --timeout value instead of silent fallback"
```

---

### Task 5: Add database index

**Files:**
- Modify: `src/store.rs` (`Store::open`)

- [ ] **Step 1: Add index creation after table creation**

In `store.rs` `Store::open`, add to the existing CREATE TABLE `execute_batch` (or as a separate call after it):

```sql
CREATE INDEX IF NOT EXISTS idx_messages_unread ON messages(to_agent, read);
```

- [ ] **Step 2: Run tests**

Run: `cargo test`
Expected: all pass (index is additive, doesn't break anything).

- [ ] **Step 3: Commit**

```bash
git add src/store.rs
git commit -m "perf: add index on messages(to_agent, read) for --wait polling"
```

---

### Task 6: Fix reply hint and remove serde_json

**Files:**
- Modify: `src/main.rs` (`print_messages`, line 128)
- Modify: `Cargo.toml` (line 13)

- [ ] **Step 1: Fix reply hint placeholder**

In `print_messages` (main.rs line 128), change:

```rust
// Before:
println!("  → Reply: squad send {id} {} \"<your response>\"", msg.from_agent);

// After:
println!("  → Reply: squad send {id} {} \"...\"", msg.from_agent);
```

- [ ] **Step 2: Remove serde_json from Cargo.toml**

Delete the line `serde_json = "1"` from `[dependencies]` (line 13).

- [ ] **Step 3: Run tests**

Run: `cargo test`
Expected: all pass.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs Cargo.toml
git commit -m "fix: safer reply hint placeholder, remove unused serde_json dependency"
```

---

### Task 7: Fix README timeout values

**Files:**
- Modify: `README.md` (line 44)
- Modify: `README.zh-CN.md` (line 64)

- [ ] **Step 1: Fix English README**

In `README.md` line 44, change:
```
`squad receive <id> [--wait] [--timeout N]` | Check inbox (`--wait` blocks until message, default 120s)
```
to:
```
`squad receive <id> [--wait] [--timeout N]` | Check inbox (`--wait` blocks until message, default 3600s)
```

- [ ] **Step 2: Fix Chinese README**

In `README.zh-CN.md` line 64, change:
```
`squad receive <id> [--wait] [--timeout N]` | 检查收件箱（`--wait` 阻塞等待，默认 120 秒）
```
to:
```
`squad receive <id> [--wait] [--timeout N]` | 检查收件箱（`--wait` 阻塞等待，默认 3600 秒）
```

- [ ] **Step 3: Commit**

```bash
git add README.md README.zh-CN.md
git commit -m "docs: fix --wait default timeout value (3600s, not 120s)"
```

---

## Summary

| Task | Files | Tests Added |
|------|-------|-------------|
| 1. ID validation | main.rs, cli_test.rs | 2 |
| 2. Sender verification | main.rs, store.rs, cli_test.rs, store_test.rs | 2 |
| 3. Receive existence check | main.rs, cli_test.rs | 1 |
| 4. --timeout error | main.rs, cli_test.rs | 1 |
| 5. DB index | store.rs | 0 |
| 6. Reply hint + serde_json | main.rs, Cargo.toml | 0 |
| 7. README fix | README.md, README.zh-CN.md | 0 |
| **Total** | **7 files** | **6 new tests** |

After all tasks: 49 existing + 6 new = **55 tests**.
