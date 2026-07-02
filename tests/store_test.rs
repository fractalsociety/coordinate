use squad::autopilot::{
    plan_terminal_sessions, AutopilotConfig, GeneratedTeamRole, ModelProvider, RiskLevel,
    TaskGraphStatus, TaskGraphTask,
};
use squad::store::{AutopilotAgentInput, Store};
use tempfile::TempDir;

#[test]
fn test_store_init_creates_tables() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    let agents = store.list_agents(false).unwrap();
    assert!(agents.is_empty());
}

#[test]
fn test_store_init_creates_autopilot_tables() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("messages.db");
    let _store = Store::open(&db_path).unwrap();
    let conn = rusqlite::Connection::open(&db_path).unwrap();

    for table in [
        "autopilot_runs",
        "autopilot_agents",
        "autopilot_tasks",
        "autopilot_task_dependencies",
        "autopilot_reviews",
        "autopilot_terminal_sessions",
    ] {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "missing table {table}");
    }

    assert_table_has_columns(
        &conn,
        "autopilot_runs",
        &["id", "prd_path", "status", "created_at", "completed_at"],
    );
    assert_table_has_columns(
        &conn,
        "autopilot_tasks",
        &[
            "id",
            "run_id",
            "title",
            "description",
            "assigned_role",
            "assigned_agent_id",
            "status",
            "priority",
            "risk_level",
            "acceptance_criteria",
            "created_at",
            "completed_at",
        ],
    );
    assert_table_has_columns(
        &conn,
        "autopilot_terminal_sessions",
        &[
            "id",
            "run_id",
            "agent_id",
            "terminal_kind",
            "command",
            "status",
        ],
    );
}

#[test]
fn test_create_autopilot_run_persists_initial_running_record() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();

    let run = store.create_autopilot_run("./PRD.md").unwrap();

    assert!(run.id > 0);
    assert_eq!(run.prd_path, "./PRD.md");
    assert_eq!(run.status, "running");
    assert!(!run.created_at.is_empty());
    assert_eq!(run.completed_at, None);

    let persisted = store.get_autopilot_run(run.id).unwrap().unwrap();
    assert_eq!(persisted, run);
}

#[test]
fn test_create_autopilot_run_rejects_empty_prd_path() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();

    let error = store.create_autopilot_run("   ").unwrap_err().to_string();

    assert!(error.contains("autopilot PRD path cannot be empty"));
    assert_eq!(store.get_autopilot_run(1).unwrap(), None);
}

#[test]
fn test_create_autopilot_agents_persists_generated_agents_for_run() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    let run = store.create_autopilot_run("./PRD.md").unwrap();
    let inputs = vec![
        AutopilotAgentInput {
            name: "Autopilot Manager".to_string(),
            role: "manager".to_string(),
            model_provider: "claude".to_string(),
            skills_prompt: "Own PRD execution.".to_string(),
        },
        AutopilotAgentInput {
            name: "Rust Backend Engineer".to_string(),
            role: "rust_backend".to_string(),
            model_provider: "codex".to_string(),
            skills_prompt: "Implement Rust changes.".to_string(),
        },
    ];

    let agents = store.create_autopilot_agents(run.id, &inputs).unwrap();

    assert_eq!(agents.len(), 2);
    assert!(agents[0].id > 0);
    assert_eq!(agents[0].run_id, run.id);
    assert_eq!(agents[0].name, "Autopilot Manager");
    assert_eq!(agents[0].role, "manager");
    assert_eq!(agents[0].model_provider, "claude");
    assert_eq!(agents[0].skills_prompt, "Own PRD execution.");
    assert_eq!(agents[0].status, "planned");
    assert_eq!(agents[1].role, "rust_backend");

    let persisted = store.list_autopilot_agents(run.id).unwrap();
    assert_eq!(persisted, agents);
}

#[test]
fn test_create_autopilot_agents_rejects_missing_run_and_empty_fields() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    let valid_input = AutopilotAgentInput {
        name: "Autopilot Manager".to_string(),
        role: "manager".to_string(),
        model_provider: "claude".to_string(),
        skills_prompt: "Own PRD execution.".to_string(),
    };

    let missing_run_error = store
        .create_autopilot_agents(999, std::slice::from_ref(&valid_input))
        .unwrap_err()
        .to_string();
    assert!(missing_run_error.contains("autopilot run does not exist: 999"));

    let run = store.create_autopilot_run("./PRD.md").unwrap();
    let empty_list_error = store
        .create_autopilot_agents(run.id, &[])
        .unwrap_err()
        .to_string();
    assert!(empty_list_error.contains("autopilot agent list cannot be empty"));

    let invalid_input = AutopilotAgentInput {
        role: "   ".to_string(),
        ..valid_input
    };
    let empty_field_error = store
        .create_autopilot_agents(run.id, &[invalid_input])
        .unwrap_err()
        .to_string();
    assert!(empty_field_error.contains("autopilot agent role cannot be empty"));
    assert!(store.list_autopilot_agents(run.id).unwrap().is_empty());
}

