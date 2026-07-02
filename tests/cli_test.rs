use assert_cmd::Command;
use predicates::prelude::*;
use rusqlite::Connection;
use serde_json::Value;
use squad::autopilot::{RiskLevel, TaskGraphStatus, TaskGraphTask};
use squad::setup::{command_content, current_version, PLATFORMS};
use squad::store::{AutopilotAgentInput, Store};
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

#[cfg(unix)]
fn make_executable(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

fn create_fake_binary(dir: &std::path::Path, name: &str) {
    let path = dir.join(name);
    std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    make_executable(&path);
}

fn create_fake_tmux(dir: &std::path::Path) {
    let path = dir.join("tmux");
    std::fs::write(
        &path,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TMUX_LOG\"\nstate=\"$TMUX_LOG.session\"\nif [ \"$1\" = \"has-session\" ]; then [ -f \"$state\" ] && exit 0 || exit 1; fi\nif [ \"$1\" = \"new-session\" ]; then : > \"$state\"; fi\nexit 0\n",
    )
    .unwrap();
    #[cfg(unix)]
    make_executable(&path);
}

fn create_fake_osascript(dir: &std::path::Path) {
    let path = dir.join("osascript");
    std::fs::write(
        &path,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$OSASCRIPT_LOG\"\nexit 0\n",
    )
    .unwrap();
    #[cfg(unix)]
    make_executable(&path);
}

fn squad(workspace: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("squad").unwrap();
    cmd.current_dir(workspace);
    cmd
}

fn mark_agent_stale(workspace: &std::path::Path, agent_id: &str) {
    let db = workspace.join(".squad").join("messages.db");
    let conn = Connection::open(db).unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    conn.execute(
        "UPDATE agents SET last_seen = ?1 WHERE id = ?2",
        rusqlite::params![now - 601, agent_id],
    )
    .unwrap();
}

fn set_message_timestamp(workspace: &std::path::Path, content: &str, created_at: i64) {
    let db = workspace.join(".squad").join("messages.db");
    let conn = Connection::open(db).unwrap();
    conn.execute(
        "UPDATE messages SET created_at = ?1 WHERE content = ?2",
        rusqlite::params![created_at, content],
    )
    .unwrap();
}

fn first_task_id(workspace: &std::path::Path) -> String {
    let db = workspace.join(".squad").join("messages.db");
    let conn = Connection::open(db).unwrap();
    conn.query_row(
        "SELECT id FROM tasks ORDER BY created_at, id LIMIT 1",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

fn message_id_by_content(workspace: &std::path::Path, content: &str) -> i64 {
    let db = workspace.join(".squad").join("messages.db");
    let conn = Connection::open(db).unwrap();
    conn.query_row(
        "SELECT id FROM messages WHERE content = ?1 ORDER BY created_at, id LIMIT 1",
        [content],
        |row| row.get(0),
    )
    .unwrap()
}

fn path_with_fake_binary(tmp: &TempDir, binary: &str) -> String {
    let bin_dir = tmp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    create_fake_binary(&bin_dir, binary);
    match std::env::var("PATH") {
        Ok(existing) => format!("{}:{}", bin_dir.display(), existing),
        Err(_) => bin_dir.display().to_string(),
    }
}

fn path_with_fake_tmux(tmp: &TempDir) -> String {
    let bin_dir = tmp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    create_fake_tmux(&bin_dir);
    match std::env::var("PATH") {
        Ok(existing) => format!("{}:{}", bin_dir.display(), existing),
        Err(_) => bin_dir.display().to_string(),
    }
}

fn path_with_fake_osascript(tmp: &TempDir) -> String {
    let bin_dir = tmp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    create_fake_osascript(&bin_dir);
    match std::env::var("PATH") {
        Ok(existing) => format!("{}:{}", bin_dir.display(), existing),
        Err(_) => bin_dir.display().to_string(),
    }
}

#[test]
fn test_init() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path())
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("Initialized"));
}

