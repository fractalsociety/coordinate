# Non-blocking Receive Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the double-consumer race condition by removing `--wait` from all AI agent instructions and hardening the receive transaction.

**Architecture:** Template-only fix. No new CLI flags. AI agents already have `squad receive <id>` (check once, return immediately) which is naturally AI-safe. The root cause was the slash command/role templates instructing agents to use `--wait`.

**Tech Stack:** Rust, rusqlite

**Spec:** `docs/superpowers/specs/2026-03-22-poll-receive-design.md`

---

## Important: Test Patterns

**`tests/cli_test.rs`:** `TempDir::new()` + `squad(tmp.path())` + `predicate::str::contains` (no 's')

**`tests/e2e_test.rs`:** `setup_workspace()` helper + `squad(tmp.path())`

**`tests/store_test.rs`:** `TempDir::new()` + `Store::open(&tmp.path().join("messages.db"))`

---

### Task 1: Harden receive_messages with id fence

**Files:**
- Modify: `src/store.rs` (`receive_messages`, line 147)

The current `UPDATE` uses `WHERE to_agent = ?1 AND read = 0`, which could mark messages that arrived AFTER the SELECT as read (phantom read). Add an `id <= max_id` fence to only mark the messages we actually returned.

- [ ] **Step 1: Fix the UPDATE in receive_messages**

In `src/store.rs`, replace the current `receive_messages` (lines 147-173):

```rust
/// Atomically read and mark messages as read using a transaction.
pub fn receive_messages(&self, agent_id: &str) -> Result<Vec<MessageRecord>> {
    let tx = self.conn.unchecked_transaction()?;
    let mut stmt = tx.prepare(
        "SELECT id, from_agent, to_agent, content, created_at, read
         FROM messages WHERE to_agent = ?1 AND read = 0 ORDER BY created_at",
    )?;
    let messages: Vec<MessageRecord> = stmt
        .query_map([agent_id], |row| {
            Ok(MessageRecord {
                id: row.get(0)?,
                from_agent: row.get(1)?,
                to_agent: row.get(2)?,
                content: row.get(3)?,
                created_at: row.get(4)?,
                read: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);

    if !messages.is_empty() {
        let max_id = messages.last().unwrap().id;
        tx.execute(
            "UPDATE messages SET read = 1 WHERE to_agent = ?1 AND read = 0 AND id <= ?2",
            rusqlite::params![agent_id, max_id],
        )?;
    }
    tx.commit()?;
    Ok(messages)
}
```

Changes from current code:
- Added `if !messages.is_empty()` guard (skip no-op UPDATE)
- Added `AND id <= ?2` fence (only mark messages we actually SELECTed)

- [ ] **Step 2: Run tests**

Run: `cargo test`
Expected: all 61 existing tests pass. The behavior is identical for normal use — the fence only matters when a new message arrives between SELECT and UPDATE.

- [ ] **Step 3: Commit**

```bash
git add src/store.rs
git commit -m "fix: bound receive UPDATE to selected message IDs to prevent phantom read"
```

---

### Task 2: Update slash command templates

**Files:**
- Modify: `src/setup.rs` (SQUAD_MD_CONTENT, SQUAD_TOML_CONTENT)

- [ ] **Step 1: Update both templates**

In both `SQUAD_MD_CONTENT` and `SQUAD_TOML_CONTENT`, make these changes:

**Step 4 (Communicate):** Replace `squad receive <your-id> --wait` with `squad receive <your-id>`:
```
4. Communicate using squad commands:
   - `squad send <your-id> <to> "<message>"` — send a message (use @all to broadcast)
   - `squad receive <your-id>` — check for new messages
   - `squad agents` — see who is online
   - `squad pending` — check unread messages
   - `squad history` — view message history
```

**Step 5:** Replace blocking wait with non-blocking check:
```
5. After completing any task, check for new messages:
   `squad receive <your-id>`
   If no messages, continue with other work or check again shortly.
```

**Step 6:** Keep `squad agents` confirmation as-is.

**Step 7 (old IMPORTANT --wait retry):** Remove entirely.

**Step 8 (SESSION CONFLICT):** Renumber to 7. Keep content as-is.

- [ ] **Step 2: Run tests**

Run: `cargo test`
Expected: check if `test_md_content_has_required_sections` or `test_toml_content_has_required_sections` assert on `--wait`. If so, update them.

- [ ] **Step 3: Fix any failing setup tests**

If tests check for `--wait` in template content, update the assertions to check for the new content (e.g., check for `squad receive <your-id>` without `--wait`).

- [ ] **Step 4: Commit**

```bash
git add src/setup.rs tests/setup_test.rs
git commit -m "fix: remove --wait from slash command templates, use non-blocking receive"
```

---

### Task 3: Update role templates