#[test]
fn test_create_autopilot_terminal_sessions_persists_planned_sessions() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    let run = store.create_autopilot_run("./PRD.md").unwrap();
    let agents = store
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
    let roles = vec![
        GeneratedTeamRole {
            role_id: "manager".to_string(),
            prompt_file: "generated/manager".to_string(),
        },
        GeneratedTeamRole {
            role_id: "rust_backend".to_string(),
            prompt_file: "generated/rust_backend".to_string(),
        },
    ];
    let config = AutopilotConfig {
        role_overrides: std::collections::BTreeMap::from([
            ("manager".to_string(), ModelProvider::Claude),
            ("rust_backend".to_string(), ModelProvider::Codex),
        ]),
        ..AutopilotConfig::default()
    };
    let sessions = plan_terminal_sessions(tmp.path(), &roles, &config).unwrap();

    let records = store
        .create_autopilot_terminal_sessions(run.id, &sessions)
        .unwrap();

    assert_eq!(records.len(), 2);
    assert_eq!(records[0].run_id, run.id);
    assert_eq!(records[0].agent_id, agents[0].id);
    assert_eq!(records[0].terminal_kind, "tmux");
    assert_eq!(records[0].command, "codex --yolo");
    assert_eq!(records[0].status, "planned");
    assert_eq!(records[1].agent_id, agents[1].id);
    assert_eq!(records[1].command, "codex --yolo");

    let persisted = store.list_autopilot_terminal_sessions(run.id).unwrap();
    assert_eq!(persisted, records);

    let second_records = store
        .create_autopilot_terminal_sessions(run.id, &sessions)
        .unwrap();
    assert_eq!(second_records, records);
    assert_eq!(
        store
            .list_autopilot_terminal_sessions(run.id)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn test_create_autopilot_terminal_sessions_rejects_missing_agent() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    let run = store.create_autopilot_run("./PRD.md").unwrap();
    let roles = vec![GeneratedTeamRole {
        role_id: "manager".to_string(),
        prompt_file: "generated/manager".to_string(),
    }];
    let sessions = plan_terminal_sessions(tmp.path(), &roles, &AutopilotConfig::default()).unwrap();

    let error = store
        .create_autopilot_terminal_sessions(run.id, &sessions)
        .unwrap_err()
        .to_string();

    assert!(error.contains("has no persisted agent"));
    assert!(store
        .list_autopilot_terminal_sessions(run.id)
        .unwrap()
        .is_empty());
}

#[test]
fn test_create_autopilot_tasks_persists_extracted_task_graph_rows() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    let run = store.create_autopilot_run("./PRD.md").unwrap();
    let mut tasks = vec![
        task_graph_task(
            "task-1",
            "Create schema",
            TaskGraphStatus::Sequential,
            RiskLevel::High,
            Some("sqlite_engineer"),
        ),
        task_graph_task(
            "task-2",
            "Update docs",
            TaskGraphStatus::ReadyParallel,
            RiskLevel::Low,
            None,
        ),
    ];
    tasks[1].depends_on = vec!["task-1".to_string()];

    let records = store.create_autopilot_tasks(run.id, &tasks).unwrap();

    assert_eq!(records.len(), 2);
    assert!(records[0].id > 0);
    assert_eq!(records[0].run_id, run.id);
    assert_eq!(records[0].title, "Create schema");
    assert_eq!(records[0].description, "Implement Create schema");
    assert_eq!(records[0].assigned_role.as_deref(), Some("sqlite_engineer"));
    assert_eq!(records[0].assigned_agent_id, None);
    assert_eq!(records[0].status, "SEQUENTIAL");
    assert_eq!(records[0].priority, 10);
    assert_eq!(records[0].risk_level.as_deref(), Some("high"));
    assert_eq!(
        records[0].acceptance_criteria,
        vec!["Create schema is complete".to_string()]
    );
    assert!(!records[0].created_at.is_empty());
    assert_eq!(records[0].completed_at, None);
    assert_eq!(records[1].status, "READY_PARALLEL");
    assert_eq!(records[1].risk_level.as_deref(), Some("low"));

    let persisted = store.list_autopilot_tasks(run.id).unwrap();
    assert_eq!(persisted, records);
    let dependencies = store.list_autopilot_task_dependencies(run.id).unwrap();
    assert_eq!(dependencies.len(), 1);
    assert_eq!(dependencies[0].task_id, records[1].id);
    assert_eq!(dependencies[0].depends_on_task_id, records[0].id);
}

