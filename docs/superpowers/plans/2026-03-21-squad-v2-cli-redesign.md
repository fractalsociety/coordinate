# Squad V2: CLI-Based Multi-Agent Communication Redesign

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Redesign squad from a daemon-based workflow engine into a lightweight CLI message bus that lets multiple AI agents (Claude Code, Gemini, Codex, etc.) communicate via simple shell commands — no daemon, no MCP server, no YAML workflows.

**Architecture:** All agent communication happens through CLI commands (`squad join`, `squad send`, `squad receive`, `squad agents`, `squad leave`) backed by a single SQLite file (`.squad/messages.db`). Agent identity is established at runtime via `squad join <id> [--role <role>]`. Role templates (`.md` files) provide predefined behavior prompts. No background processes — every command is a one-shot operation that reads/writes SQLite directly. Agent lifecycle is managed explicitly via `squad join` / `squad leave` (no PID tracking — AI agents invoke commands via transient subshells, making PID-based cleanup unreliable).

**Tech Stack:** Rust (sync, no tokio), rusqlite (bundled SQLite), serde/serde_json, serde_yaml, anyhow

**Key design decisions (from review):**
- `squad send <from> <to> <message>` — sender is explicit, three positional args (AI agents know their own ID)
- `squad send <from> @all <message>` — broadcast to all agents except sender
- `squad join <id>` — ID is the agent's unique name. `--role <role>` optionally loads a role template. Allows `squad join worker-1 --role worker` and `squad join worker-2 --role worker`
- `squad receive <id> --wait` — blocks until a message arrives (eliminates polling; agent runs this when idle)
- No PID tracking — agents are removed only by explicit `squad leave` or `squad clean`
- Each task in the plan adds its module to `lib.rs` incrementally (avoids compile failures from missing modules)
- SQLite: `PRAGMA busy_timeout=5000` for concurrent access; `receive_messages` uses transaction for atomicity

---

## File Structure

### Files to DELETE (entire old architecture)

```
src/daemon/           — All files (server.rs, registry.rs, mailbox.rs, health.rs, recovery.rs, audit.rs, session.rs, store.rs, mod.rs)
src/mcp/              — All files (mod.rs, tools.rs, client.rs, transport.rs)
src/protocol/         — All files (mod.rs)
src/workflow/          — All files (engine.rs, mod.rs)
src/adapter/           — All files (mod.rs, hook.rs, watcher.rs)
src/tui/              — All files (app.rs, ui.rs, mod.rs)
src/bin/              — All files (squad-mcp.rs, squad-hook.rs)
src/setup.rs          — Will be replaced
src/config/mod.rs     — Will be replaced
```

### Files to CREATE

```
src/
  main.rs             — CLI entry point: init, join, agents, send, receive, leave, pending, history, team, roles, clean
  lib.rs              — Module exports (added incrementally per task)
  store.rs            — SQLite: messages table + agents table (with busy_timeout, transactions, broadcast)
  roles.rs            — Load role templates from .squad/roles/*.md
  teams.rs            — Load team templates from .squad/teams/*.yaml
  init.rs             — Workspace initialization
  roles/
    manager.md        — Builtin role template (bundled via include_str!)
    worker.md         — Builtin role template
    inspector.md      — Builtin role template
tests/
  store_test.rs       — SQLite store unit tests
  roles_test.rs       — Role template tests
  teams_test.rs       — Team template tests
  init_test.rs        — Init command tests
  cli_test.rs         — CLI integration tests
  e2e_test.rs         — Full collaboration scenario tests
```

### Runtime directory structure

```
.squad/
  messages.db          — SQLite: messages + agents tables
  roles/
    manager.md         — Predefined role: project manager
    worker.md          — Predefined role: execution worker
    inspector.md       — Predefined role: code reviewer/inspector
  teams/
    dev.yaml           — Predefined team: manager + worker + inspector
```

---

## Task 1: Project Cleanup & Dependency Update

**Files:**
- Modify: `Cargo.toml`
- Delete: `src/daemon/`, `src/mcp/`, `src/protocol/`, `src/workflow/`, `src/adapter/`, `src/tui/`, `src/bin/`, `src/setup.rs`, `src/config/`
- Modify: `src/lib.rs` (empty stub)
- Modify: `src/main.rs` (minimal stub)