**Files:**
- Modify: `src/roles/worker.md`
- Modify: `src/roles/manager.md`
- Modify: `src/roles/inspector.md`

- [ ] **Step 1: Update worker.md**

Replace lines 11-12 with:
```
- After completing a task, check for new messages with `squad receive <your-id>`
- If no messages, continue with other work or check again shortly.
```

- [ ] **Step 2: Update manager.md**

Replace lines 18-19 with:
```
- When waiting for results, check for messages with `squad receive manager`
- If no messages yet, continue monitoring or check again shortly.
```

- [ ] **Step 3: Update inspector.md**

Replace lines 19-20 with:
```
- After completing a review, check for new messages with `squad receive <your-id>`
- If no messages, continue with other work or check again shortly.
```

- [ ] **Step 4: Run tests**

Run: `cargo test`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add src/roles/worker.md src/roles/manager.md src/roles/inspector.md
git commit -m "fix: update role templates to use non-blocking receive"
```

---

### Task 4: Fix freeform role message and HELP_TEXT

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Fix freeform role fallback in cmd_join**

In `src/main.rs` line 169, change:
```rust
// Before:
println!("Communicate using: squad send, squad receive --wait, squad agents");

// After:
println!("Communicate using: squad send, squad receive, squad agents");
```

- [ ] **Step 2: Update HELP_TEXT**

In `src/main.rs` HELP_TEXT (line 376+), update:

**COMMANDS section:** Change receive description:
```
  squad receive <id> [--wait] [--timeout N]  Check inbox (--wait blocks, for debug only)
```

**QUICK START step 5:** Change:
```
  5. squad receive worker                    Worker checks for task
```

**HOW TO PARTICIPATE step 4:** Change:
```
  4. squad receive <your-id>                 Check for next task or feedback
```

**EXAMPLES:** Change:
```
  squad receive worker --wait --timeout 60
```
To:
```
  squad receive worker
```

- [ ] **Step 3: Run tests**

Run: `cargo test`
Expected: all pass.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "fix: remove --wait from freeform role output and help text"
```

---

### Task 5: Update READMEs

**Files:**
- Modify: `README.md`
- Modify: `README.zh-CN.md`

- [ ] **Step 1: Update README.md**

**Commands table (line 44):** Change:
```
| `squad receive <id> [--wait] [--timeout N]` | Check inbox (`--wait` blocks until message, default 3600s) |
```
To:
```
| `squad receive <id> [--wait]` | Check inbox (`--wait` for debug only) |
```

**Quick Start (line 32):** Change `squad receive worker --wait` to `squad receive worker`.

**ASCII diagram (lines 80-92):** Replace all `squad receive <agent> --wait` with `squad receive <agent>`.

**"The `--wait` Pattern" section (lines 97-108):** Replace with a simpler "Message Checking" section:
```
### Checking for Messages

After completing work, agents check for new messages:

```
Agent completes task
  → squad send <id> manager "done: summary..."
  → squad receive <id>                        ← check for next task
  → if no messages, continue other work
  → check again when ready
```
```

- [ ] **Step 2: Update README.zh-CN.md**

Apply equivalent changes to the Chinese README:
- Commands table: update receive row
- Manual example section: remove `--wait`
- Any `--wait` Pattern equivalent section

- [ ] **Step 3: Run tests**

Run: `cargo test`
Expected: all pass.

- [ ] **Step 4: Commit**

```bash
git add README.md README.zh-CN.md
git commit -m "docs: update READMEs to reflect non-blocking receive pattern"
```

---

### Task 6: Reinstall and verify

- [ ] **Step 1: Run full test suite + clippy**

```bash
cargo test
cargo clippy -- -D warnings
```

- [ ] **Step 2: Reinstall and update slash commands**

```bash
cargo install --path .
squad setup
```

- [ ] **Step 3: Manual smoke test**

```bash
squad clean && squad init
squad join worker --role worker
squad send worker worker "test"
squad receive worker             # should show message
squad receive worker             # should show "No new messages"
```

- [ ] **Step 4: Commit version bump**

```bash
# In Cargo.toml, change version to "0.3.1"
git add Cargo.toml
git commit -m "chore: bump version to 0.3.1"
```

---

## Summary

| Task | Files | What changes |
|------|-------|-------------|
| 1. id fence | store.rs | 3 lines — `id <= max_id` in UPDATE |
| 2. Slash command templates | setup.rs, setup_test.rs | Remove --wait, simplify instructions |
| 3. Role templates | worker.md, manager.md, inspector.md | Remove --wait, use plain receive |
| 4. Freeform + help | main.rs | Fix line 169 + HELP_TEXT |
| 5. READMEs | README.md, README.zh-CN.md | Remove --wait pattern, update examples |
| 6. Verify + release | Cargo.toml | Version 0.3.1 |
| **Total** | **8 files** | **~50 lines changed** |