#[test]
fn test_init_refresh_roles_updates_builtin_files_only() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();

    let roles_dir = tmp.path().join(".squad").join("roles");
    std::fs::write(roles_dir.join("manager.md"), "stale manager").unwrap();
    std::fs::write(roles_dir.join("custom.md"), "keep custom").unwrap();

    squad(tmp.path())
        .args(["init", "--refresh-roles"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Initialized"));

    assert_ne!(
        std::fs::read_to_string(roles_dir.join("manager.md")).unwrap(),
        "stale manager"
    );
    assert_eq!(
        std::fs::read_to_string(roles_dir.join("custom.md")).unwrap(),
        "keep custom"
    );
}

#[test]
fn test_autopilot_init_creates_config_and_directories() {
    let tmp = TempDir::new().unwrap();

    squad(tmp.path())
        .args(["autopilot", "init"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Initialized squad autopilot workspace.",
        ))
        .stdout(predicate::str::contains("Created config:"));

    assert!(tmp.path().join(".squad").exists());
    assert!(tmp.path().join(".squad").join("autopilot.toml").exists());
    assert!(tmp.path().join(".squad").join("autopilot").exists());
    assert!(tmp
        .path()
        .join(".squad")
        .join("roles")
        .join("generated")
        .exists());
    assert!(tmp.path().join(".squad").join("teams").exists());

    let config = std::fs::read_to_string(tmp.path().join(".squad").join("autopilot.toml")).unwrap();
    assert!(config.contains("[role_overrides]"));
    assert!(config.contains("manager = \"codex\""));
    assert!(config.contains("scientific_planner = \"codex\""));
    assert!(config.contains("literature_worker = \"codex\""));
    assert!(config.contains("claude_coding_worker = \"claude\""));
    assert!(config.contains("test_worker = \"codex\""));
    assert!(config.contains("trace_collector = \"codex\""));
    assert!(config.contains("claude = 0.20"));
    assert!(config.contains("codex = 0.80"));
    assert!(config.contains("claude_coding_only = true"));
    assert!(config.contains("codex_backfill_when_waiting_on_claude = true"));
    assert!(config.contains("gemini = 0.00"));
    assert!(config.contains("openrouter_free = 0.00"));
    assert!(config.contains("local = 0.00"));
}

#[test]
fn test_autopilot_init_does_not_overwrite_existing_config() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    let config_path = tmp.path().join(".squad").join("autopilot.toml");
    std::fs::write(&config_path, "[role_overrides]\ndocs = \"gemini\"\n").unwrap();

    squad(tmp.path())
        .args(["autopilot", "init"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Config already exists:"));

    assert_eq!(
        std::fs::read_to_string(config_path).unwrap(),
        "[role_overrides]\ndocs = \"gemini\"\n"
    );
}

#[test]
fn test_autopilot_plan_reads_prd_and_prints_status_summary() {
    let tmp = TempDir::new().unwrap();
    let prd_path = tmp.path().join("PRD.md");
    std::fs::write(
        &prd_path,
        r#"
Product Goal
Build an autopilot planner.

Implementation Task Checklist
[ ] 1. Add schema - Sequential
[ ] 2. Add docs - Parallel
[ ] 3. Wire command - Parallel

Dependencies
3 depends on 1
"#,
    )
    .unwrap();

    squad(tmp.path())
        .args(["autopilot", "plan", "PRD.md"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Autopilot plan for PRD.md"))
        .stdout(predicate::str::contains("Tasks: 3"))
        .stdout(predicate::str::contains("READY_PARALLEL: 1"))
        .stdout(predicate::str::contains("BLOCKED: 1"))
        .stdout(predicate::str::contains("SEQUENTIAL: 1"))
        .stdout(predicate::str::contains("Ready Parallel:"))
        .stdout(predicate::str::contains("- task-2 Add docs"))
        .stdout(predicate::str::contains("Blocked:"))
        .stdout(predicate::str::contains("- task-3 Wire command"))
        .stdout(predicate::str::contains("depends on: task-1"))
        .stdout(predicate::str::contains("Sequential:"))
        .stdout(predicate::str::contains("- task-1 Add schema"));
}

#[test]
fn test_autopilot_plan_requires_prd_path() {
    let tmp = TempDir::new().unwrap();

    squad(tmp.path())
        .args(["autopilot", "plan"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Usage: squad autopilot plan <PRD.md>",
        ));
}

#[test]
fn test_autopilot_launch_prints_terminal_session_plan_for_run() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    let store = Store::open(&tmp.path().join(".squad").join("messages.db")).unwrap();
    let run = store.create_autopilot_run("./PRD.md").unwrap();
    store
        .create_autopilot_agents(
            run.id,
            &[
                AutopilotAgentInput {
                    name: "Autopilot Manager".to_string(),
                    role: "manager".to_string(),
                    model_provider: "codex".to_string(),
                    skills_prompt: "Coordinate work.".to_string(),
                },
                AutopilotAgentInput {
                    name: "Inspector".to_string(),
                    role: "inspector".to_string(),
                    model_provider: "claude".to_string(),
                    skills_prompt: "Review work.".to_string(),
                },
                AutopilotAgentInput {
                    name: "Rust Backend Engineer".to_string(),
                    role: "rust_backend".to_string(),
                    model_provider: "codex".to_string(),
                    skills_prompt: "Implement Rust changes.".to_string(),
                },
            ],
        )
        .unwrap();

    squad(tmp.path())
        .args(["autopilot", "launch", "--run-id", &run.id.to_string()])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "Autopilot launch plan for run {}",
            run.id
        )))
        .stdout(predicate::str::contains("PRD: ./PRD.md"))
        .stdout(predicate::str::contains("Sessions: 3"))
        .stdout(predicate::str::contains("Persisted sessions: 3"))
        .stdout(predicate::str::contains(format!(
            "Terminal backend: {}",
            if cfg!(target_os = "macos") {
                "macos-terminal"
            } else {
                "tmux"
            }
        )))
        .stdout(predicate::str::contains(
            "- manager-r1 [manager] codex -> codex --yolo",
        ))
        .stdout(predicate::str::contains(
            "- inspector-r1 [inspector] codex -> codex --yolo",
        ))
        .stdout(predicate::str::contains(
            "- rust_backend-r1 [worker] codex -> codex --yolo",
        ))
        .stdout(predicate::str::contains(
            "inject: $squad manager manager-r1",
        ))
        .stdout(predicate::str::contains(
            "inject: $squad inspector inspector-r1",
        ))
        .stdout(predicate::str::contains(
            "inject: $squad rust_backend rust_backend-r1",
        ));

    let persisted_sessions = store.list_autopilot_terminal_sessions(run.id).unwrap();
    assert_eq!(persisted_sessions.len(), 3);
    assert_eq!(persisted_sessions[0].command, "codex --yolo");
    assert_eq!(persisted_sessions[2].command, "codex --yolo");
}

#[test]
fn test_autopilot_launch_execute_runs_tmux_commands() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    let store = Store::open(&tmp.path().join(".squad").join("messages.db")).unwrap();
    let run = store.create_autopilot_run("./PRD.md").unwrap();
    store
        .create_autopilot_agents(
            run.id,
            &[
                AutopilotAgentInput {
                    name: "Autopilot Manager".to_string(),
                    role: "manager".to_string(),
                    model_provider: "claude".to_string(),
                    skills_prompt: "Coordinate work.".to_string(),
                },
                AutopilotAgentInput {
                    name: "Rust Backend Engineer".to_string(),
                    role: "rust_backend".to_string(),
                    model_provider: "codex".to_string(),
                    skills_prompt: "Implement Rust changes.".to_string(),
                },
            ],
        )
        .unwrap();
    store
        .register_agent_with_metadata("manager", "manager", Some("codex"), Some(2))
        .unwrap();
    store
        .register_agent_with_metadata("rust_backend", "rust_backend", Some("codex"), Some(2))
        .unwrap();
    let path_env = path_with_fake_tmux(&tmp);
    let tmux_log = tmp.path().join("tmux.log");

    squad(tmp.path())
        .args([
            "autopilot",
            "launch",
            "--run-id",
            &run.id.to_string(),
            "--execute",
            "--tmux-session",
            "test-autopilot",
        ])
        .env("PATH", path_env)
        .env("TMUX_LOG", &tmux_log)
        .env("SQUAD_AUTOPILOT_ASSIGN_WAIT_SECS", "0")
        .env("SQUAD_AUTOPILOT_PROMPT_DELAY_SECS", "0")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Sequential tmux launch: launching manager",
        ))
        .stdout(predicate::str::contains(
            "Executed tmux launch: test-autopilot",
        ))
        .stdout(predicate::str::contains(
            "Attach with: tmux attach -t test-autopilot",
        ));

    let log = std::fs::read_to_string(tmux_log).unwrap();
    assert!(log.contains("has-session -t test-autopilot"));
    assert!(log.contains("new-session -d -s test-autopilot -n manager-r1"));
    assert!(log.contains("codex --yolo"));
    assert!(log.contains("send-keys -t test-autopilot:manager-r1 $squad manager manager-r1 C-m"));
    assert!(log.contains("new-window -t test-autopilot -n rust_backend-r1"));
    assert!(log.contains("codex --yolo"));
    assert!(log.contains(
        "send-keys -t test-autopilot:rust_backend-r1 $squad rust_backend rust_backend-r1 C-m"
    ));
}