- [ ] **Step 1: Delete all old source directories and files**

```bash
rm -rf src/daemon src/mcp src/protocol src/workflow src/adapter src/tui src/bin src/config
rm -f src/setup.rs
```

- [ ] **Step 2: Update Cargo.toml**

```toml
[package]
name = "squad"
version = "0.2.0"
edition = "2021"
license = "MIT"
description = "Multi-AI-agent terminal collaboration via simple CLI commands"
repository = "https://github.com/fractalsociety/coordinate"

[dependencies]
anyhow = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"
rusqlite = { version = "0.31", features = ["bundled"] }
chrono = "0.4"

[dev-dependencies]
tempfile = "3"
assert_cmd = "2"
predicates = "3"
```

No `[[bin]]` sections — only the default binary from `src/main.rs`.

- [ ] **Step 3: Create empty lib.rs**

```rust
// src/lib.rs
// Modules are added as they are implemented in subsequent tasks.
```

- [ ] **Step 4: Create minimal main.rs stub**

```rust
use anyhow::Result;

fn main() -> Result<()> {
    println!("squad v0.2.0");
    Ok(())
}
```

- [ ] **Step 5: Delete old tests**

```bash
rm -f tests/e2e.rs tests/adapter.rs tests/daemon_cli.rs tests/daemon_persistence.rs tests/mcp_tools.rs tests/mcp_transport.rs tests/setup_doctor.rs tests/tui_watch.rs tests/workflow_engine.rs tests/audit_log.rs
```

- [ ] **Step 6: Verify compilation**

Run: `cargo build`
Expected: Success

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "chore: strip old daemon/mcp/workflow architecture for v2 redesign"
```

---

## Task 2: SQLite Message Store

**Files:**
- Create: `src/store.rs`
- Modify: `src/lib.rs` (add `pub mod store;`)
- Test: `tests/store_test.rs`

- [ ] **Step 1: Add module to lib.rs**

```rust
// src/lib.rs
pub mod store;
```

- [ ] **Step 2: Write failing test for store init**

```rust
// tests/store_test.rs
use squad::store::Store;
use tempfile::TempDir;

