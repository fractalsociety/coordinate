# Session Token Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Detect and notify agents when their identity is taken over by another terminal joining with the same ID.

**Architecture:** Add `session_token` column to agents table. On join, generate UUID and write to `.squad/sessions/<id>` file. On send/receive, read token from file and validate against DB. Mismatch = displacement notification.

**Tech Stack:** Rust, rusqlite, anyhow, uuid (new dependency)

**Spec:** `docs/superpowers/specs/2026-03-22-session-token-design.md`

---

## File Structure

| File | Responsibility |
|------|---------------|
| `src/store.rs` | DB schema migration, `register_agent` returns token, `get_session_token` query |
| `src/session.rs` (new) | Session file read/write/delete/validate helpers |
| `src/main.rs` | Call session write on join, validate on send/receive, delete on leave, cleanup on clean |
| `src/setup.rs` | Add "session replaced" instruction to slash command templates |
| `src/init.rs` | Create `.squad/sessions/` directory on init |
| `Cargo.toml` | Add `uuid` dependency |
| `tests/session_test.rs` (new) | Unit tests for session module |
| `tests/cli_test.rs` | CLI integration tests for displacement behavior |
| `tests/store_test.rs` | Store-level token tests |

---

### Task 1: Add uuid dependency and session_token column

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/store.rs:28-50` (schema), `src/store.rs:53-60` (register_agent)
- Test: `tests/store_test.rs`

- [ ] **Step 1: Write failing test for register_agent returning a token**

Add to `tests/store_test.rs`:

```rust
#[test]
fn test_register_agent_returns_session_token() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    let token = store.register_agent("worker", "worker").unwrap();
    assert!(!token.is_empty());
    assert_eq!(token.len(), 36); // UUID v4 format: 8-4-4-4-12
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test test_register_agent_returns_session_token -- --nocapture`
Expected: FAIL — `register_agent` returns `()`, not `String`.

- [ ] **Step 3: Add uuid dependency to Cargo.toml**

Add under `[dependencies]`:

```toml
uuid = { version = "1", features = ["v4"] }
```

- [ ] **Step 4: Add session_token column to schema and update register_agent**

In `src/store.rs`, modify the CREATE TABLE for agents:

```rust
conn.execute_batch(
    "CREATE TABLE IF NOT EXISTS agents (
        id TEXT PRIMARY KEY,
        role TEXT NOT NULL,
        joined_at INTEGER NOT NULL,
        session_token TEXT
    );
    CREATE TABLE IF NOT EXISTS messages (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        from_agent TEXT NOT NULL,
        to_agent TEXT NOT NULL,
        content TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        read INTEGER NOT NULL DEFAULT 0
    );",
)?;
```

Note: For existing databases, SQLite silently ignores the column if the table already exists. To handle migration, add after the CREATE TABLE block:

```rust
// Migration: add session_token column if missing (existing DBs)
let _ = conn.execute_batch(
    "ALTER TABLE agents ADD COLUMN session_token TEXT;"
);
```

The `ALTER TABLE` will error if column already exists — the `let _ =` discards that error. This is safe because `CREATE TABLE IF NOT EXISTS` won't re-create the table.

Modify `register_agent` to generate and store a token:

```rust
pub fn register_agent(&self, id: &str, role: &str) -> Result<String> {
    let now = chrono::Utc::now().timestamp();
    let token = uuid::Uuid::new_v4().to_string();
    self.conn.execute(
        "INSERT OR REPLACE INTO agents (id, role, joined_at, session_token) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![id, role, now, token],
    )?;
    Ok(token)
}
```

- [ ] **Step 5: Add get_session_token query**

Add to `src/store.rs`:

```rust
use rusqlite::OptionalExtension;

pub fn get_session_token(&self, id: &str) -> Result<Option<String>> {
    let token: Option<String> = self.conn.query_row(
        "SELECT session_token FROM agents WHERE id = ?1",
        [id],
        |row| row.get(0),
    ).optional()?;
    Ok(token)
}
```

Note: `.optional()` from `rusqlite::OptionalExtension` correctly distinguishes "no rows found" (returns `Ok(None)`) from real DB errors (propagates the error). Do NOT use `.ok()` which swallows all errors.

- [ ] **Step 6: Fix all callers of register_agent**

In `src/main.rs` `cmd_join` (line 145), `register_agent` now returns `String`. Change:

```rust
// Before:
store.register_agent(id, role)?;