#[test]
fn test_autopilot_launch_execute_runs_macos_terminal_commands() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    let store = Store::open(&tmp.path().join(".squad").join("messages.db")).unwrap();
    let run = store.create_autopilot_run("./PRD.md").unwrap();
    store
        .create_autopilot_agents(
            run.id,
            &[
                AutopilotAgentInput {
                    name: "Autopilot Manager".to_string(),
                    role: "manager".to_string(),
                    model_provider: "codex".to_string(),
                    skills_prompt: "Coordinate work.".to_string(),
                },
                AutopilotAgentInput {
                    name: "Rust Backend Engineer".to_string(),
                    role: "rust_backend".to_string(),
                    model_provider: "codex".to_string(),
                    skills_prompt: "Implement Rust changes.".to_string(),
                },
            ],
        )
        .unwrap();
    store
        .register_agent_with_metadata("manager", "manager", Some("codex"), Some(2))
        .unwrap();
    store
        .register_agent_with_metadata("rust_backend", "rust_backend", Some("codex"), Some(2))
        .unwrap();
    let path_env = path_with_fake_osascript(&tmp);
    let osascript_log = tmp.path().join("osascript.log");

    squad(tmp.path())
        .args([
            "autopilot",
            "launch",
            "--run-id",
            &run.id.to_string(),
            "--terminal-backend",
            "macos-terminal",
            "--execute",
            "--terminal-title",
            "test-autopilot",
        ])
        .env("PATH", path_env)
        .env("OSASCRIPT_LOG", &osascript_log)
        .env("SQUAD_AUTOPILOT_ASSIGN_WAIT_SECS", "0")
        .assert()
        .success()
        .stdout(predicate::str::contains("Terminal backend: macos-terminal"))
        .stdout(predicate::str::contains(
            "Sequential macOS Terminal launch: launching manager",
        ))
        .stdout(predicate::str::contains(
            "Executed macOS Terminal launch: test-autopilot",
        ))
        .stdout(predicate::str::contains(
            "Opened physical Terminal.app windows.",
        ));

    let log = std::fs::read_to_string(osascript_log).unwrap();
    assert!(log.contains("tell application \"Terminal\""));
    assert!(log.contains("test-autopilot - manager"));
    assert!(log.contains("codex --yolo"));
    assert!(log.contains("$squad manager manager"));
    assert!(log.contains("test-autopilot - rust_backend"));
    assert!(log.contains("codex --yolo"));
    assert!(log.contains("$squad rust_backend rust_backend"));
}

#[test]
fn test_autopilot_launch_execute_delivers_ready_assignments_to_squad_tasks() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    let store = Store::open(&tmp.path().join(".squad").join("messages.db")).unwrap();
    let run = store.create_autopilot_run("./PRD.md").unwrap();
    store
        .create_autopilot_agents(
            run.id,
            &[
                AutopilotAgentInput {
                    name: "Autopilot Manager".to_string(),
                    role: "manager".to_string(),
                    model_provider: "claude".to_string(),
                    skills_prompt: "Coordinate work.".to_string(),
                },
                AutopilotAgentInput {
                    name: "Rust Backend Engineer".to_string(),
                    role: "rust_backend".to_string(),
                    model_provider: "codex".to_string(),
                    skills_prompt: "Implement Rust changes.".to_string(),
                },
            ],
        )
        .unwrap();
    store
        .create_autopilot_tasks(
            run.id,
            &[TaskGraphTask {
                id: "task-1".to_string(),
                title: "Implement queue bridge".to_string(),
                description: "Create normal squad task assignments for launched agents."
                    .to_string(),
                assigned_role: Some("rust_backend".to_string()),
                status: TaskGraphStatus::ReadyParallel,
                priority: 10,
                risk_level: RiskLevel::Medium,
                acceptance_criteria: vec!["Worker receives a queued task.".to_string()],
                likely_files: Vec::new(),
                test_requirements: Vec::new(),
                depends_on: Vec::new(),
            }],
        )
        .unwrap();
    store.assign_ready_autopilot_tasks(run.id).unwrap();
    store
        .register_agent_with_metadata("manager", "manager", Some("claude"), Some(2))
        .unwrap();
    store
        .register_agent_with_metadata("rust_backend", "rust_backend", Some("codex"), Some(2))
        .unwrap();

    let path_env = path_with_fake_osascript(&tmp);
    let osascript_log = tmp.path().join("osascript.log");

    squad(tmp.path())
        .args([
            "autopilot",
            "launch",
            "--run-id",
            &run.id.to_string(),
            "--terminal-backend",
            "macos-terminal",
            "--execute",
            "--terminal-title",
            "test-autopilot",
        ])
        .env("PATH", path_env)
        .env("OSASCRIPT_LOG", &osascript_log)
        .env("SQUAD_AUTOPILOT_ASSIGN_WAIT_SECS", "0")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Sequential task delivery for rust_backend: 1 created, 0 already existed.",
        ))
        .stdout(predicate::str::contains(
            "Autopilot task delivery: 1 created, 0 already existed.",
        ));

    let store = Store::open(&tmp.path().join(".squad").join("messages.db")).unwrap();
    let tasks = store
        .list_tasks(Some("rust_backend"), Some("queued"))
        .unwrap();
    assert_eq!(tasks.len(), 1);
    assert!(tasks[0].title.contains("Implement queue bridge"));
    assert!(tasks[0].body.contains("Autopilot-Task-ID: 1"));

    let inbox = store.receive_messages("rust_backend").unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].kind, "task_assigned");
}

#[test]
fn test_autopilot_launch_requires_run_id_flag() {
    let tmp = TempDir::new().unwrap();

    squad(tmp.path())
        .args(["autopilot", "launch", "1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Usage: squad autopilot launch --run-id <id> [--execute] [--terminal-backend <tmux|macos-terminal>]",
        ));
}