#[test]
fn test_store_init_creates_tables() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    let agents = store.list_agents().unwrap();
    assert!(agents.is_empty());
}
```

- [ ] **Step 3: Run test — expect FAIL**

Run: `cargo test --test store_test test_store_init`
Expected: FAIL — store.rs doesn't exist

- [ ] **Step 4: Implement Store**

```rust
// src/store.rs
use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AgentRecord {
    pub id: String,
    pub role: String,
    pub joined_at: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MessageRecord {
    pub id: i64,
    pub from_agent: String,
    pub to_agent: String,
    pub content: String,
    pub created_at: i64,
    pub read: bool,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open database: {}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;"
        )?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS agents (
                id TEXT PRIMARY KEY,
                role TEXT NOT NULL,
                joined_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                from_agent TEXT NOT NULL,
                to_agent TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                read INTEGER NOT NULL DEFAULT 0
            );"
        )?;
        Ok(Self { conn })
    }

    pub fn register_agent(&self, id: &str, role: &str) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        self.conn.execute(
            "INSERT OR REPLACE INTO agents (id, role, joined_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![id, role, now],
        )?;
        Ok(())
    }

    pub fn unregister_agent(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM agents WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn list_agents(&self) -> Result<Vec<AgentRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, role, joined_at FROM agents ORDER BY joined_at"
        )?;
        let agents = stmt.query_map([], |row| {
            Ok(AgentRecord {
                id: row.get(0)?,
                role: row.get(1)?,
                joined_at: row.get(2)?,
            })
        })?.collect::<Result<Vec<_>, _>>()?;
        Ok(agents)
    }

    pub fn agent_exists(&self, id: &str) -> Result<bool> {
        let exists: bool = self.conn.query_row(
            "SELECT COUNT(*) > 0 FROM agents WHERE id = ?1", [id], |row| row.get(0),
        )?;
        Ok(exists)
    }

    fn agent_names(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT id FROM agents ORDER BY id")?;
        let names = stmt.query_map([], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(names)
    }

    pub fn send_message(&self, from: &str, to: &str, content: &str) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        self.conn.execute(
            "INSERT INTO messages (from_agent, to_agent, content, created_at, read) VALUES (?1, ?2, ?3, ?4, 0)",
            rusqlite::params![from, to, content, now],
        )?;
        Ok(())
    }

    pub fn send_message_checked(&self, from: &str, to: &str, content: &str) -> Result<()> {
        if !self.agent_exists(to)? {
            let names = self.agent_names()?;
            anyhow::bail!("{to} does not exist. Online agents: {}", names.join(", "));
        }
        self.send_message(from, to, content)
    }

    /// Broadcast a message to all agents except the sender.
    pub fn broadcast_message(&self, from: &str, content: &str) -> Result<Vec<String>> {
        let agents = self.agent_names()?;
        let recipients: Vec<_> = agents.into_iter().filter(|a| a != from).collect();
        for to in &recipients {
            self.send_message(from, to, content)?;
        }
        Ok(recipients)
    }

    /// Atomically read and mark messages as read using a transaction.
    pub fn receive_messages(&self, agent_id: &str) -> Result<Vec<MessageRecord>> {
        let tx = self.conn.unchecked_transaction()?;
        let mut stmt = tx.prepare(
            "SELECT id, from_agent, to_agent, content, created_at, read
             FROM messages WHERE to_agent = ?1 AND read = 0 ORDER BY created_at"
        )?;
        let messages: Vec<MessageRecord> = stmt.query_map([agent_id], |row| {
            Ok(MessageRecord {
                id: row.get(0)?,
                from_agent: row.get(1)?,
                to_agent: row.get(2)?,
                content: row.get(3)?,
                created_at: row.get(4)?,
                read: row.get(5)?,
            })
        })?.collect::<Result<Vec<_>, _>>()?;
        drop(stmt);

        tx.execute(
            "UPDATE messages SET read = 1 WHERE to_agent = ?1 AND read = 0",
            [agent_id],
        )?;
        tx.commit()?;
        Ok(messages)
    }

    /// Check if there are unread messages for an agent (used by --wait).
    pub fn has_unread_messages(&self, agent_id: &str) -> Result<bool> {
        let has: bool = self.conn.query_row(
            "SELECT COUNT(*) > 0 FROM messages WHERE to_agent = ?1 AND read = 0",
            [agent_id],
            |row| row.get(0),
        )?;
        Ok(has)
    }

    pub fn pending_messages(&self) -> Result<Vec<MessageRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, from_agent, to_agent, content, created_at, read
             FROM messages WHERE read = 0 ORDER BY created_at"
        )?;
        let messages = stmt.query_map([], |row| {
            Ok(MessageRecord {
                id: row.get(0)?,
                from_agent: row.get(1)?,
                to_agent: row.get(2)?,
                content: row.get(3)?,
                created_at: row.get(4)?,
                read: row.get(5)?,
            })
        })?.collect::<Result<Vec<_>, _>>()?;
        Ok(messages)
    }

    /// All messages (including read), optionally filtered by agent.
    pub fn all_messages(&self, agent_id: Option<&str>) -> Result<Vec<MessageRecord>> {
        let (sql, params): (&str, Vec<&dyn rusqlite::types::ToSql>) = match agent_id {
            Some(id) => (
                "SELECT id, from_agent, to_agent, content, created_at, read
                 FROM messages WHERE from_agent = ?1 OR to_agent = ?1 ORDER BY created_at",
                vec![&id as &dyn rusqlite::types::ToSql],
            ),
            None => (
                "SELECT id, from_agent, to_agent, content, created_at, read
                 FROM messages ORDER BY created_at",
                vec![],
            ),
        };
        let mut stmt = self.conn.prepare(sql)?;
        let messages = stmt.query_map(params.as_slice(), |row| {
            Ok(MessageRecord {
                id: row.get(0)?,
                from_agent: row.get(1)?,
                to_agent: row.get(2)?,
                content: row.get(3)?,
                created_at: row.get(4)?,
                read: row.get(5)?,
            })
        })?.collect::<Result<Vec<_>, _>>()?;
        Ok(messages)
    }
}
```

- [ ] **Step 5: Write remaining store tests**

```rust
// Append to tests/store_test.rs