// After:
let token = store.register_agent(id, role)?;
```

The `token` variable will be used in Task 3. For now, just capture it.

- [ ] **Step 7: Run tests**

Run: `cargo test`
Expected: all 49 existing tests pass + 1 new test passes.

Note: existing tests that call `register_agent` will need their return values captured or ignored. Since they use `unwrap()`, the `String` return is silently consumed. No changes needed.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml src/store.rs tests/store_test.rs
git commit -m "feat: register_agent generates session token (stored in DB)"
```

---

### Task 2: Create session file module

**Files:**
- Create: `src/session.rs`
- Modify: `src/lib.rs`
- Modify: `src/init.rs` (create sessions dir)
- Test: `tests/session_test.rs` (new)

- [ ] **Step 1: Write failing test for session file write/read**

Create `tests/session_test.rs`:

```rust
use tempfile::TempDir;

#[test]
fn test_write_and_read_session() {
    let tmp = TempDir::new().unwrap();
    let sessions_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    squad::session::write_token(&sessions_dir, "worker", "abc-123").unwrap();
    let token = squad::session::read_token(&sessions_dir, "worker").unwrap();
    assert_eq!(token, Some("abc-123".to_string()));
}

#[test]
fn test_read_nonexistent_session() {
    let tmp = TempDir::new().unwrap();
    let sessions_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    let token = squad::session::read_token(&sessions_dir, "nobody").unwrap();
    assert_eq!(token, None);
}

#[test]
fn test_delete_session() {
    let tmp = TempDir::new().unwrap();
    let sessions_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    squad::session::write_token(&sessions_dir, "worker", "abc-123").unwrap();
    squad::session::delete_token(&sessions_dir, "worker").unwrap();
    let token = squad::session::read_token(&sessions_dir, "worker").unwrap();
    assert_eq!(token, None);
}

#[test]
fn test_validate_token_match() {
    let tmp = TempDir::new().unwrap();
    let sessions_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    squad::session::write_token(&sessions_dir, "worker", "abc-123").unwrap();
    let result = squad::session::validate(&sessions_dir, "worker", "abc-123");
    assert!(result.is_ok());
}

#[test]
fn test_validate_token_mismatch() {
    let tmp = TempDir::new().unwrap();
    let sessions_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    squad::session::write_token(&sessions_dir, "worker", "old-token").unwrap();
    let result = squad::session::validate(&sessions_dir, "worker", "new-token");
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Session replaced"));
}

#[test]
fn test_validate_no_file_is_ok() {
    let tmp = TempDir::new().unwrap();
    let sessions_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    // No session file = backward compat, should pass
    let result = squad::session::validate(&sessions_dir, "worker", "any-token");
    assert!(result.is_ok());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test session_test`
Expected: FAIL — module `squad::session` does not exist.

- [ ] **Step 3: Create src/session.rs**