#[test]
fn test_create_autopilot_tasks_rejects_missing_run_empty_list_and_invalid_graph() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    let valid_task = task_graph_task(
        "task-1",
        "Create schema",
        TaskGraphStatus::Sequential,
        RiskLevel::Medium,
        None,
    );

    let missing_run_error = store
        .create_autopilot_tasks(999, std::slice::from_ref(&valid_task))
        .unwrap_err()
        .to_string();
    assert!(missing_run_error.contains("autopilot run does not exist: 999"));

    let run = store.create_autopilot_run("./PRD.md").unwrap();
    let empty_list_error = store
        .create_autopilot_tasks(run.id, &[])
        .unwrap_err()
        .to_string();
    assert!(empty_list_error.contains("autopilot task list cannot be empty"));

    let invalid_task = TaskGraphTask {
        title: "   ".to_string(),
        ..valid_task
    };
    let empty_field_error = store
        .create_autopilot_tasks(run.id, &[invalid_task])
        .unwrap_err()
        .to_string();
    assert!(empty_field_error.contains("autopilot task title cannot be empty"));
    assert!(store.list_autopilot_tasks(run.id).unwrap().is_empty());

    let missing_dependency = TaskGraphTask {
        depends_on: vec!["task-999".to_string()],
        ..task_graph_task(
            "task-2",
            "Update docs",
            TaskGraphStatus::Blocked,
            RiskLevel::Medium,
            None,
        )
    };
    let missing_dependency_error = store
        .create_autopilot_tasks(run.id, &[missing_dependency])
        .unwrap_err()
        .to_string();
    assert!(missing_dependency_error.contains("depends on missing task 'task-999'"));
    assert!(store.list_autopilot_tasks(run.id).unwrap().is_empty());
    assert!(store
        .list_autopilot_task_dependencies(run.id)
        .unwrap()
        .is_empty());
}

#[test]
fn test_ready_autopilot_tasks_returns_unassigned_tasks_with_completed_dependencies() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    let run = store.create_autopilot_run("./PRD.md").unwrap();
    let mut completed_dependency = task_graph_task(
        "task-1",
        "Complete foundation",
        TaskGraphStatus::Done,
        RiskLevel::Low,
        None,
    );
    completed_dependency.priority = 1;
    let mut dependency_ready = task_graph_task(
        "task-2",
        "Use completed dependency",
        TaskGraphStatus::ReadyParallel,
        RiskLevel::Medium,
        None,
    );
    dependency_ready.priority = 20;
    dependency_ready.depends_on = vec!["task-1".to_string()];
    let mut blocked_by_ready_task = task_graph_task(
        "task-3",
        "Wait for dependency",
        TaskGraphStatus::ReadyParallel,
        RiskLevel::Medium,
        None,
    );
    blocked_by_ready_task.depends_on = vec!["task-2".to_string()];
    let mut sequential = task_graph_task(
        "task-4",
        "Sequential fallback",
        TaskGraphStatus::Sequential,
        RiskLevel::Medium,
        None,
    );
    sequential.priority = 30;
    let review = task_graph_task(
        "task-5",
        "Needs review",
        TaskGraphStatus::ReviewRequired,
        RiskLevel::High,
        None,
    );
    store
        .create_autopilot_tasks(
            run.id,
            &[
                completed_dependency,
                dependency_ready,
                blocked_by_ready_task,
                sequential,
                review,
            ],
        )
        .unwrap();

    let ready = store.ready_autopilot_tasks(run.id).unwrap();

    assert_eq!(ready.len(), 2);
    assert_eq!(ready[0].title, "Sequential fallback");
    assert_eq!(ready[1].title, "Use completed dependency");

    let missing_run_error = store.ready_autopilot_tasks(999).unwrap_err().to_string();
    assert!(missing_run_error.contains("autopilot run does not exist: 999"));
}