#[test]
fn test_register_and_list_agent() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    store.register_agent("manager", "manager").unwrap();
    let agents = store.list_agents().unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].id, "manager");
}

#[test]
fn test_unregister_agent() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    store.register_agent("worker", "worker").unwrap();
    store.unregister_agent("worker").unwrap();
    assert!(store.list_agents().unwrap().is_empty());
}

#[test]
fn test_send_and_receive_messages() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    store.register_agent("manager", "manager").unwrap();
    store.register_agent("worker", "worker").unwrap();

    store.send_message("manager", "worker", "implement auth module").unwrap();
    store.send_message("manager", "worker", "also add tests").unwrap();

    let messages = store.receive_messages("worker").unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].from_agent, "manager");
    assert_eq!(messages[0].content, "implement auth module");

    // Already read — should be empty now
    let again = store.receive_messages("worker").unwrap();
    assert!(again.is_empty());
}

#[test]
fn test_send_to_nonexistent_agent_fails() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    store.register_agent("manager", "manager").unwrap();

    let result = store.send_message_checked("manager", "nobody", "hello");
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("does not exist"));
}

#[test]
fn test_broadcast_message() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    store.register_agent("manager", "manager").unwrap();
    store.register_agent("worker-1", "worker").unwrap();
    store.register_agent("worker-2", "worker").unwrap();

    let recipients = store.broadcast_message("manager", "code interface changed").unwrap();
    assert_eq!(recipients.len(), 2);
    assert!(recipients.contains(&"worker-1".to_string()));
    assert!(recipients.contains(&"worker-2".to_string()));

    let msgs1 = store.receive_messages("worker-1").unwrap();
    assert_eq!(msgs1.len(), 1);
    assert_eq!(msgs1[0].content, "code interface changed");

    // Manager should NOT receive its own broadcast
    let msgs_mgr = store.receive_messages("manager").unwrap();
    assert!(msgs_mgr.is_empty());
}

#[test]
fn test_has_unread_messages() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();

    assert!(!store.has_unread_messages("worker").unwrap());
    store.send_message("manager", "worker", "task").unwrap();
    assert!(store.has_unread_messages("worker").unwrap());

    store.receive_messages("worker").unwrap();
    assert!(!store.has_unread_messages("worker").unwrap());
}

#[test]
fn test_all_messages_history() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();

    store.send_message("manager", "worker", "task 1").unwrap();
    store.send_message("worker", "manager", "done").unwrap();
    store.receive_messages("worker").unwrap(); // marks task 1 as read

    // all_messages returns read + unread
    let all = store.all_messages(None).unwrap();
    assert_eq!(all.len(), 2);

    // Filtered by agent
    let worker_msgs = store.all_messages(Some("worker")).unwrap();
    assert_eq!(worker_msgs.len(), 2); // both sent-to and sent-from
}

#[test]
fn test_pending_messages() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();

    store.send_message("manager", "worker", "task 1").unwrap();
    store.send_message("inspector", "worker", "review").unwrap();
    assert_eq!(store.pending_messages().unwrap().len(), 2);
}

#[test]
fn test_multiple_agents_same_role() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    store.register_agent("worker-1", "worker").unwrap();
    store.register_agent("worker-2", "worker").unwrap();

    let agents = store.list_agents().unwrap();
    assert_eq!(agents.len(), 2);

    store.send_message("manager", "worker-1", "task A").unwrap();
    store.send_message("manager", "worker-2", "task B").unwrap();

    let msgs1 = store.receive_messages("worker-1").unwrap();
    assert_eq!(msgs1[0].content, "task A");

    let msgs2 = store.receive_messages("worker-2").unwrap();
    assert_eq!(msgs2[0].content, "task B");
}
```

- [ ] **Step 6: Run all store tests**

Run: `cargo test --test store_test`
Expected: ALL PASS

- [ ] **Step 7: Commit**

```bash
git add src/store.rs src/lib.rs tests/store_test.rs
git commit -m "feat: add SQLite store with busy_timeout, atomic receive, broadcast, and history"
```

---

## Task 3: Role Templates

**Files:**
- Create: `src/roles.rs`, `src/roles/manager.md`, `src/roles/worker.md`, `src/roles/inspector.md`
- Modify: `src/lib.rs` (add `pub mod roles;`)
- Test: `tests/roles_test.rs`

- [ ] **Step 1: Add module to lib.rs**

Append `pub mod roles;` to `src/lib.rs`.

- [ ] **Step 2: Write failing tests**

```rust
// tests/roles_test.rs
use squad::roles::{load_role, default_role_prompt, BUILTIN_ROLES};
use tempfile::TempDir;
use std::fs;