#[test]
fn test_autopilot_run_creates_team_graph_sessions_and_assignments() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    let prd_path = tmp.path().join("PRD.md");
    std::fs::write(
        &prd_path,
        r#"
Product Goal
Build an autopilot runner for the Rust CLI.

Milestones
- MVP 1: Generate report

Acceptance Criteria
- Final report preserves PRD context.

Implementation Task Checklist
[ ] 1. Add schema persistence - Sequential
[ ] 2. Add CLI docs - Parallel
[ ] 3. Add tests - Parallel

Test Requirements
- cargo test
"#,
    )
    .unwrap();

    squad(tmp.path())
        .args(["autopilot", "run", "PRD.md"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Autopilot run initialized: 1"))
        .stdout(predicate::str::contains("PRD: PRD.md"))
        .stdout(predicate::str::contains("Agents:"))
        .stdout(predicate::str::contains("Tasks: 3"))
        .stdout(predicate::str::contains("Sessions planned:"))
        .stdout(predicate::str::contains("Ready tasks assigned:"))
        .stdout(predicate::str::contains("Tests recorded: 1"))
        .stdout(predicate::str::contains("Final report:"))
        .stdout(predicate::str::contains("Acceptance criteria: pending"))
        .stdout(predicate::str::contains(
            "Next: squad autopilot launch --run-id 1",
        ));

    assert!(tmp
        .path()
        .join(".squad")
        .join("teams")
        .join("autopilot.yaml")
        .exists());
    assert!(tmp
        .path()
        .join(".squad")
        .join("autopilot")
        .join("tests-run.json")
        .exists());
    let final_report_path = tmp
        .path()
        .join(".squad")
        .join("autopilot")
        .join("final-report.md");
    assert!(final_report_path.exists());
    let final_report = std::fs::read_to_string(final_report_path).unwrap();
    assert!(final_report.contains("# Squad Autopilot Final Report"));
    assert!(final_report.contains("## Product Goals"));
    assert!(final_report.contains("Build an autopilot runner for the Rust CLI."));
    assert!(final_report.contains("## Milestones"));
    assert!(final_report.contains("MVP 1: Generate report"));
    assert!(final_report.contains("## Acceptance Criteria"));
    assert!(final_report.contains("Final report preserves PRD context."));
    assert!(final_report.contains("## Test Requirements"));
    assert!(final_report.contains("cargo test"));
    assert!(final_report.contains("## PRD Tasks Completed"));
    assert!(final_report.contains("## Task Graph"));
    assert!(final_report.contains("task-1 [SEQUENTIAL] Add schema persistence"));
    assert!(final_report.contains("task-2 [READY_PARALLEL] Add CLI docs"));
    assert!(final_report.contains("## Agents Used"));
    assert!(final_report.contains("manager: Autopilot Manager (codex)"));
    assert!(final_report.contains("## Model Mix Used"));
    assert!(final_report.contains("claude 20%"));
    assert!(final_report.contains("codex 80%"));
    assert!(final_report.contains("gemini 0%"));
    assert!(final_report.contains("openrouter_free 0%"));
    assert!(final_report.contains("openrouter_cheap 0%"));
    assert!(final_report.contains("local 0%"));
    assert!(final_report.contains("## Files Changed"));
    assert!(final_report.contains("## Tests Run"));
    assert!(final_report.contains("cargo test - skipped"));
    assert!(final_report.contains("## Failures / Retries"));
    assert!(final_report.contains("## Unresolved Risks"));
    assert!(final_report.contains("acceptance criteria are still pending"));
    assert!(final_report.contains("## Final Git Diff Summary"));
    let store = Store::open(&tmp.path().join(".squad").join("messages.db")).unwrap();
    let run = store.get_autopilot_run(1).unwrap().unwrap();
    assert_eq!(run.status, "running");
    assert_eq!(
        store.list_autopilot_agents(run.id).unwrap().len() >= 3,
        true
    );
    assert_eq!(store.list_autopilot_tasks(run.id).unwrap().len(), 3);
    assert!(!store
        .list_autopilot_terminal_sessions(run.id)
        .unwrap()
        .is_empty());
    let session_count_after_run = store
        .list_autopilot_terminal_sessions(run.id)
        .unwrap()
        .len();

    squad(tmp.path())
        .args(["autopilot", "launch", "--run-id", "1"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "Persisted sessions: {session_count_after_run}"
        )));
    assert_eq!(
        store
            .list_autopilot_terminal_sessions(run.id)
            .unwrap()
            .len(),
        session_count_after_run
    );
}

#[test]
fn test_autopilot_rejects_unknown_subcommand() {
    let tmp = TempDir::new().unwrap();

    squad(tmp.path())
        .args(["autopilot", "explode"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Usage: squad autopilot <init|plan|launch|run>",
        ));
}

#[test]
fn test_join_and_agents() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "manager"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Joined as manager"));
    squad(tmp.path())
        .arg("agents")
        .assert()
        .success()
        .stdout(predicate::str::contains("manager"));
}

#[test]
fn test_join_with_role_flag() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "worker-1", "--role", "worker"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Joined as worker-1"));
}