```rust
use anyhow::{bail, Context, Result};
use std::path::Path;

/// Write a session token file for an agent.
pub fn write_token(sessions_dir: &Path, agent_id: &str, token: &str) -> Result<()> {
    std::fs::create_dir_all(sessions_dir)
        .with_context(|| format!("failed to create {}", sessions_dir.display()))?;
    let path = sessions_dir.join(agent_id);
    std::fs::write(&path, token)
        .with_context(|| format!("failed to write session file: {}", path.display()))?;
    Ok(())
}

/// Read a session token file. Returns None if file does not exist.
pub fn read_token(sessions_dir: &Path, agent_id: &str) -> Result<Option<String>> {
    let path = sessions_dir.join(agent_id);
    if !path.exists() {
        return Ok(None);
    }
    let token = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read session file: {}", path.display()))?;
    Ok(Some(token))
}

/// Delete a session token file.
pub fn delete_token(sessions_dir: &Path, agent_id: &str) -> Result<()> {
    let path = sessions_dir.join(agent_id);
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

/// Delete all session token files.
pub fn delete_all(sessions_dir: &Path) -> Result<()> {
    if sessions_dir.exists() {
        for entry in std::fs::read_dir(sessions_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                std::fs::remove_file(entry.path())?;
            }
        }
    }
    Ok(())
}

/// Validate that the local token matches the expected token (from DB).
/// Returns Ok(()) if they match or if no local session file exists (backward compat).
/// Errors with "Session replaced" if the local token differs from expected.
pub fn validate(sessions_dir: &Path, agent_id: &str, expected_token: &str) -> Result<()> {
    let current = read_token(sessions_dir, agent_id)?;
    match current {
        None => Ok(()), // No session file = agent joined before this feature, skip
        Some(token) if token == expected_token => Ok(()),
        Some(_) => bail!(
            "Session replaced. Another terminal joined as {agent_id}. \
             Re-join with a different ID (e.g. squad join {agent_id}-2 --role <your-role>)."
        ),
    }
}
```

- [ ] **Step 4: Register module in lib.rs**

Add to `src/lib.rs`:

```rust
pub mod session;
```

- [ ] **Step 5: Add sessions dir creation to init.rs**

In `src/init.rs` `init_workspace`, after creating `teams_dir`, add:

```rust
let sessions_dir = squad_dir.join("sessions");
std::fs::create_dir_all(&sessions_dir)
    .with_context(|| format!("failed to create {}", sessions_dir.display()))?;
```

- [ ] **Step 6: Run tests**

Run: `cargo test`
Expected: all pass including 5 new session tests.

- [ ] **Step 7: Commit**

```bash
git add src/session.rs src/lib.rs src/init.rs tests/session_test.rs
git commit -m "feat: add session token file module for agent identity tracking"
```

---

### Task 3: Wire session tokens into join, leave, clean

**Files:**
- Modify: `src/main.rs` (cmd_join, cmd_leave, cmd_clean)
- Test: `tests/cli_test.rs`

- [ ] **Step 1: Add sessions_dir helper to main.rs**

Add after `open_store`:

```rust
fn sessions_dir(workspace: &Path) -> PathBuf {
    workspace.join(".squad").join("sessions")
}
```

- [ ] **Step 2: Write session token in cmd_join**

In `cmd_join`, after `let token = store.register_agent(id, role)?;`, add:

```rust
squad::session::write_token(&sessions_dir(&workspace), id, &token)?;
```

- [ ] **Step 3: Delete session token in cmd_leave**

In `cmd_leave`, after `store.unregister_agent(id)?;`, add:

```rust
squad::session::delete_token(&sessions_dir(&workspace), id)?;
```

- [ ] **Step 4: Delete all session tokens in cmd_clean**

In `cmd_clean`, before the "Cleaned" println, add:

```rust
squad::session::delete_all(&workspace.join(".squad").join("sessions"))?;
```

- [ ] **Step 5: Write CLI test for join creating session file**

Add to `tests/cli_test.rs`:

```rust
#[test]
fn test_join_creates_session_file() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "worker", "--role", "worker"])
        .assert()
        .success();
    let session_path = tmp.path().join(".squad").join("sessions").join("worker");
    assert!(session_path.exists());
    let token = std::fs::read_to_string(&session_path).unwrap();
    assert_eq!(token.len(), 36); // UUID v4
}
```

- [ ] **Step 6: Write CLI test for leave deleting session file**

Add to `tests/cli_test.rs`:

```rust
#[test]
fn test_leave_deletes_session_file() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path()).args(["join", "worker"]).assert().success();
    squad(tmp.path()).args(["leave", "worker"]).assert().success();
    let session_path = tmp.path().join(".squad").join("sessions").join("worker");
    assert!(!session_path.exists());
}
```

- [ ] **Step 7: Run tests**

Run: `cargo test`
Expected: all pass.

- [ ] **Step 8: Commit**

```bash
git add src/main.rs tests/cli_test.rs
git commit -m "feat: write/delete session tokens on join/leave/clean"
```