#[test]
fn test_builtin_roles_exist() {
    assert!(BUILTIN_ROLES.contains(&"manager"));
    assert!(BUILTIN_ROLES.contains(&"worker"));
    assert!(BUILTIN_ROLES.contains(&"inspector"));
}

#[test]
fn test_load_builtin_role() {
    let prompt = default_role_prompt("manager").unwrap();
    assert!(prompt.contains("manager"));
}

#[test]
fn test_load_custom_role_from_disk() {
    let tmp = TempDir::new().unwrap();
    let roles_dir = tmp.path().join(".squad").join("roles");
    fs::create_dir_all(&roles_dir).unwrap();
    fs::write(roles_dir.join("custom.md"), "You are a custom agent.").unwrap();
    let prompt = load_role(tmp.path(), "custom").unwrap();
    assert_eq!(prompt, "You are a custom agent.");
}

#[test]
fn test_custom_role_overrides_builtin() {
    let tmp = TempDir::new().unwrap();
    let roles_dir = tmp.path().join(".squad").join("roles");
    fs::create_dir_all(&roles_dir).unwrap();
    fs::write(roles_dir.join("manager.md"), "Custom manager.").unwrap();
    let prompt = load_role(tmp.path(), "manager").unwrap();
    assert_eq!(prompt, "Custom manager.");
}
```

- [ ] **Step 3: Run tests — expect FAIL**

Run: `cargo test --test roles_test`

- [ ] **Step 4: Create builtin role templates**

Create `src/roles/` directory with three `.md` files:

**src/roles/manager.md:**
```markdown
You are the project manager (manager).

## Responsibilities
- Analyze the user's goal and break it into concrete sub-tasks
- Run `squad agents` to see who is on the team
- Use `squad send manager <agent> "<task>"` to assign tasks
- Use `squad send manager @all "<announcement>"` to broadcast to everyone
- Collect results and forward to inspectors for review
- Based on inspector feedback, decide whether to request rework
- When all tasks pass review, summarize the final result to the user

## Collaboration Rules
- Before assigning tasks, check who is online with `squad agents`
- When assigning, clearly state requirements and acceptance criteria
- After receiving worker results, forward to inspector for review
- If inspector says FAIL, forward feedback to the worker for rework
- If inspector says PASS, the task is complete
- When waiting for results, run `squad receive manager --wait` to block until a message arrives
```

**src/roles/worker.md:**
```markdown
You are an execution worker (worker).

## Responsibilities
- Execute assigned tasks (write code, fix bugs, implement features, etc.)
- Report results back with `squad send <your-id> manager "<summary>"`
- When receiving revision requests, address all points and report back

## Collaboration Rules
- Only work on tasks assigned by the manager
- Always include a clear summary of changes made
- After completing a task, run `squad receive <your-id> --wait` to wait for the next task or feedback
```

**src/roles/inspector.md:**
```markdown
You are the code inspector (inspector).

## Responsibilities
- Review code changes, implementation quality, and correctness
- Send results to both the worker and manager:
  - `squad send <your-id> <worker-id> "<specific feedback>"`
  - `squad send <your-id> manager "PASS: <summary>"` or `"FAIL: <issues>"`

## Review Criteria
- Code correctness and logic
- Error handling and edge cases
- Code readability and maintainability
- Security considerations
- Whether the implementation meets the stated requirements

## Collaboration Rules
- Be specific in feedback — point to exact issues and suggest fixes
- Use PASS or FAIL as the first word when reporting to manager
- After completing a review, run `squad receive <your-id> --wait` to wait for the next review request
```

- [ ] **Step 5: Implement roles.rs**

```rust
// src/roles.rs
use anyhow::{Context, Result};
use std::path::Path;

pub const BUILTIN_ROLES: &[&str] = &["manager", "worker", "inspector"];