#[test]
fn test_join_accepts_capability_metadata_flags() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();

    squad(tmp.path())
        .args([
            "join",
            "worker",
            "--role",
            "worker",
            "--client",
            "codex",
            "--protocol-version",
            "2",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Joined as worker"));

    let db = tmp.path().join(".squad").join("messages.db");
    let conn = Connection::open(db).unwrap();
    let row: (Option<String>, Option<i64>) = conn
        .query_row(
            "SELECT client_type, protocol_version FROM agents WHERE id = ?1",
            ["worker"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(row.0.as_deref(), Some("codex"));
    assert_eq!(row.1, Some(2));
}

#[test]
fn test_join_rejects_role_flag_without_value() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();

    squad(tmp.path())
        .args(["join", "worker", "--role"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--role requires a value"));
}

#[test]
fn test_join_rejects_role_flag_followed_by_another_flag() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();

    squad(tmp.path())
        .args(["join", "worker", "--role", "--client", "codex"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--role requires a value"));
}

#[test]
fn test_join_rejects_client_flag_without_value() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();

    squad(tmp.path())
        .args(["join", "worker", "--client"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--client requires a value"));
}

#[test]
fn test_join_rejects_client_flag_followed_by_another_flag() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();

    squad(tmp.path())
        .args(["join", "worker", "--client", "--protocol-version", "2"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--client requires a value"));
}

#[test]
fn test_join_rejects_protocol_flag_without_value() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();

    squad(tmp.path())
        .args(["join", "worker", "--protocol-version"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--protocol-version requires a value",
        ));
}

#[test]
fn test_join_rejects_protocol_flag_followed_by_another_flag() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();

    squad(tmp.path())
        .args(["join", "worker", "--protocol-version", "--client", "codex"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--protocol-version requires a value",
        ));
}

#[test]
fn test_agents_json_exposes_effective_capability_fields_and_fallbacks() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();

    squad(tmp.path())
        .args(["join", "legacy", "--role", "worker"])
        .assert()
        .success();
    squad(tmp.path())
        .args([
            "join",
            "modern",
            "--role",
            "worker",
            "--client",
            "gemini",
            "--protocol-version",
            "2",
        ])
        .assert()
        .success();

    let output = squad(tmp.path())
        .args(["agents", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let entries: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();

    let legacy = entries
        .iter()
        .find(|entry| entry["id"] == "legacy")
        .unwrap();
    assert_eq!(legacy["client_type_raw"], Value::Null);
    assert_eq!(legacy["protocol_version_raw"], Value::Null);
    assert_eq!(legacy["effective_client_type"], "unknown");
    assert_eq!(legacy["effective_protocol_version"], 1);
    assert_eq!(legacy["supports_task_commands"], false);
    assert_eq!(legacy["supports_json_receive"], false);

    let modern = entries
        .iter()
        .find(|entry| entry["id"] == "modern")
        .unwrap();
    assert_eq!(modern["client_type_raw"], "gemini");
    assert_eq!(modern["protocol_version_raw"], 2);
    assert_eq!(modern["effective_client_type"], "gemini");
    assert_eq!(modern["effective_protocol_version"], 2);
    assert_eq!(modern["supports_task_commands"], true);
    assert_eq!(modern["supports_json_receive"], true);
}

#[test]
fn test_agents_human_output_shows_capability_metadata_when_present() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();

    squad(tmp.path())
        .args(["join", "legacy", "--role", "worker"])
        .assert()
        .success();

    squad(tmp.path())
        .args([
            "join",
            "worker",
            "--role",
            "worker",
            "--client",
            "opencode",
            "--protocol-version",
            "2",
        ])
        .assert()
        .success();

    squad(tmp.path())
        .arg("agents")
        .assert()
        .success()
        .stdout(predicate::str::contains("client: unknown"))
        .stdout(predicate::str::contains("protocol: 1"))
        .stdout(predicate::str::contains("client: opencode"))
        .stdout(predicate::str::contains("protocol: 2"));
}

#[test]
fn test_send_and_receive() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "manager"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["send", "manager", "worker", "implement auth module"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["receive", "worker"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[from manager]"))
        .stdout(predicate::str::contains("implement auth module"))
        .stdout(predicate::str::contains(
            "After processing, run `squad receive worker --wait` to continue listening.",
        ));
}

#[test]
fn test_send_warns_when_recipient_is_stale() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "manager"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success();

    mark_agent_stale(tmp.path(), "worker");

    squad(tmp.path())
        .args(["send", "manager", "worker", "implement auth module"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Sent to worker."))
        .stdout(predicate::str::contains(
            "Warning: worker is stale (last seen 10m ago). Message was queued but may not be seen soon.",
        ));
}

#[test]
fn test_send_reads_message_from_file() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "manager"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success();

    let message_path = tmp.path().join("message.txt");
    std::fs::write(&message_path, "line 1\nline 2").unwrap();

    squad(tmp.path())
        .args(["send", "--file", "message.txt", "manager", "worker"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Sent to worker."));

    squad(tmp.path())
        .args(["receive", "worker"])
        .assert()
        .success()
        .stdout(predicate::str::contains("line 1\nline 2"));
}

#[test]
fn test_send_reads_message_from_stdin() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "manager"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["send", "--file", "-", "manager", "worker"])
        .write_stdin("line 1\nline 2")
        .assert()
        .success()
        .stdout(predicate::str::contains("Sent to worker."));

    squad(tmp.path())
        .args(["receive", "worker"])
        .assert()
        .success()
        .stdout(predicate::str::contains("line 1\nline 2"));
}

#[test]
fn test_send_broadcast() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "manager"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "worker-1"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "worker-2"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["send", "manager", "@all", "interface changed"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Broadcast to 2 agents"));

    squad(tmp.path())
        .args(["receive", "worker-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("interface changed"));
}

#[test]
fn test_send_keeps_inline_message_starting_with_file_flag() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "manager"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["send", "manager", "worker", "--file", "missing.txt"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["receive", "worker"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--file missing.txt"));
}

#[test]
fn test_broadcast_warns_about_stale_recipients() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "manager"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "worker-1"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "worker-2"])
        .assert()
        .success();

    mark_agent_stale(tmp.path(), "worker-2");

    squad(tmp.path())
        .args(["send", "manager", "@all", "interface changed"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Broadcast to 2 agents: worker-1, worker-2",
        ))
        .stdout(predicate::str::contains(
            "Warning: stale recipients: worker-2 (10m ago).",
        ));
}

#[test]
fn test_send_to_nonexistent() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "manager"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["send", "manager", "nobody", "hello"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("does not exist"));
}

#[test]
fn test_send_from_nonexistent_fails() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["send", "ghost", "worker", "hello"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("ghost does not exist"));
}

#[test]
fn test_leave() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "manager"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["leave", "manager"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "manager archived from the squad. Unread work was preserved.",
        ));
    squad(tmp.path())
        .arg("agents")
        .assert()
        .success()
        .stdout(predicate::str::contains("No agents"));
}

#[test]
fn test_leave_nonexistent_agent_fails() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["leave", "ghost"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("ghost does not exist"));
}

#[test]
fn test_leave_archived_agent_fails_with_archived_message() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["leave", "worker"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["leave", "worker"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("worker is archived"));
}

#[test]
fn test_archived_agent_errors_are_distinct_from_nonexistent() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "manager"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["leave", "worker"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["send", "manager", "worker", "hello"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("worker is archived"));

    squad(tmp.path())
        .args(["receive", "worker"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("worker is archived"));

    squad(tmp.path())
        .args(["send", "manager", "ghost", "hello"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("ghost does not exist"));
}

#[test]
fn test_help_describes_leave_as_archive() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path())
        .arg("help")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "squad leave <id>                           Archive agent",
        ));
}

#[test]
fn test_readmes_and_manager_role_describe_archive_semantics() {
    let readme = std::fs::read_to_string("README.md").unwrap();
    let readme_zh = std::fs::read_to_string("README.zh-CN.md").unwrap();
    let manager_role = std::fs::read_to_string("src/roles/manager.md").unwrap();

    assert!(readme.contains("| `squad leave <id>` | Archive agent and preserve unread work |"));
    assert!(readme_zh.contains("| `squad leave <id>` | 归档 Agent，并保留未读工作 |"));
    assert!(manager_role.contains(
        "use `squad leave <id>` to archive it, preserve any unread work, and reassign its task to another agent"
    ));
}