#[test]
fn test_autopilot_launch_blockers_and_status_counts_monitor_dependencies() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    let run = store.create_autopilot_run("./PRD.md").unwrap();
    let mut dependency = task_graph_task(
        "task-1",
        "Complete foundation",
        TaskGraphStatus::Sequential,
        RiskLevel::Medium,
        None,
    );
    dependency.priority = 1;
    let mut blocked = task_graph_task(
        "task-2",
        "Launch dependent work",
        TaskGraphStatus::ReadyParallel,
        RiskLevel::Medium,
        None,
    );
    blocked.depends_on = vec!["task-1".to_string()];
    let failed = task_graph_task(
        "task-3",
        "Failed work",
        TaskGraphStatus::Failed,
        RiskLevel::High,
        None,
    );
    let records = store
        .create_autopilot_tasks(run.id, &[dependency, blocked, failed])
        .unwrap();

    let blockers = store.autopilot_task_launch_blockers(records[1].id).unwrap();
    assert_eq!(blockers.len(), 1);
    assert!(blockers[0].contains("dependency"));
    assert!(blockers[0].contains("SEQUENTIAL"));

    let counts = store.autopilot_task_status_counts(run.id).unwrap();
    assert_eq!(counts.ready_parallel, 1);
    assert_eq!(counts.sequential, 1);
    assert_eq!(counts.failed, 1);
    assert_eq!(counts.done, 0);
}

#[test]
fn test_assign_ready_autopilot_tasks_assigns_role_specific_and_open_tasks_to_workers() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    let run = store.create_autopilot_run("./PRD.md").unwrap();
    let agents = store
        .create_autopilot_agents(
            run.id,
            &[
                autopilot_agent("Autopilot Manager", "manager"),
                autopilot_agent("Autopilot Inspector", "inspector"),
                autopilot_agent("Rust Backend Engineer", "rust_backend"),
                autopilot_agent("Docs Engineer", "docs"),
            ],
        )
        .unwrap();
    let mut rust_task = task_graph_task(
        "task-1",
        "Implement scheduler",
        TaskGraphStatus::ReadyParallel,
        RiskLevel::Medium,
        Some("rust_backend"),
    );
    rust_task.priority = 20;
    let open_task = task_graph_task(
        "task-2",
        "Write assignment notes",
        TaskGraphStatus::ReadyParallel,
        RiskLevel::Low,
        None,
    );
    store
        .create_autopilot_tasks(run.id, &[rust_task, open_task])
        .unwrap();

    let assigned = store.assign_ready_autopilot_tasks(run.id).unwrap();

    assert_eq!(assigned.len(), 2);
    assert_eq!(assigned[0].title, "Implement scheduler");
    assert_eq!(assigned[0].assigned_role.as_deref(), Some("rust_backend"));
    assert_eq!(assigned[0].assigned_agent_id, Some(agents[2].id));
    assert_eq!(assigned[1].title, "Write assignment notes");
    assert_eq!(assigned[1].assigned_role.as_deref(), Some("docs"));
    assert_eq!(assigned[1].assigned_agent_id, Some(agents[3].id));
    assert!(store.ready_autopilot_tasks(run.id).unwrap().is_empty());
}

#[test]
fn test_assign_ready_autopilot_tasks_rejects_missing_worker_role() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    let run = store.create_autopilot_run("./PRD.md").unwrap();
    store
        .create_autopilot_agents(run.id, &[autopilot_agent("Docs Engineer", "docs")])
        .unwrap();
    let task = task_graph_task(
        "task-1",
        "Implement scheduler",
        TaskGraphStatus::ReadyParallel,
        RiskLevel::Medium,
        Some("rust_backend"),
    );
    store.create_autopilot_tasks(run.id, &[task]).unwrap();

    let error = store
        .assign_ready_autopilot_tasks(run.id)
        .unwrap_err()
        .to_string();

    assert!(error.contains("has no worker for role 'rust_backend'"));
}

#[test]
fn test_adaptive_assignment_routes_small_coding_to_claude_and_broad_work_to_codex() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    let run = store.create_autopilot_run("./PRD.md").unwrap();
    let agents = store
        .create_autopilot_agents(
            run.id,
            &[
                AutopilotAgentInput {
                    model_provider: "claude".to_string(),
                    ..autopilot_agent("Claude Small Coding Worker", "claude_coding_worker")
                },
                AutopilotAgentInput {
                    model_provider: "codex".to_string(),
                    ..autopilot_agent("Codex Worker", "coding_worker")
                },
            ],
        )
        .unwrap();
    let mut small_coding = task_graph_task(
        "task-1",
        "Implement parser guard",
        TaskGraphStatus::ReadyParallel,
        RiskLevel::Low,
        None,
    );
    small_coding.description = "Implement one guard in src/parser.rs and add one test.".to_string();
    small_coding.acceptance_criteria = vec!["invalid input is rejected".to_string()];
    let broad_docs = task_graph_task(
        "task-2",
        "Write architecture docs summary",
        TaskGraphStatus::ReadyParallel,
        RiskLevel::Low,
        None,
    );
    store
        .create_autopilot_tasks(run.id, &[small_coding, broad_docs])
        .unwrap();

    let assigned = store.assign_ready_autopilot_tasks(run.id).unwrap();

    assert_eq!(assigned.len(), 2);
    assert_eq!(assigned[0].title, "Implement parser guard");
    assert_eq!(assigned[0].assigned_agent_id, Some(agents[0].id));
    assert_eq!(
        assigned[0].assigned_role.as_deref(),
        Some("claude_coding_worker")
    );
    assert_eq!(assigned[1].title, "Write architecture docs summary");
    assert_eq!(assigned[1].assigned_agent_id, Some(agents[1].id));
    assert_eq!(assigned[1].assigned_role.as_deref(), Some("coding_worker"));
}