pub fn default_role_prompt(role: &str) -> Option<String> {
    match role {
        "manager" => Some(include_str!("roles/manager.md").to_string()),
        "worker" => Some(include_str!("roles/worker.md").to_string()),
        "inspector" => Some(include_str!("roles/inspector.md").to_string()),
        _ => None,
    }
}

pub fn load_role(workspace: &Path, role: &str) -> Result<String> {
    let custom_path = workspace.join(".squad").join("roles").join(format!("{role}.md"));
    if custom_path.exists() {
        return std::fs::read_to_string(&custom_path)
            .with_context(|| format!("failed to read role: {}", custom_path.display()));
    }
    default_role_prompt(role)
        .with_context(|| format!("unknown role: {role}. Available: {}", BUILTIN_ROLES.join(", ")))
}

pub fn list_roles(workspace: &Path) -> Vec<String> {
    let mut roles: Vec<String> = BUILTIN_ROLES.iter().map(|s| s.to_string()).collect();
    let custom_dir = workspace.join(".squad").join("roles");
    if let Ok(entries) = std::fs::read_dir(&custom_dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.path().file_stem() {
                let name = name.to_string_lossy().to_string();
                if !roles.contains(&name) {
                    roles.push(name);
                }
            }
        }
    }
    roles.sort();
    roles
}
```

- [ ] **Step 6: Run tests**

Run: `cargo test --test roles_test`
Expected: ALL PASS

- [ ] **Step 7: Commit**

```bash
git add src/roles.rs src/roles/ src/lib.rs tests/roles_test.rs
git commit -m "feat: add role template system with builtin manager/worker/inspector"
```

---

## Task 4: Team Templates

**Files:**
- Create: `src/teams.rs`
- Modify: `src/lib.rs` (add `pub mod teams;`)
- Test: `tests/teams_test.rs`

- [ ] **Step 1: Add module, write failing tests, implement, verify**

Follow same pattern as Task 3. The `TeamConfig` struct:

```rust
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TeamConfig {
    pub name: String,
    pub roles: BTreeMap<String, TeamRole>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TeamRole {
    pub prompt_file: String,
}
```

Builtin team "dev" has roles: manager, worker, inspector.

`load_team(workspace, name)` checks `.squad/teams/{name}.yaml` first, falls back to builtin.

`squad team <name>` is display-only — shows which roles are needed and the join commands to run in each terminal. No batch-join (team apply) because each agent must run `squad join` in its own terminal to receive the role prompt output.

- [ ] **Step 2: Run tests**

Run: `cargo test --test teams_test`
Expected: ALL PASS

- [ ] **Step 3: Commit**

```bash
git add src/teams.rs src/lib.rs tests/teams_test.rs
git commit -m "feat: add team template system with builtin dev team"
```

---

## Task 5: Init Command

**Files:**
- Create: `src/init.rs`
- Modify: `src/lib.rs` (add `pub mod init;`)
- Test: `tests/init_test.rs`

- [ ] **Step 1: Add module, write failing tests**

```rust
// tests/init_test.rs
use squad::init::init_workspace;
use tempfile::TempDir;

#[test]
fn test_init_creates_squad_directory() {
    let tmp = TempDir::new().unwrap();
    init_workspace(tmp.path()).unwrap();
    assert!(tmp.path().join(".squad").exists());
    assert!(tmp.path().join(".squad").join("roles").exists());
    assert!(tmp.path().join(".squad").join("teams").exists());
    assert!(tmp.path().join(".squad").join("roles").join("manager.md").exists());
}

#[test]
fn test_init_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    init_workspace(tmp.path()).unwrap();
    init_workspace(tmp.path()).unwrap(); // Should not error
}
```

- [ ] **Step 2: Implement init.rs**

Creates `.squad/`, `.squad/roles/` with default templates, `.squad/teams/`, adds `.squad/` to `.gitignore`.

- [ ] **Step 3: Run tests**

Run: `cargo test --test init_test`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add src/init.rs src/lib.rs tests/init_test.rs
git commit -m "feat: add workspace initialization with default role templates"
```

---

## Task 6: CLI Entry Point — All Commands

**Files:**
- Modify: `src/main.rs`
- Test: `tests/cli_test.rs`