---

### Task 4: Add session validation to send and receive

**Files:**
- Modify: `src/main.rs` (cmd_send, cmd_receive)
- Test: `tests/cli_test.rs`, `tests/e2e_test.rs`

This is the core feature — detecting displacement.

- [ ] **Step 1: Write the key integration test (displacement detection)**

Add to `tests/e2e_test.rs`:

```rust
#[test]
fn test_second_join_displaces_first() {
    let tmp = setup_workspace();

    // First terminal joins as worker
    squad(tmp.path())
        .args(["join", "worker", "--role", "worker"])
        .assert()
        .success();

    // Save first terminal's session token
    let token_path = tmp.path().join(".squad").join("sessions").join("worker");
    let first_token = std::fs::read_to_string(&token_path).unwrap();

    // Second terminal joins as worker (overwrites)
    squad(tmp.path())
        .args(["join", "worker", "--role", "worker"])
        .assert()
        .success();

    // Token file should have changed
    let second_token = std::fs::read_to_string(&token_path).unwrap();
    assert_ne!(first_token, second_token);

    // Simulate first terminal: restore its old token file.
    // In real usage, Terminal 1's file stays unchanged — it's the DB that gets
    // overwritten by Terminal 2's join. But since both terminals share the same
    // filesystem path in this test, Terminal 2's join also overwrites the file.
    // We restore it manually to simulate Terminal 1's perspective.
    std::fs::write(&token_path, &first_token).unwrap();
    squad(tmp.path())
        .args(["send", "worker", "worker", "hello"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Session replaced"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test test_second_join_displaces_first -- --nocapture`
Expected: FAIL — send doesn't validate session tokens yet.

- [ ] **Step 3: Add check_session helper to main.rs**

Add a helper that uses `session::validate` with the DB token, avoiding code duplication:

```rust
/// Check if this agent's session is still valid. Returns Ok(()) if valid or if
/// no session tracking exists (backward compat). Errors with "Session replaced" if displaced.
fn check_session(workspace: &Path, store: &squad::store::Store, agent_id: &str) -> Result<()> {
    let sessions = sessions_dir(workspace);
    if let Some(db_token) = store.get_session_token(agent_id)? {
        squad::session::validate(&sessions, agent_id, &db_token)?;
    }
    Ok(())
}
```

Note: Backward compatibility is handled at two levels: if the agent has no DB token (`get_session_token` returns `None`), the check is skipped entirely. If the agent has a DB token but no local session file (joined before this feature was deployed), `validate` returns `Ok(())`.

- [ ] **Step 4: Add session validation to cmd_send**

In `cmd_send`, after `let store = open_store(&workspace)?;`, add one line:

```rust
fn cmd_send(from: &str, to: &str, content: &str) -> Result<()> {
    let workspace = find_workspace()?;
    let store = open_store(&workspace)?;
    check_session(&workspace, &store, from)?;

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

- [ ] **Step 5: Add session validation to cmd_receive**

In `cmd_receive`, validate once at entry (covers non-wait path), and also in the --wait loop (detects displacement during polling):

```rust
fn cmd_receive(agent: &str, wait: bool, timeout_secs: u64) -> Result<()> {
    let workspace = find_workspace()?;

    // Validate session at entry (catches displacement immediately)
    let store = open_store(&workspace)?;
    check_session(&workspace, &store, agent)?;

    if wait {
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
        loop {
            let store = open_store(&workspace)?;

            // Re-check for displacement on each poll (~500ms)
            check_session(&workspace, &store, agent)?;

            if store.has_unread_messages(agent)? {
                let messages = store.receive_messages(agent)?;
                if !messages.is_empty() {
                    print_messages(&messages, Some(agent));
                    return Ok(());
                }
            }
            if std::time::Instant::now() > deadline {
                println!("No new messages (timed out after {timeout_secs}s).");
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    } else {
        let messages = store.receive_messages(agent)?;
        if messages.is_empty() {
            println!("No new messages.");
        } else {
            print_messages(&messages, Some(agent));
        }
        Ok(())
    }
}
```

- [ ] **Step 6: Run tests**

Run: `cargo test`
Expected: all pass including the new displacement test.

- [ ] **Step 7: Write test for receive displacement**

Add to `tests/e2e_test.rs`:

```rust
#[test]
fn test_receive_detects_displacement() {
    let tmp = setup_workspace();

    // Join, save token, then overwrite by re-joining
    squad(tmp.path()).args(["join", "worker"]).assert().success();
    let token_path = tmp.path().join(".squad").join("sessions").join("worker");
    let first_token = std::fs::read_to_string(&token_path).unwrap();

    squad(tmp.path()).args(["join", "worker"]).assert().success();

    // Restore first token to simulate first terminal's perspective.
    // (See comment in test_second_join_displaces_first for explanation.)
    std::fs::write(&token_path, &first_token).unwrap();

    // Receive should detect displacement
    squad(tmp.path())
        .args(["receive", "worker"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Session replaced"));
}
```

- [ ] **Step 8: Run tests**

Run: `cargo test`
Expected: all pass.

- [ ] **Step 9: Commit**

```bash
git add src/main.rs tests/cli_test.rs tests/e2e_test.rs
git commit -m "feat: detect and report session displacement on send/receive"
```

---

### Task 5: Update slash command templates

**Files:**
- Modify: `src/setup.rs` (SQUAD_MD_CONTENT, SQUAD_TOML_CONTENT)
- Test: `tests/setup_test.rs`

- [ ] **Step 1: Add displacement recovery instruction to both templates**

In `src/setup.rs`, in both `SQUAD_MD_CONTENT` and `SQUAD_TOML_CONTENT`, add after the "IMPORTANT" retry step:

```
8. **SESSION CONFLICT:** If any squad command returns "Session replaced", it means another terminal took your ID. Re-join with a suffixed ID (e.g. `squad join worker-2 --role worker`) and continue.
```

Note: Update step numbering in both templates accordingly (current step 7 becomes step 7, new step is 8).

- [ ] **Step 2: Write test for new instruction in templates**

Add to `tests/setup_test.rs`:

```rust
#[test]
fn test_md_content_has_session_conflict_instruction() {
    assert!(squad::setup::SQUAD_MD_CONTENT.contains("Session replaced"));
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test`
Expected: all pass.

- [ ] **Step 4: Commit**

```bash
git add src/setup.rs tests/setup_test.rs
git commit -m "feat: add session conflict recovery instruction to slash commands"
```

---

### Task 6: Reinstall and final verification

**Files:** None (runtime verification)

- [ ] **Step 1: Run full test suite**

Run: `cargo test`
Expected: all tests pass (49 existing + ~10 new).

- [ ] **Step 2: Run clippy**

Run: `cargo clippy -- -D warnings`
Expected: no warnings.

- [ ] **Step 3: Reinstall binary and slash commands**

```bash
cargo install --path .
squad setup
```

- [ ] **Step 4: Manual smoke test**

```bash
squad clean
squad init
squad join worker --role worker
# Check session file exists:
cat .squad/sessions/worker
# Re-join (simulates second terminal):
squad join worker --role worker
# Previous session file is now different
```

- [ ] **Step 5: Bump version to 0.3.0**

In `Cargo.toml`, change `version = "0.2.0"` to `version = "0.3.0"`.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml
git commit -m "chore: bump version to 0.3.0"
```

---

## Summary

| Task | Files | Tests Added |
|------|-------|-------------|
| 1. DB schema + register_agent | store.rs, Cargo.toml, store_test.rs | 1 |
| 2. Session file module | session.rs, lib.rs, init.rs, session_test.rs | 6 |
| 3. Wire into join/leave/clean | main.rs, cli_test.rs | 2 |
| 4. Validate on send/receive | main.rs, e2e_test.rs | 2 |
| 5. Slash command update | setup.rs, setup_test.rs | 1 |
| 6. Final verification | Cargo.toml | 0 |
| **Total** | **9 files** | **12 new tests** |

After all tasks: 49 existing + 12 new = **61 tests**.