#[test]
fn test_adaptive_assignment_reassigns_too_large_claude_role_task_to_codex() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    let run = store.create_autopilot_run("./PRD.md").unwrap();
    let agents = store
        .create_autopilot_agents(
            run.id,
            &[
                AutopilotAgentInput {
                    model_provider: "claude".to_string(),
                    ..autopilot_agent("Claude Small Coding Worker", "claude_coding_worker")
                },
                AutopilotAgentInput {
                    model_provider: "codex".to_string(),
                    ..autopilot_agent("Codex Worker", "coding_worker")
                },
            ],
        )
        .unwrap();
    let mut broad = task_graph_task(
        "task-1",
        "Review architecture and summarize PRD",
        TaskGraphStatus::ReadyParallel,
        RiskLevel::Medium,
        Some("claude_coding_worker"),
    );
    broad.description =
        "Review the full architecture, summarize the PRD, and write docs.".to_string();
    broad.acceptance_criteria = vec![
        "architecture reviewed".to_string(),
        "PRD summarized".to_string(),
        "docs updated".to_string(),
        "risks listed".to_string(),
    ];
    store.create_autopilot_tasks(run.id, &[broad]).unwrap();

    let assigned = store.assign_ready_autopilot_tasks(run.id).unwrap();

    assert_eq!(assigned.len(), 1);
    assert_eq!(assigned[0].assigned_agent_id, Some(agents[1].id));
    assert_eq!(
        assigned[0].assigned_role.as_deref(),
        Some("claude_coding_worker")
    );
}

#[test]
fn test_autopilot_review_lifecycle_accepts_and_completes_task() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    let run = store.create_autopilot_run("./PRD.md").unwrap();
    let agents = store
        .create_autopilot_agents(
            run.id,
            &[
                autopilot_agent("Inspector", "inspector"),
                autopilot_agent("Rust Backend Engineer", "rust_backend"),
            ],
        )
        .unwrap();
    let task = task_graph_task(
        "task-1",
        "Implement review flow",
        TaskGraphStatus::ReadyParallel,
        RiskLevel::Medium,
        Some("rust_backend"),
    );
    let task = store
        .create_autopilot_tasks(run.id, &[task])
        .unwrap()
        .remove(0);
    let assigned = store
        .assign_ready_autopilot_tasks(run.id)
        .unwrap()
        .remove(0);
    assert_eq!(assigned.assigned_agent_id, Some(agents[1].id));

    let review_task = store.submit_autopilot_task_for_review(task.id).unwrap();
    assert_eq!(review_task.status, "REVIEW_REQUIRED");
    assert!(review_task.completed_at.is_some());

    let accepted = store
        .accept_autopilot_task_result(task.id, Some(agents[0].id), Some("looks good"))
        .unwrap();

    assert_eq!(accepted.status, "DONE");
    let reviews = store.list_autopilot_reviews(task.id).unwrap();
    assert_eq!(reviews.len(), 1);
    assert_eq!(reviews[0].verdict, "accepted");
    assert_eq!(reviews[0].notes.as_deref(), Some("looks good"));
    assert!(store.autopilot_run_acceptance_satisfied(run.id).unwrap());
    let completed_run = store
        .complete_autopilot_run_if_accepted(run.id)
        .unwrap()
        .unwrap();
    assert_eq!(completed_run.status, "completed");
    assert!(completed_run.completed_at.is_some());
    let persisted_run = store.get_autopilot_run(run.id).unwrap().unwrap();
    assert_eq!(persisted_run.status, "completed");
    assert!(persisted_run.completed_at.is_some());
}