#[test]
fn test_agents_hides_archived_by_default_and_shows_them_with_all() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "manager"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["leave", "worker"])
        .assert()
        .success();

    squad(tmp.path())
        .arg("agents")
        .assert()
        .success()
        .stdout(predicate::str::contains("manager"))
        .stdout(predicate::str::contains("worker").not());

    squad(tmp.path())
        .args(["agents", "--all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("manager"))
        .stdout(predicate::str::contains("worker"))
        .stdout(predicate::str::contains("archived"));
}

#[test]
fn test_join_reactivates_archived_agent_with_same_id() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["leave", "worker"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Joined as worker"))
        .stdout(predicate::str::contains("worker-2").not());
}

#[test]
fn test_setup_list() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path())
        .args(["setup", "--list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("claude"))
        .stdout(predicate::str::contains("gemini"))
        .stdout(predicate::str::contains("codex"))
        .stdout(predicate::str::contains("opencode"));
}

#[test]
fn test_help_describes_wait_as_debug_only() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path())
        .arg("help")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "squad receive <id> [--wait] [--timeout N] [--json]",
        ))
        .stdout(predicate::str::contains(
            "`--wait` blocks until a message arrives",
        ))
        .stdout(predicate::str::contains(
            "squad task create <from> <to> --title <title> [--body <body>]",
        ))
        .stdout(predicate::str::contains("squad init [--refresh-roles]"))
        .stdout(predicate::str::contains("Worker checks once for tasks"));
}

#[test]
fn test_readmes_describe_receive_timeout_debug_path() {
    let readme = std::fs::read_to_string("README.md").unwrap();
    let readme_zh = std::fs::read_to_string("README.zh-CN.md").unwrap();

    assert!(readme.contains("| `squad receive <id> [--wait] [--json]` |"));
    assert!(readme.contains("| `squad task create <from> <to> --title <title> [--body <body>]` |"));
    assert!(readme.contains("| `squad task complete <agent> <task-id> --summary <text>` |"));
    assert!(readme.contains("| `squad task list [--agent <id>] [--status <status>]` |"));
    assert!(readme_zh.contains("| `squad receive <id> [--wait] [--json]` |"));
    assert!(
        readme_zh.contains("| `squad task create <from> <to> --title <title> [--body <body>]` |")
    );
    assert!(readme_zh.contains("| `squad task complete <agent> <task-id> --summary <text>` |"));
    assert!(readme_zh.contains("| `squad task list [--agent <id>] [--status <status>]` |"));
}

#[test]
fn test_receive_rejects_unknown_flag() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["receive", "worker", "--bogus"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown receive flag: --bogus"));
}

#[test]
fn test_receive_rejects_invalid_timeout_value() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["receive", "worker", "--wait", "--timeout", "nope"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid --timeout value: nope"));
}

#[test]
fn test_receive_rejects_timeout_without_wait() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["receive", "worker", "--timeout", "5"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--timeout requires --wait"));
}

#[test]
fn test_role_prompts_prefer_task_commands_with_send_receive_fallback() {
    let manager_role = std::fs::read_to_string("src/roles/manager.md").unwrap();
    let worker_role = std::fs::read_to_string("src/roles/worker.md").unwrap();
    let inspector_role = std::fs::read_to_string("src/roles/inspector.md").unwrap();

    assert!(manager_role.contains(
        "Prefer `squad task create manager <agent> --title \"<title>\" [--body \"<body>\"]`"
    ));
    assert!(manager_role.contains("keep `squad send` / `squad receive` as the fallback path"));
    assert!(worker_role.contains("Prefer `squad task ack <your-id> <task-id>` and `squad task complete <your-id> <task-id> --summary \"<summary>\"`"));
    assert!(worker_role.contains("keep `squad send` / `squad receive` as the fallback path"));
    assert!(inspector_role.contains("Prefer `squad send --task-id <task-id> --reply-to <message-id> <your-id> <worker-id> \"<specific feedback>\"`"));
    assert!(
        inspector_role.contains("keep plain `squad send` / `squad receive` as the fallback path")
    );
}

#[test]
fn test_join_freeform_role_succeeds() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "cto"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Joined as cto"))
        .stdout(predicate::str::contains("Interpret this role autonomously"))
        .stdout(predicate::str::contains("squad send"))
        .stdout(predicate::str::contains("squad receive, squad agents"))
        .stdout(predicate::str::contains("--wait").not());
}

#[test]
fn test_history() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "manager"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["send", "manager", "worker", "task 1"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["receive", "worker"])
        .assert()
        .success(); // marks as read

    // history still shows it
    squad(tmp.path())
        .arg("history")
        .assert()
        .success()
        .stdout(predicate::str::contains("task 1"));
}

#[test]
fn test_history_shows_timestamps() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "manager"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["send", "manager", "worker", "task timestamped"])
        .assert()
        .success();
    set_message_timestamp(tmp.path(), "task timestamped", 1_704_067_200);

    squad(tmp.path())
        .arg("history")
        .assert()
        .success()
        .stdout(predicate::str::contains("[2024-01-01T00:00:00Z]"))
        .stdout(predicate::str::contains("task timestamped"));
}

#[test]
fn test_history_formats_multiline_messages_readably() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "manager"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["send", "--file", "-", "manager", "worker"])
        .write_stdin("line 1\nline 2")
        .assert()
        .success();
    set_message_timestamp(tmp.path(), "line 1\nline 2", 1_704_067_200);

    squad(tmp.path())
        .arg("history")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "* [2024-01-01T00:00:00Z] manager -> worker: line 1\n  | line 2",
        ));
}