This is the core wiring task.

- [ ] **Step 1: Write CLI integration tests**

```rust
// tests/cli_test.rs
use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn squad(workspace: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("squad").unwrap();
    cmd.current_dir(workspace);
    cmd
}

#[test]
fn test_init() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success()
        .stdout(predicate::str::contains("Initialized"));
}

#[test]
fn test_join_and_agents() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path()).args(["join", "manager"]).assert().success()
        .stdout(predicate::str::contains("Joined as manager"));
    squad(tmp.path()).arg("agents").assert().success()
        .stdout(predicate::str::contains("manager"));
}

#[test]
fn test_join_with_role_flag() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path()).args(["join", "worker-1", "--role", "worker"]).assert().success()
        .stdout(predicate::str::contains("Joined as worker-1"));
}

#[test]
fn test_send_and_receive() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path()).args(["join", "manager"]).assert().success();
    squad(tmp.path()).args(["join", "worker"]).assert().success();

    squad(tmp.path())
        .args(["send", "manager", "worker", "implement auth module"])
        .assert().success();

    squad(tmp.path())
        .args(["receive", "worker"])
        .assert().success()
        .stdout(predicate::str::contains("[from manager]"))
        .stdout(predicate::str::contains("implement auth module"));
}

#[test]
fn test_send_broadcast() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path()).args(["join", "manager"]).assert().success();
    squad(tmp.path()).args(["join", "worker-1"]).assert().success();
    squad(tmp.path()).args(["join", "worker-2"]).assert().success();

    squad(tmp.path())
        .args(["send", "manager", "@all", "interface changed"])
        .assert().success()
        .stdout(predicate::str::contains("Broadcast to 2 agents"));

    squad(tmp.path())
        .args(["receive", "worker-1"])
        .assert().success()
        .stdout(predicate::str::contains("interface changed"));
}

#[test]
fn test_send_to_nonexistent() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path()).args(["join", "manager"]).assert().success();

    squad(tmp.path())
        .args(["send", "manager", "nobody", "hello"])
        .assert().failure()
        .stderr(predicate::str::contains("does not exist"));
}

#[test]
fn test_leave() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path()).args(["join", "manager"]).assert().success();
    squad(tmp.path()).args(["leave", "manager"]).assert().success();
    squad(tmp.path()).arg("agents").assert().success()
        .stdout(predicate::str::contains("No agents"));
}

#[test]
fn test_history() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path()).args(["join", "manager"]).assert().success();
    squad(tmp.path()).args(["join", "worker"]).assert().success();

    squad(tmp.path()).args(["send", "manager", "worker", "task 1"]).assert().success();
    squad(tmp.path()).args(["receive", "worker"]).assert().success(); // marks as read

    // history still shows it
    squad(tmp.path()).arg("history").assert().success()
        .stdout(predicate::str::contains("task 1"));
}
```

- [ ] **Step 2: Run tests — expect FAIL**

Run: `cargo test --test cli_test`

- [ ] **Step 3: Implement main.rs**

Key command signatures:
- `squad init`
- `squad join <id> [--role <role>]` — if `--role` omitted, role = id
- `squad leave <id>`
- `squad agents`
- `squad send <from> <to> <message...>` — three positional args. If `to` is `@all`, broadcast
- `squad receive <id> [--wait] [--timeout <secs>]` — `--wait` blocks until message arrives
- `squad pending`
- `squad history [agent]` — show all messages including read ones
- `squad roles`
- `squad teams`
- `squad team <name>`
- `squad clean`
- `squad help`

The `cmd_receive` with `--wait`:
```rust
fn cmd_receive(agent: &str, wait: bool, timeout_secs: u64) -> Result<()> {
    let workspace = find_workspace()?;
    let store = open_store(&workspace)?;

    if wait {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
        loop {
            let messages = store.receive_messages(agent)?;
            if !messages.is_empty() {
                print_messages(&messages);
                return Ok(());
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
            print_messages(&messages);
        }
        Ok(())
    }
}
```