#[test]
fn test_autopilot_review_reject_requeue_and_promote_failed_task() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    let run = store.create_autopilot_run("./PRD.md").unwrap();
    let agents = store
        .create_autopilot_agents(
            run.id,
            &[
                AutopilotAgentInput {
                    model_provider: "claude".to_string(),
                    ..autopilot_agent("Inspector", "inspector")
                },
                AutopilotAgentInput {
                    model_provider: "local".to_string(),
                    ..autopilot_agent("Local Worker", "local_worker")
                },
                AutopilotAgentInput {
                    model_provider: "codex".to_string(),
                    ..autopilot_agent("Codex Worker", "codex_worker")
                },
            ],
        )
        .unwrap();
    let task = task_graph_task(
        "task-1",
        "Implement retry flow",
        TaskGraphStatus::ReadyParallel,
        RiskLevel::Medium,
        Some("local_worker"),
    );
    let task = store
        .create_autopilot_tasks(run.id, &[task])
        .unwrap()
        .remove(0);
    store.assign_ready_autopilot_tasks(run.id).unwrap();
    store.submit_autopilot_task_for_review(task.id).unwrap();

    let failed = store
        .reject_autopilot_task_result(task.id, Some(agents[0].id), Some("needs fixes"))
        .unwrap();
    assert_eq!(failed.status, "FAILED");
    assert!(!store.autopilot_run_acceptance_satisfied(run.id).unwrap());

    let requeued = store.requeue_failed_autopilot_task(task.id).unwrap();
    assert_eq!(requeued.status, "READY_PARALLEL");
    assert_eq!(requeued.assigned_agent_id, None);

    let reassigned = store
        .assign_ready_autopilot_tasks(run.id)
        .unwrap()
        .remove(0);
    store
        .submit_autopilot_task_for_review(reassigned.id)
        .unwrap();
    store
        .reject_autopilot_task_result(task.id, Some(agents[0].id), Some("promote"))
        .unwrap();
    let promoted = store.promote_failed_autopilot_task(task.id).unwrap();
    assert_eq!(promoted.status, "READY_PARALLEL");
    assert_eq!(promoted.assigned_agent_id, Some(agents[2].id));
    assert_eq!(promoted.assigned_role.as_deref(), Some("codex_worker"));
}

fn autopilot_agent(name: &str, role: &str) -> AutopilotAgentInput {
    AutopilotAgentInput {
        name: name.to_string(),
        role: role.to_string(),
        model_provider: "codex".to_string(),
        skills_prompt: format!("Operate as {role}."),
    }
}

fn task_graph_task(
    id: &str,
    title: &str,
    status: TaskGraphStatus,
    risk_level: RiskLevel,
    assigned_role: Option<&str>,
) -> TaskGraphTask {
    TaskGraphTask {
        id: id.to_string(),
        title: title.to_string(),
        description: format!("Implement {title}"),
        assigned_role: assigned_role.map(str::to_string),
        status,
        priority: 10,
        risk_level,
        acceptance_criteria: vec![format!("{title} is complete")],
        likely_files: Vec::new(),
        test_requirements: Vec::new(),
        depends_on: Vec::new(),
    }
}

fn assert_table_has_columns(conn: &rusqlite::Connection, table: &str, expected: &[&str]) {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    let columns: Vec<String> = stmt
        .query_map([], |row| row.get(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    for column in expected {
        assert!(
            columns.contains(&column.to_string()),
            "missing {table}.{column}"
        );
    }
}

#[test]
fn test_store_open_migrates_legacy_message_schema_without_reset() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("messages.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE agents (
            id TEXT PRIMARY KEY,
            role TEXT NOT NULL,
            joined_at INTEGER NOT NULL
        );
        CREATE TABLE messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            from_agent TEXT NOT NULL,
            to_agent TEXT NOT NULL,
            content TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            read INTEGER NOT NULL DEFAULT 0
        );
        INSERT INTO agents (id, role, joined_at) VALUES ('worker', 'worker', 100);
        INSERT INTO messages (from_agent, to_agent, content, created_at, read)
        VALUES ('manager', 'worker', 'legacy note', 101, 0);",
    )
    .unwrap();
    drop(conn);

    let store = Store::open(&db_path).unwrap();
    let agents = store.list_agents(false).unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].id, "worker");

    let messages = store.receive_messages("worker").unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content, "legacy note");
    assert_eq!(messages[0].kind, "note");
    assert_eq!(messages[0].task_id, None);
    assert_eq!(messages[0].reply_to, None);

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let autopilot_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name LIKE 'autopilot_%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(autopilot_count, 6);
}

#[test]
fn test_register_and_list_agent() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    store.register_agent("manager", "manager").unwrap();
    let agents = store.list_agents(false).unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].id, "manager");
}