#[test]
fn test_history_filters_by_from_and_to() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    for agent in ["manager", "worker", "inspector"] {
        squad(tmp.path()).args(["join", agent]).assert().success();
    }

    squad(tmp.path())
        .args(["send", "manager", "worker", "task from manager"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["send", "worker", "manager", "done from worker"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["send", "inspector", "worker", "review from inspector"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["history", "--from", "manager"])
        .assert()
        .success()
        .stdout(predicate::str::contains("task from manager"))
        .stdout(predicate::str::contains("done from worker").not())
        .stdout(predicate::str::contains("review from inspector").not());

    squad(tmp.path())
        .args(["history", "--to", "manager"])
        .assert()
        .success()
        .stdout(predicate::str::contains("done from worker"))
        .stdout(predicate::str::contains("task from manager").not())
        .stdout(predicate::str::contains("review from inspector").not());
}

#[test]
fn test_history_positional_agent_filter_still_works() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    for agent in ["manager", "worker", "inspector"] {
        squad(tmp.path()).args(["join", agent]).assert().success();
    }

    squad(tmp.path())
        .args(["send", "manager", "worker", "task for worker"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["send", "inspector", "manager", "review for manager"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["history", "worker"])
        .assert()
        .success()
        .stdout(predicate::str::contains("task for worker"))
        .stdout(predicate::str::contains("review for manager").not());
}

#[test]
fn test_history_filters_by_since() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "manager"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["send", "manager", "worker", "old task"])
        .assert()
        .success();
    set_message_timestamp(tmp.path(), "old task", 1_704_067_200);

    squad(tmp.path())
        .args(["send", "manager", "worker", "new task"])
        .assert()
        .success();
    set_message_timestamp(tmp.path(), "new task", 1_704_067_320);

    squad(tmp.path())
        .args(["history", "--since", "2024-01-01T00:01:00Z"])
        .assert()
        .success()
        .stdout(predicate::str::contains("new task"))
        .stdout(predicate::str::contains("old task").not());
}

#[test]
fn test_history_filters_by_numeric_since() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "manager"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["send", "manager", "worker", "older task"])
        .assert()
        .success();
    set_message_timestamp(tmp.path(), "older task", 1_704_067_200);

    squad(tmp.path())
        .args(["send", "manager", "worker", "latest task"])
        .assert()
        .success();
    set_message_timestamp(tmp.path(), "latest task", 1_704_067_320);

    squad(tmp.path())
        .args(["history", "--since", "1704067260"])
        .assert()
        .success()
        .stdout(predicate::str::contains("latest task"))
        .stdout(predicate::str::contains("older task").not());
}

#[test]
fn test_receive_wait_timeout_suggests_checking_again() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["receive", "worker", "--wait", "--timeout", "0"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "No new messages (timed out after 0s). Run `squad receive worker --wait` to continue listening.",
        ));
}

#[test]
fn test_receive_wait_message_includes_continue_listening_prompt() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "manager"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["send", "manager", "worker", "queued for wait"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["receive", "worker", "--wait"])
        .assert()
        .success()
        .stdout(predicate::str::contains("queued for wait"))
        .stdout(predicate::str::contains(
            "After processing, run `squad receive worker --wait` to continue listening.",
        ));
}

#[test]
fn test_task_commands_cover_create_ack_complete_requeue_and_list() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    for agent in ["manager", "worker", "worker-2"] {
        squad(tmp.path()).args(["join", agent]).assert().success();
    }

    squad(tmp.path())
        .args([
            "task",
            "create",
            "manager",
            "worker",
            "--title",
            "auth-module",
            "--body",
            "Implement auth module",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created task"))
        .stdout(predicate::str::contains("auth-module"));

    let task_id = first_task_id(tmp.path());

    squad(tmp.path())
        .args(["task", "list", "--agent", "worker", "--status", "queued"])
        .assert()
        .success()
        .stdout(predicate::str::contains(&task_id))
        .stdout(predicate::str::contains(format!("[task {task_id}] queued")))
        .stdout(predicate::str::contains("assigned_to: worker"))
        .stdout(predicate::str::contains("lease_owner: unleased"))
        .stdout(predicate::str::contains("title: auth-module"));

    squad(tmp.path())
        .args(["task", "ack", "worker", &task_id])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("Acked task {task_id}")));

    squad(tmp.path())
        .args([
            "task",
            "complete",
            "worker",
            &task_id,
            "--summary",
            "Auth shipped",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "Completed task {task_id}"
        )))
        .stdout(predicate::str::contains("Auth shipped"));

    squad(tmp.path())
        .args(["task", "requeue", &task_id, "--to", "worker-2"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("Requeued task {task_id}")))
        .stdout(predicate::str::contains("worker-2"));

    squad(tmp.path())
        .args(["task", "list", "--agent", "worker-2", "--status", "queued"])
        .assert()
        .success()
        .stdout(predicate::str::contains(&task_id))
        .stdout(predicate::str::contains(format!("[task {task_id}] queued")))
        .stdout(predicate::str::contains("assigned_to: worker-2"))
        .stdout(predicate::str::contains("lease_owner: unleased"))
        .stdout(predicate::str::contains("title: auth-module"));
}

#[test]
fn test_receive_text_formats_task_messages_with_metadata() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    for agent in ["manager", "worker"] {
        squad(tmp.path()).args(["join", agent]).assert().success();
    }

    squad(tmp.path())
        .args([
            "task",
            "create",
            "manager",
            "worker",
            "--title",
            "auth-module",
            "--body",
            "Implement auth module",
        ])
        .assert()
        .success();

    let task_id = first_task_id(tmp.path());

    squad(tmp.path())
        .args(["receive", "worker"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "[task {task_id}] queued from manager"
        )))
        .stdout(predicate::str::contains("Title: auth-module"))
        .stdout(predicate::str::contains("Body: Implement auth module"))
        .stdout(predicate::str::contains(format!(
            "Reply: squad send --task-id {task_id} worker manager \"<your response>\""
        )));
}