The `cmd_send` with `@all` broadcast:
```rust
fn cmd_send(from: &str, to: &str, content: &str) -> Result<()> {
    let workspace = find_workspace()?;
    let store = open_store(&workspace)?;
    if to == "@all" {
        let recipients = store.broadcast_message(from, content)?;
        println!("Broadcast to {} agents: {}", recipients.len(), recipients.join(", "));
    } else {
        store.send_message_checked(from, to, content)?;
        println!("Sent to {to}.");
    }
    Ok(())
}
```

UTF-8 safe truncation in `cmd_pending`:
```rust
let preview: String = msg.content.chars().take(60).collect();
let suffix = if msg.content.chars().count() > 60 { "..." } else { "" };
```

`find_workspace()` walks up from cwd looking for `.squad/` directory. Clear error: `"Not a squad workspace. Run 'squad init' first."`

- [ ] **Step 4: Run CLI tests**

Run: `cargo test --test cli_test`
Expected: ALL PASS

- [ ] **Step 5: Commit**

```bash
git add src/main.rs tests/cli_test.rs
git commit -m "feat: implement all CLI commands with --wait, broadcast, and history"
```

---

## Task 7: End-to-End Integration Tests

**Files:**
- Create: `tests/e2e_test.rs`

- [ ] **Step 1: Write full collaboration scenario test**

Test the complete flow:
1. Three agents join (manager, worker, inspector)
2. Manager sends task to worker
3. Worker replies to manager
4. Manager forwards to inspector
5. Inspector sends FAIL to worker + manager
6. Worker fixes and reports back
7. Inspector sends PASS to manager
8. All messages received correctly
9. `history` shows the full conversation

Also test: broadcast, multiple workers with same role, send to left agent fails, clean command.

- [ ] **Step 2: Run all tests**

Run: `cargo test`
Expected: ALL PASS

- [ ] **Step 3: Commit**

```bash
git add tests/e2e_test.rs
git commit -m "test: add end-to-end collaboration flow tests"
```

---

## Task 8: Documentation Update

**Files:**
- Modify: `README.md`
- Delete: old docs and examples

- [ ] **Step 1: Update README.md for v2**

Key sections: Quick Start, Commands table, Role templates, Team templates, How agents communicate, the `--wait` pattern.

- [ ] **Step 2: Delete obsolete files**

```bash
rm -f docs/getting-started.md docs/workflow-modes.md docs/adapters.md docs/cli-reference.md docs/squad-yaml.md
rm -rf examples/
```

- [ ] **Step 3: Final build + test**

Run: `cargo build && cargo test`
Expected: ALL PASS

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "docs: update README and clean up obsolete docs/examples for v2"
```

---

## Summary

| Task | What | Key Files |
|------|------|-----------|
| 1 | Project cleanup | Cargo.toml, delete old src/ |
| 2 | SQLite store | `store.rs` (busy_timeout, atomic receive, broadcast, history) |
| 3 | Role templates | `roles.rs` + `src/roles/*.md` (with `--wait` instructions) |
| 4 | Team templates | `teams.rs` (display-only, no batch-join) |
| 5 | Init command | `init.rs` |
| 6 | All CLI commands | `main.rs` (with `--wait`, `@all`, `history`) |
| 7 | E2E tests | `e2e_test.rs` |
| 8 | Documentation | `README.md` |

**Before (v1):** 28 source files, ~4500 LOC, daemon + MCP + workflow engine + TUI + 3 adapters

**After (v2):** ~6 source files, ~600 LOC, pure CLI + SQLite

**Key CLI commands:**
```
squad init                                     # Initialize workspace
squad join <id> [--role <role>]                # Join as agent (role defaults to id)
squad leave <id>                               # Remove agent
squad agents                                   # List online agents
squad send <from> <to> <message>               # Send message
squad send <from> @all <message>               # Broadcast to all agents
squad receive <id>                             # Check inbox (immediate)
squad receive <id> --wait [--timeout <secs>]   # Block until message arrives (default 120s)
squad pending                                  # Show all unread messages
squad history [agent]                          # Show all messages (including read)
squad roles                                    # List available roles
squad team <name>                              # Show team template info
squad clean                                    # Clear all state
```

**Agent work loop (enabled by --wait):**
```
Agent completes task
  → squad send <id> manager "done: summary..."
  → squad receive <id> --wait              ← blocks here until next message
  → receives new task or feedback
  → works on it
  → repeat
```