#[test]
fn test_store_open_migrates_agent_capability_columns() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("messages.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE agents (
            id TEXT PRIMARY KEY,
            role TEXT NOT NULL,
            joined_at INTEGER NOT NULL,
            session_token TEXT,
            last_seen INTEGER,
            status TEXT NOT NULL DEFAULT 'active',
            archived_at INTEGER
        );
        CREATE TABLE messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            from_agent TEXT NOT NULL,
            to_agent TEXT NOT NULL,
            content TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            read INTEGER NOT NULL DEFAULT 0
        );",
    )
    .unwrap();
    drop(conn);

    let _store = Store::open(&db_path).unwrap();
    let conn = rusqlite::Connection::open(&db_path).unwrap();

    let mut stmt = conn.prepare("PRAGMA table_info(agents)").unwrap();
    let columns: Vec<String> = stmt
        .query_map([], |row| row.get(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    assert!(columns.contains(&"client_type".to_string()));
    assert!(columns.contains(&"protocol_version".to_string()));
}

#[test]
fn test_unregister_agent() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    store.register_agent("worker", "worker").unwrap();
    store.unregister_agent("worker").unwrap();
    assert!(store.list_agents(false).unwrap().is_empty());
}

#[test]
fn test_unregister_agent_fails_when_update_affects_zero_rows() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    store.register_agent("worker", "worker").unwrap();

    store.unregister_agent("worker").unwrap();

    let result = store.unregister_agent("worker");
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("worker is archived"));
}

#[test]
fn test_unregister_archives_agent_and_rejoin_preserves_unread_messages() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    store.register_agent("manager", "manager").unwrap();
    let (joined_id, _) = store.register_agent_unique("worker", "worker").unwrap();
    assert_eq!(joined_id, "worker");

    store
        .send_message_checked("manager", "worker", "task-before-leave")
        .unwrap();
    store.unregister_agent("worker").unwrap();

    assert!(!store.agent_exists("worker").unwrap());
    let active_agents = store.list_agents(false).unwrap();
    assert_eq!(active_agents.len(), 1);
    assert_eq!(active_agents[0].id, "manager");

    let (rejoined_id, _) = store.register_agent_unique("worker", "worker").unwrap();
    assert_eq!(rejoined_id, "worker");

    let messages = store.receive_messages("worker").unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content, "task-before-leave");
}

#[test]
fn test_archived_agents_do_not_receive_suffixes_for_new_active_agents() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    let (first_id, _) = store.register_agent_unique("worker", "worker").unwrap();
    assert_eq!(first_id, "worker");

    store.unregister_agent("worker").unwrap();

    let (rejoined_id, _) = store.register_agent_unique("worker", "worker").unwrap();
    assert_eq!(rejoined_id, "worker");

    let (suffixed_id, _) = store.register_agent_unique("worker", "worker").unwrap();
    assert_eq!(suffixed_id, "worker-2");
}

#[test]
fn test_archived_suffix_is_reused_before_allocating_new_suffix() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();

    let (first_id, _) = store.register_agent_unique("worker", "worker").unwrap();
    let (second_id, _) = store.register_agent_unique("worker", "worker").unwrap();
    assert_eq!(first_id, "worker");
    assert_eq!(second_id, "worker-2");

    store.unregister_agent("worker-2").unwrap();

    let (reused_id, _) = store.register_agent_unique("worker", "worker").unwrap();
    assert_eq!(reused_id, "worker-2");
}

#[test]
fn test_send_and_receive_messages() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    store.register_agent("manager", "manager").unwrap();
    store.register_agent("worker", "worker").unwrap();

    store
        .send_message("manager", "worker", "implement auth module")
        .unwrap();
    store
        .send_message("manager", "worker", "also add tests")
        .unwrap();

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
fn test_send_to_archived_agent_fails() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    store.register_agent("manager", "manager").unwrap();
    store.register_agent("worker", "worker").unwrap();
    store.unregister_agent("worker").unwrap();

    let result = store.send_message_checked("manager", "worker", "hello");
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("worker is archived"));
}

#[test]
fn test_archived_agent_rejects_heartbeat_and_inbox_checks() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    store.register_agent("manager", "manager").unwrap();
    store.register_agent("worker", "worker").unwrap();
    store
        .send_message_checked("manager", "worker", "hello")
        .unwrap();
    store.unregister_agent("worker").unwrap();

    let heartbeat = store.touch_agent("worker");
    assert!(heartbeat.is_err());
    assert!(heartbeat
        .unwrap_err()
        .to_string()
        .contains("worker is archived"));

    let unread = store.has_unread_messages("worker");
    assert!(unread.is_err());
    assert!(unread
        .unwrap_err()
        .to_string()
        .contains("worker is archived"));

    let receive = store.receive_messages("worker");
    assert!(receive.is_err());
    assert!(receive
        .unwrap_err()
        .to_string()
        .contains("worker is archived"));
}