#[test]
fn test_send_supports_task_id_and_reply_to_metadata() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    for agent in ["manager", "worker"] {
        squad(tmp.path()).args(["join", agent]).assert().success();
    }

    squad(tmp.path())
        .args([
            "task",
            "create",
            "manager",
            "worker",
            "--title",
            "auth-module",
            "--body",
            "Implement auth module",
        ])
        .assert()
        .success();
    let task_id = first_task_id(tmp.path());

    squad(tmp.path())
        .args(["receive", "worker"])
        .assert()
        .success();
    let assignment_message_id = message_id_by_content(tmp.path(), "auth-module");

    let output = squad(tmp.path())
        .args([
            "send",
            "--task-id",
            &task_id,
            "--reply-to",
            &assignment_message_id.to_string(),
            "worker",
            "manager",
            "Need clarification on auth edge cases",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(String::from_utf8(output)
        .unwrap()
        .contains("Sent to manager."));

    let output = squad(tmp.path())
        .args(["receive", "manager", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let lines: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let first = &lines[0];
    assert_eq!(first["kind"], "note");
    assert_eq!(first["task_id"], task_id);
    assert_eq!(first["reply_to"], assignment_message_id);
    assert_eq!(first["content"], "Need clarification on auth edge cases");
}

#[test]
fn test_receive_json_preserves_mixed_inbox_order_and_task_structure() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    for agent in ["manager", "worker"] {
        squad(tmp.path()).args(["join", agent]).assert().success();
    }

    squad(tmp.path())
        .args(["send", "manager", "worker", "legacy note before task"])
        .assert()
        .success();
    set_message_timestamp(tmp.path(), "legacy note before task", 1_704_067_200);

    squad(tmp.path())
        .args([
            "task",
            "create",
            "manager",
            "worker",
            "--title",
            "auth-module",
            "--body",
            "Implement auth module",
        ])
        .assert()
        .success();
    set_message_timestamp(tmp.path(), "auth-module", 1_704_067_200);
    let task_id = first_task_id(tmp.path());

    squad(tmp.path())
        .args(["send", "manager", "worker", "legacy note after task"])
        .assert()
        .success();
    set_message_timestamp(tmp.path(), "legacy note after task", 1_704_067_200);

    let output = squad(tmp.path())
        .args(["receive", "worker", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let entries: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();

    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0]["kind"], "note");
    assert_eq!(entries[0]["content"], "legacy note before task");

    assert_eq!(entries[1]["kind"], "task_assigned");
    assert_eq!(entries[1]["task_id"], task_id);
    assert_eq!(entries[1]["task"]["id"], task_id);
    assert_eq!(entries[1]["task"]["title"], "auth-module");
    assert_eq!(entries[1]["task"]["body"], "Implement auth module");
    assert_eq!(entries[1]["task"]["status"], "queued");

    assert_eq!(entries[2]["kind"], "note");
    assert_eq!(entries[2]["content"], "legacy note after task");
}

#[test]
fn test_receive_json_empty_inbox_emits_no_objects() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["receive", "worker", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn test_receive_wait_json_empty_inbox_emits_no_objects() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["receive", "worker", "--wait", "--timeout", "0", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

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

#[test]
fn test_setup_templates_pass_client_and_protocol_metadata_on_join() {
    let claude = command_content(PLATFORMS.iter().find(|p| p.name == "claude").unwrap());
    let codex = command_content(PLATFORMS.iter().find(|p| p.name == "codex").unwrap());
    let opencode = command_content(PLATFORMS.iter().find(|p| p.name == "opencode").unwrap());
    let gemini = command_content(PLATFORMS.iter().find(|p| p.name == "gemini").unwrap());

    assert!(claude.contains("--client claude"));
    assert!(claude.contains("--protocol-version 2"));
    assert!(codex.contains("--client codex"));
    assert!(opencode.contains("--client opencode"));
    assert!(gemini.contains("--client gemini"));
    assert!(gemini.contains("--protocol-version 2"));
}

#[test]
fn test_leave_deletes_session_file() {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["leave", "worker"])
        .assert()
        .success();
    let session_path = tmp.path().join(".squad").join("sessions").join("worker");
    assert!(!session_path.exists());
}

#[test]
fn test_doctor_reports_template_archived_task_and_protocol_warnings_without_mutating_state() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let path_env = path_with_fake_binary(&tmp, "claude");

    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args([
            "join",
            "manager",
            "--role",
            "manager",
            "--protocol-version",
            "2",
        ])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "legacy", "--role", "worker"])
        .assert()
        .success();
    squad(tmp.path())
        .args([
            "join",
            "archived-worker",
            "--role",
            "worker",
            "--protocol-version",
            "2",
        ])
        .assert()
        .success();

    squad(tmp.path())
        .args([
            "task",
            "create",
            "manager",
            "archived-worker",
            "--title",
            "pending-task",
            "--body",
            "body",
        ])
        .assert()
        .success();
    let pending_task_id = first_task_id(tmp.path());
    squad(tmp.path())
        .args(["leave", "archived-worker"])
        .assert()
        .success();

    let template_path = home.join(".claude/commands/squad.md");
    std::fs::create_dir_all(template_path.parent().unwrap()).unwrap();
    std::fs::write(
        &template_path,
        "# squad-version: 0.0.1\noutdated template\n",
    )
    .unwrap();
    let before_template = std::fs::read_to_string(&template_path).unwrap();

    let db = tmp.path().join(".squad").join("messages.db");
    let conn = Connection::open(&db).unwrap();
    let before_last_seen: Option<i64> = conn
        .query_row(
            "SELECT last_seen FROM agents WHERE id = 'legacy'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let before_status: String = conn
        .query_row(
            "SELECT status FROM tasks WHERE id = ?1",
            [&pending_task_id],
            |row| row.get(0),
        )
        .unwrap();
    drop(conn);

    squad(tmp.path())
        .arg("doctor")
        .env("HOME", &home)
        .env("PATH", &path_env)
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "WARN: slash template claude is outdated (installed=0.0.1, current={}); run squad init or squad setup",
            current_version()
        )))
        .stdout(predicate::str::contains(format!(
            "WARN: archived agent archived-worker has pending tasks: {}",
            pending_task_id
        )))
        .stdout(predicate::str::contains(
            "WARN: legacy has effective_protocol_version=1; task commands should fall back to send/receive",
        ));

    assert_eq!(
        std::fs::read_to_string(&template_path).unwrap(),
        before_template
    );
    let conn = Connection::open(&db).unwrap();
    let after_last_seen: Option<i64> = conn
        .query_row(
            "SELECT last_seen FROM agents WHERE id = 'legacy'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let after_status: String = conn
        .query_row(
            "SELECT status FROM tasks WHERE id = ?1",
            [&pending_task_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(after_last_seen, before_last_seen);
    assert_eq!(after_status, before_status);
}

#[test]
fn test_doctor_reports_clean_state_and_help_mentions_command() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let empty_bin = tmp.path().join("empty_bin");
    std::fs::create_dir_all(&empty_bin).unwrap();

    squad(tmp.path()).arg("init").assert().success();
    squad(tmp.path())
        .args([
            "join",
            "modern",
            "--role",
            "worker",
            "--client",
            "codex",
            "--protocol-version",
            "2",
        ])
        .assert()
        .success();

    squad(tmp.path())
        .arg("doctor")
        .env("HOME", &home)
        .env("PATH", empty_bin.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "OK: no installed slash templates detected",
        ))
        .stdout(predicate::str::contains(
            "OK: no archived agents with pending tasks",
        ))
        .stdout(predicate::str::contains(
            "OK: all agents meet protocol threshold",
        ));

    squad(tmp.path())
        .arg("help")
        .assert()
        .success()
        .stdout(predicate::str::contains("squad doctor"));
}