#[test]
fn test_broadcast_message() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    store.register_agent("manager", "manager").unwrap();
    store.register_agent("worker-1", "worker").unwrap();
    store.register_agent("worker-2", "worker").unwrap();

    let recipients = store
        .broadcast_message("manager", "code interface changed")
        .unwrap();
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
fn test_broadcast_skips_archived_agents() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    store.register_agent("manager", "manager").unwrap();
    store.register_agent("worker-1", "worker").unwrap();
    store.register_agent("worker-2", "worker").unwrap();
    store.unregister_agent("worker-2").unwrap();

    let recipients = store
        .broadcast_message("manager", "code interface changed")
        .unwrap();
    assert_eq!(recipients, vec!["worker-1".to_string()]);

    let msgs1 = store.receive_messages("worker-1").unwrap();
    assert_eq!(msgs1.len(), 1);

    let msgs2 = store.receive_messages("worker-2");
    assert!(msgs2.is_err());
    assert!(msgs2
        .unwrap_err()
        .to_string()
        .contains("worker-2 is archived"));
}

#[test]
fn test_has_unread_messages() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    store.register_agent("worker", "worker").unwrap();

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
    store.register_agent("worker", "worker").unwrap();

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
fn test_plain_messages_default_to_note_metadata() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    store.register_agent("worker", "worker").unwrap();

    store.send_message("manager", "worker", "task 1").unwrap();

    let messages = store.all_messages(None).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].kind, "note");
    assert_eq!(messages[0].task_id, None);
    assert_eq!(messages[0].reply_to, None);
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
fn test_register_agent_returns_session_token() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    let token = store.register_agent("worker", "worker").unwrap();
    assert!(!token.is_empty());
    assert_eq!(token.len(), 36); // UUID v4 format: 8-4-4-4-12
}

#[test]
fn test_multiple_agents_same_role() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    store.register_agent("worker-1", "worker").unwrap();
    store.register_agent("worker-2", "worker").unwrap();

    let agents = store.list_agents(false).unwrap();
    assert_eq!(agents.len(), 2);

    store.send_message("manager", "worker-1", "task A").unwrap();
    store.send_message("manager", "worker-2", "task B").unwrap();

    let msgs1 = store.receive_messages("worker-1").unwrap();
    assert_eq!(msgs1[0].content, "task A");

    let msgs2 = store.receive_messages("worker-2").unwrap();
    assert_eq!(msgs2[0].content, "task B");
}

#[test]
fn test_archived_agent_pending_task_diagnostics_include_assignee_and_lease_owner_matches_once_per_agent(
) {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    store.register_agent("manager", "manager").unwrap();
    store.register_agent("archived-worker", "worker").unwrap();

    let queued_task = store
        .create_task("manager", "archived-worker", "queued", "queued body")
        .unwrap();
    let acked_task = store
        .create_task("manager", "archived-worker", "acked", "acked body")
        .unwrap();
    store.ack_task("archived-worker", &acked_task).unwrap();
    let completed_task = store
        .create_task("manager", "archived-worker", "done", "done body")
        .unwrap();
    store.ack_task("archived-worker", &completed_task).unwrap();
    store
        .complete_task("archived-worker", &completed_task, "done")
        .unwrap();

    store.unregister_agent("archived-worker").unwrap();

    let warnings = store.archived_agents_with_pending_tasks().unwrap();

    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].0, "archived-worker");
    assert_eq!(warnings[0].1, vec![queued_task, acked_task]);
}

#[test]
fn test_archived_agent_pending_task_diagnostics_ignore_active_agents_and_completed_tasks() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    store.register_agent("manager", "manager").unwrap();
    store.register_agent("active-worker", "worker").unwrap();

    let task_id = store
        .create_task("manager", "active-worker", "queued", "queued body")
        .unwrap();

    let warnings = store.archived_agents_with_pending_tasks().unwrap();

    assert!(warnings.is_empty());
    assert_eq!(store.get_task(&task_id).unwrap().unwrap().status, "queued");
}

#[test]
fn test_protocol_diagnostics_use_effective_version_for_active_agents_only() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    store
        .register_agent_with_metadata("legacy-null", "worker", Some("codex"), None)
        .unwrap();
    store
        .register_agent_with_metadata("legacy-one", "worker", Some("claude"), Some(1))
        .unwrap();
    store
        .register_agent_with_metadata("modern", "worker", Some("opencode"), Some(2))
        .unwrap();
    store
        .register_agent_with_metadata("archived-legacy", "worker", Some("gemini"), Some(1))
        .unwrap();
    store.unregister_agent("archived-legacy").unwrap();

    let warnings = store.active_agents_below_protocol(2, 1).unwrap();

    assert_eq!(
        warnings,
        vec![
            ("legacy-null".to_string(), 1),
            ("legacy-one".to_string(), 1),
        ]
    );
}
