use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use serde_json::json;
use squad::service::router;
use squad::store::{
    normalize_service_provider_lane, ServiceTaskInput, ServiceTaskRecord, ServiceTaskReportInput,
    ServiceWorkerInput, Store, DEFAULT_SERVICE_CLAIM_LEASE_SECS, DEFAULT_SERVICE_LANE_ESCAPE_SECS,
};
use std::thread;
use tempfile::TempDir;
use tower::ServiceExt;

#[test]
fn service_store_creates_http_tables() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("messages.db");
    let _store = Store::open(&db_path).unwrap();
    let conn = rusqlite::Connection::open(&db_path).unwrap();

    for table in [
        "service_workers",
        "service_tasks",
        "service_task_reports",
        "service_prds",
        "service_events",
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
}

fn register_service_worker(
    store: &Store,
    id: &str,
    kind: &str,
) -> squad::store::ServiceWorkerRecord {
    store
        .service_register_worker(ServiceWorkerInput {
            id: id.to_string(),
            kind: kind.to_string(),
            role: "coding_worker".to_string(),
            status: None,
            capacity: Some(1),
            metadata: None,
        })
        .unwrap()
}

#[test]
fn service_store_accepts_cursor_worker_and_task_provider() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();

    let worker = register_service_worker(&store, "cursor-1", " Cursor ");
    assert_eq!(worker.kind, "cursor");

    let task = create_service_task(&store, "Cursor task", "CURSOR", 1);
    assert_eq!(task.preferred_model, "cursor");
}

#[test]
fn service_store_rejects_unknown_worker_kind_with_cursor_in_allowed_list() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();

    let error = store
        .service_register_worker(ServiceWorkerInput {
            id: "unknown-1".to_string(),
            kind: "unknown".to_string(),
            role: "coding_worker".to_string(),
            status: None,
            capacity: Some(1),
            metadata: None,
        })
        .unwrap_err()
        .to_string();

    assert!(error.contains("Expected codex, claude, cursor"));
}

fn create_service_task(
    store: &Store,
    title: &str,
    preferred_model: &str,
    priority: i64,
) -> ServiceTaskRecord {
    store
        .service_create_task(ServiceTaskInput {
            title: title.to_string(),
            description: "Implement a focused queue task".to_string(),
            acceptance_criteria: vec!["task is claimable".to_string()],
            source_prd_path: "PULL_QUEUE.md".to_string(),
            source_task_number: None,
            priority,
            preferred_model: preferred_model.to_string(),
            role: "coding_worker".to_string(),
            parallelizable: true,
            max_attempts: None,
            dependencies: None,
        })
        .unwrap()
}

#[test]
fn service_store_runs_worker_task_lifecycle_and_redacts_reports() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    let worker = store
        .service_register_worker(ServiceWorkerInput {
            id: "codex-1".to_string(),
            kind: "codex".to_string(),
            role: "coding_worker".to_string(),
            status: None,
            capacity: Some(1),
            metadata: None,
        })
        .unwrap();
    assert_eq!(worker.status, "ready");

    let task = store
        .service_create_task(ServiceTaskInput {
            title: "Add health route".to_string(),
            description: "Implement health route".to_string(),
            acceptance_criteria: vec!["GET /health returns ok".to_string()],
            source_prd_path: "PRD.md".to_string(),
            source_task_number: Some("4".to_string()),
            priority: 1,
            preferred_model: "codex".to_string(),
            role: "coding_worker".to_string(),
            parallelizable: true,
            max_attempts: None,
            dependencies: None,
        })
        .unwrap();
    assert_eq!(task.state, "queued");

    let assigned = store
        .service_assign_task(&task.id, Some("codex-1"))
        .unwrap();
    assert_eq!(assigned.state, "assigned");
    assert_eq!(assigned.assigned_worker_id.as_deref(), Some("codex-1"));

    let acked = store.service_ack_task(&task.id).unwrap();
    assert_eq!(acked.state, "acked");
    assert!(acked.acked_at.is_some());

    let working = store.service_start_task(&task.id).unwrap();
    assert_eq!(working.state, "working");
    assert!(working.started_at.is_some());

    let reported = store
        .service_report_task(
            &task.id,
            ServiceTaskReportInput {
                summary: "Done with token=secret-value".to_string(),
                files_inspected: vec!["/Users/example/private/file.rs".to_string()],
                changed_files: vec!["src/service.rs".to_string()],
                tests_run: vec!["cargo test sk-1234567890abcdef".to_string()],
                verification: "Focused tests pass".to_string(),
                risks: "none".to_string(),
                raw_report: "api_key=abc123456789".to_string(),
            },
        )
        .unwrap();
    assert_eq!(reported.state, "reported");
    assert!(reported
        .report_hash
        .as_deref()
        .unwrap()
        .starts_with("sha256:"));

    let report = store.service_get_task_report(&task.id).unwrap().unwrap();
    assert!(report.summary.contains("[REDACTED]"));
    assert!(report.files_inspected[0].contains("[REDACTED]"));
    assert!(report.tests_run[0].contains("[REDACTED]"));
    assert!(report.raw_report.contains("[REDACTED]"));

    let verified = store
        .service_verify_task(
            &task.id,
            squad::store::ServiceTaskProgressInput {
                summary: "verification reviewed".to_string(),
                decision: Some("complete".to_string()),
                evidence_hash: Some(format!("sha256:{}", "a".repeat(64))),
                evidence: Some(serde_json::json!({
                    "publicCheck": {"passed": true},
                    "hiddenRegression": {"passed": true},
                    "verifierVerdicts": [{"verifierId": "fractal-verify-1", "verdict": "pass"}]
                })),
            },
        )
        .unwrap();
    assert_eq!(verified.state, "verified");
    assert!(verified.verified_at.is_some());
    let verification = store
        .service_get_task_verification(&task.id)
        .unwrap()
        .unwrap();
    assert_eq!(verification.decision, "complete");
    assert_eq!(
        verification.evidence["hiddenRegression"]["passed"],
        serde_json::Value::Bool(true)
    );

    let completed = store.service_complete_task(&task.id).unwrap();
    assert_eq!(completed.state, "complete");
    assert!(completed.completed_at.is_some());
    let worker = store.service_get_worker("codex-1").unwrap().unwrap();
    assert_eq!(worker.status, "ready");
    assert_eq!(worker.current_task_id, None);
}

#[test]
fn service_store_rejects_invalid_task_state_jump() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    let task = store
        .service_create_task(ServiceTaskInput {
            title: "Invalid jump".to_string(),
            description: "Try to complete before report".to_string(),
            acceptance_criteria: vec!["must fail".to_string()],
            source_prd_path: "PRD.md".to_string(),
            source_task_number: Some("7".to_string()),
            priority: 0,
            preferred_model: "codex".to_string(),
            role: "coding_worker".to_string(),
            parallelizable: true,
            max_attempts: None,
            dependencies: None,
        })
        .unwrap();

    let error = store
        .service_complete_task(&task.id)
        .unwrap_err()
        .to_string();
    assert!(error.contains("cannot be completed from state queued"));
}

#[test]
fn service_store_imports_and_syncs_prd_checkboxes() {
    let tmp = TempDir::new().unwrap();
    let prd_path = tmp.path().join("PRD.md");
    std::fs::write(
        &prd_path,
        "# Coordinate PRD\n\n- [ ] 1. Add health route - Parallel\n- [ ] 2. Add task route - Serial\n",
    )
    .unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();

    let prd = store.service_import_prd(&prd_path).unwrap();
    assert_eq!(prd.title, "Coordinate PRD");
    let tasks = store
        .service_prd_tasks(&prd_path.to_string_lossy())
        .unwrap();
    assert_eq!(tasks.len(), 2);
    let task_numbers = tasks
        .iter()
        .map(|task| task.source_task_number.as_deref().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(task_numbers, std::collections::BTreeSet::from(["1", "2"]));

    store
        .service_register_worker(ServiceWorkerInput {
            id: "codex-1".to_string(),
            kind: "codex".to_string(),
            role: "coding_worker".to_string(),
            status: None,
            capacity: Some(1),
            metadata: None,
        })
        .unwrap();
    let first = tasks
        .iter()
        .find(|task| task.source_task_number.as_deref() == Some("1"))
        .unwrap();
    store
        .service_assign_task(&first.id, Some("codex-1"))
        .unwrap();
    store.service_ack_task(&first.id).unwrap();
    store
        .service_report_task(
            &first.id,
            ServiceTaskReportInput {
                summary: "done".to_string(),
                files_inspected: vec![],
                changed_files: vec![],
                tests_run: vec!["cargo test".to_string()],
                verification: "passed".to_string(),
                risks: "none".to_string(),
                raw_report: "passed".to_string(),
            },
        )
        .unwrap();
    store.service_complete_task(&first.id).unwrap();

    let changed = store.service_sync_prd(&prd_path).unwrap();
    assert_eq!(changed, 1);
    let synced = std::fs::read_to_string(&prd_path).unwrap();
    assert!(synced.contains("- [x] 1. Add health route"));
    assert!(synced.contains("- [ ] 2. Add task route"));
}

#[test]
fn service_store_lists_events_and_routes_broad_claude_tasks_to_codex() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("messages.db");
    let store = Store::open(&db_path).unwrap();
    store
        .service_register_worker(ServiceWorkerInput {
            id: "claude-1".to_string(),
            kind: "claude".to_string(),
            role: "coding_worker".to_string(),
            status: None,
            capacity: Some(1),
            metadata: None,
        })
        .unwrap();
    store
        .service_register_worker(ServiceWorkerInput {
            id: "codex-1".to_string(),
            kind: "codex".to_string(),
            role: "coding_worker".to_string(),
            status: None,
            capacity: Some(1),
            metadata: None,
        })
        .unwrap();
    let broad_description = "implement ".repeat(120);
    let task = store
        .service_create_task(ServiceTaskInput {
            title: "Broad Claude-preferred task".to_string(),
            description: broad_description,
            acceptance_criteria: vec![
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "d".to_string(),
            ],
            source_prd_path: "PRD.md".to_string(),
            source_task_number: Some("12".to_string()),
            priority: 10,
            preferred_model: "claude".to_string(),
            role: "coding_worker".to_string(),
            parallelizable: true,
            max_attempts: None,
            dependencies: None,
        })
        .unwrap();

    let assigned = store.service_assign_task(&task.id, None).unwrap();
    assert_eq!(assigned.assigned_worker_id.as_deref(), Some("codex-1"));
    assert_eq!(assigned.scheduling_pool, "codex");
    assert!(!assigned.claude_suitable);

    let events = store.service_list_events(Some(&task.id), None, 10).unwrap();
    assert!(events
        .iter()
        .any(|event| event.event_type == "task.assigned"));
}

#[test]
fn service_store_keeps_codex_and_claude_pools_independent() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    for (id, kind) in [("claude-1", "claude"), ("codex-1", "codex")] {
        store
            .service_register_worker(ServiceWorkerInput {
                id: id.to_string(),
                kind: kind.to_string(),
                role: "coding_worker".to_string(),
                status: None,
                capacity: Some(1),
                metadata: None,
            })
            .unwrap();
    }
    let small_claude = store
        .service_create_task(ServiceTaskInput {
            title: "Small Claude task".to_string(),
            description: "Edit one assertion".to_string(),
            acceptance_criteria: vec!["one focused test passes".to_string()],
            source_prd_path: "PRD.md".to_string(),
            source_task_number: Some("17".to_string()),
            priority: 5,
            preferred_model: "claude".to_string(),
            role: "coding_worker".to_string(),
            parallelizable: true,
            max_attempts: None,
            dependencies: None,
        })
        .unwrap();
    let codex_task = store
        .service_create_task(ServiceTaskInput {
            title: "Codex task".to_string(),
            description: "Implement service route".to_string(),
            acceptance_criteria: vec!["route works".to_string()],
            source_prd_path: "PRD.md".to_string(),
            source_task_number: Some("18".to_string()),
            priority: 4,
            preferred_model: "codex".to_string(),
            role: "coding_worker".to_string(),
            parallelizable: true,
            max_attempts: None,
            dependencies: None,
        })
        .unwrap();

    let assigned_claude = store.service_assign_task(&small_claude.id, None).unwrap();
    assert_eq!(
        assigned_claude.assigned_worker_id.as_deref(),
        Some("claude-1")
    );
    assert_eq!(assigned_claude.scheduling_pool, "claude");
    assert!(assigned_claude.claude_suitable);

    let assigned_codex = store.service_assign_task(&codex_task.id, None).unwrap();
    assert_eq!(
        assigned_codex.assigned_worker_id.as_deref(),
        Some("codex-1")
    );
}

#[test]
fn service_store_blocks_dependencies_and_serial_prd_order() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    store
        .service_register_worker(ServiceWorkerInput {
            id: "codex-1".to_string(),
            kind: "codex".to_string(),
            role: "coding_worker".to_string(),
            status: None,
            capacity: Some(1),
            metadata: None,
        })
        .unwrap();
    let first = store
        .service_create_task(ServiceTaskInput {
            title: "First serial".to_string(),
            description: "Must finish first".to_string(),
            acceptance_criteria: vec!["done".to_string()],
            source_prd_path: "PRD.md".to_string(),
            source_task_number: Some("1".to_string()),
            priority: 1,
            preferred_model: "codex".to_string(),
            role: "coding_worker".to_string(),
            parallelizable: false,
            max_attempts: None,
            dependencies: None,
        })
        .unwrap();
    let second = store
        .service_create_task(ServiceTaskInput {
            title: "Second serial".to_string(),
            description: "Blocked by first".to_string(),
            acceptance_criteria: vec!["done after first".to_string()],
            source_prd_path: "PRD.md".to_string(),
            source_task_number: Some("2".to_string()),
            priority: 2,
            preferred_model: "codex".to_string(),
            role: "coding_worker".to_string(),
            parallelizable: false,
            max_attempts: None,
            dependencies: Some(vec![first.id.clone()]),
        })
        .unwrap();

    let error = store
        .service_assign_task(&second.id, Some("codex-1"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("blocked by incomplete dependency"));

    store
        .service_assign_task(&first.id, Some("codex-1"))
        .unwrap();
    store.service_ack_task(&first.id).unwrap();
    store.service_start_task(&first.id).unwrap();
    store
        .service_report_task(
            &first.id,
            ServiceTaskReportInput {
                summary: "done".to_string(),
                files_inspected: vec![],
                changed_files: vec![],
                tests_run: vec!["cargo test".to_string()],
                verification: "passed".to_string(),
                risks: "none".to_string(),
                raw_report: "passed".to_string(),
            },
        )
        .unwrap();
    store.service_complete_task(&first.id).unwrap();

    let assigned = store
        .service_assign_task(&second.id, Some("codex-1"))
        .unwrap();
    assert_eq!(assigned.assigned_worker_id.as_deref(), Some("codex-1"));
}

#[test]
fn service_store_requeues_stale_assignments() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("messages.db");
    let store = Store::open(&db_path).unwrap();
    store
        .service_register_worker(ServiceWorkerInput {
            id: "codex-1".to_string(),
            kind: "codex".to_string(),
            role: "coding_worker".to_string(),
            status: None,
            capacity: Some(1),
            metadata: None,
        })
        .unwrap();
    let task = store
        .service_create_task(ServiceTaskInput {
            title: "Stale task".to_string(),
            description: "Assigned then stale".to_string(),
            acceptance_criteria: vec!["requeues".to_string()],
            source_prd_path: "PRD.md".to_string(),
            source_task_number: Some("13".to_string()),
            priority: 1,
            preferred_model: "codex".to_string(),
            role: "coding_worker".to_string(),
            parallelizable: true,
            max_attempts: Some(2),
            dependencies: None,
        })
        .unwrap();
    store
        .service_assign_task(&task.id, Some("codex-1"))
        .unwrap();
    let stale_at = chrono::Utc::now()
        .checked_sub_signed(chrono::Duration::seconds(3600))
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "UPDATE service_tasks SET updated_at = ?1 WHERE id = ?2",
        rusqlite::params![stale_at, task.id],
    )
    .unwrap();

    let result = store.service_requeue_stale_tasks(60).unwrap();
    assert_eq!(result.requeued.len(), 1);
    assert_eq!(result.requeued[0].state, "queued");
    assert_eq!(result.requeued[0].assigned_worker_id, None);
    assert_eq!(result.requeued[0].attempt, 1);
    let worker = store.service_get_worker("codex-1").unwrap().unwrap();
    assert_eq!(worker.status, "ready");
    assert_eq!(worker.current_task_id, None);
}

#[test]
fn service_store_marks_stale_workers_offline_and_retries_current_task() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("messages.db");
    let store = Store::open(&db_path).unwrap();
    store
        .service_register_worker(ServiceWorkerInput {
            id: "codex-1".to_string(),
            kind: "codex".to_string(),
            role: "coding_worker".to_string(),
            status: None,
            capacity: Some(1),
            metadata: None,
        })
        .unwrap();
    let task = store
        .service_create_task(ServiceTaskInput {
            title: "Bridge task".to_string(),
            description: "Assigned to stale worker".to_string(),
            acceptance_criteria: vec!["retries".to_string()],
            source_prd_path: "PRD.md".to_string(),
            source_task_number: Some("14".to_string()),
            priority: 1,
            preferred_model: "codex".to_string(),
            role: "coding_worker".to_string(),
            parallelizable: true,
            max_attempts: Some(2),
            dependencies: None,
        })
        .unwrap();
    store
        .service_assign_task(&task.id, Some("codex-1"))
        .unwrap();
    let stale_at = chrono::Utc::now()
        .checked_sub_signed(chrono::Duration::seconds(3600))
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "UPDATE service_workers SET last_heartbeat_at = ?1 WHERE id = 'codex-1'",
        rusqlite::params![stale_at],
    )
    .unwrap();

    let result = store.service_expire_stale_workers(60).unwrap();
    assert_eq!(result.offline_workers.len(), 1);
    assert_eq!(result.offline_workers[0].status, "offline");
    assert_eq!(result.requeued.len(), 1);
    assert_eq!(result.requeued[0].state, "queued");
    assert_eq!(
        result.requeued[0].retry_reason.as_deref(),
        Some("stale worker heartbeat")
    );
}

#[test]
fn service_store_retry_policy_enforces_max_attempts() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    let task = store
        .service_create_task(ServiceTaskInput {
            title: "Retry task".to_string(),
            description: "Retry until capped".to_string(),
            acceptance_criteria: vec!["caps retry".to_string()],
            source_prd_path: "PRD.md".to_string(),
            source_task_number: Some("15".to_string()),
            priority: 1,
            preferred_model: "codex".to_string(),
            role: "coding_worker".to_string(),
            parallelizable: true,
            max_attempts: Some(1),
            dependencies: None,
        })
        .unwrap();

    let retried = store
        .service_retry_task(&task.id, "token=secret-value transient failure")
        .unwrap();
    assert_eq!(retried.state, "queued");
    assert_eq!(retried.attempt, 1);
    assert!(retried
        .last_error
        .as_deref()
        .unwrap()
        .contains("[REDACTED]"));

    let error = store
        .service_retry_task(&task.id, "another failure")
        .unwrap_err()
        .to_string();
    assert!(error.contains("exceeded max attempts"));
}

#[test]
fn rejected_graph_node_retries_once_escalates_and_never_double_claims() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("messages.db");
    let store = Store::open(&db_path).unwrap();
    register_service_worker(&store, "codex-1", "codex");
    register_service_worker(&store, "codex-2", "codex");
    let task = store
        .service_create_task(ServiceTaskInput {
            title: "Rejected graph node".to_string(),
            description: "Exercise verifier rejection lifecycle".to_string(),
            acceptance_criteria: vec!["retry then escalate".to_string()],
            source_prd_path: "fractal-graph:graph:test:sha256:abc".to_string(),
            source_task_number: Some("verify".to_string()),
            priority: 10,
            preferred_model: "codex".to_string(),
            role: "coding_worker".to_string(),
            parallelizable: true,
            max_attempts: Some(1),
            dependencies: None,
        })
        .unwrap();

    let first = store
        .service_claim_next_task("codex-1", 900, DEFAULT_SERVICE_LANE_ESCAPE_SECS)
        .unwrap()
        .unwrap();
    assert_eq!(first.id, task.id);
    store.service_start_task(&task.id).unwrap();
    report_rejected_graph_node(&store, &task.id);
    let retried = store
        .service_reject_task(&task.id, "hidden regression missing")
        .unwrap();
    assert_eq!(retried.state, "queued");
    assert_eq!(retried.attempt, 1);
    assert_eq!(retried.lease_owner, None);
    assert_eq!(retried.lease_expires_at, None);
    drop(store);

    let claims = ["codex-1", "codex-2"]
        .into_iter()
        .map(|worker_id| {
            let db_path = db_path.clone();
            thread::spawn(move || {
                Store::open(&db_path)
                    .unwrap()
                    .service_claim_next_task(worker_id, 900, DEFAULT_SERVICE_LANE_ESCAPE_SECS)
                    .unwrap()
                    .map(|claimed| (worker_id.to_string(), claimed.id))
            })
        })
        .map(|handle| handle.join().unwrap())
        .flatten()
        .collect::<Vec<_>>();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].1, task.id);

    let store = Store::open(&db_path).unwrap();
    store.service_start_task(&task.id).unwrap();
    report_rejected_graph_node(&store, &task.id);
    let escalated = store
        .service_reject_task(&task.id, "public verifier still failing")
        .unwrap();
    assert_eq!(escalated.state, "blocked");
    assert_eq!(escalated.lease_owner, None);
    assert!(escalated
        .failure_reason
        .as_deref()
        .unwrap()
        .contains("verification retries exhausted"));
    assert!(store
        .service_claim_next_task(
            if claims[0].0 == "codex-1" {
                "codex-2"
            } else {
                "codex-1"
            },
            900,
            DEFAULT_SERVICE_LANE_ESCAPE_SECS,
        )
        .unwrap()
        .is_none());

    let event_types = store
        .service_list_events(Some(&task.id), None, 20)
        .unwrap()
        .into_iter()
        .map(|event| event.event_type)
        .collect::<Vec<_>>();
    assert!(event_types.contains(&"task.retry".to_string()));
    assert!(event_types.contains(&"task.blocked".to_string()));
}

fn report_rejected_graph_node(store: &Store, task_id: &str) {
    store
        .service_report_task(
            task_id,
            ServiceTaskReportInput {
                summary: "worker report".to_string(),
                files_inspected: Vec::new(),
                changed_files: Vec::new(),
                tests_run: vec!["fixture".to_string()],
                verification: "worker evidence".to_string(),
                risks: "none".to_string(),
                raw_report: "reported".to_string(),
            },
        )
        .unwrap();
}

#[tokio::test]
async fn service_http_health_and_task_create_routes_work() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("messages.db");
    Store::open(&db_path).unwrap();
    let app = router(db_path);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/tasks")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "title": "HTTP task",
                        "description": "Created through HTTP",
                        "acceptanceCriteria": ["created"],
                        "sourcePrdPath": "PRD.md",
                        "priority": 1,
                        "preferredModel": "codex",
                        "role": "coding_worker",
                        "parallelizable": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["state"], "queued");
}

#[tokio::test]
async fn service_http_new_lifecycle_routes_work() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("messages.db");
    let store = Store::open(&db_path).unwrap();
    let worker = store
        .service_register_worker(ServiceWorkerInput {
            id: "codex-1".to_string(),
            kind: "codex".to_string(),
            role: "coding_worker".to_string(),
            status: Some("starting".to_string()),
            capacity: Some(1),
            metadata: None,
        })
        .unwrap();
    let task = store
        .service_create_task(ServiceTaskInput {
            title: "HTTP lifecycle".to_string(),
            description: "Exercise HTTP lifecycle".to_string(),
            acceptance_criteria: vec!["routes work".to_string()],
            source_prd_path: "PRD.md".to_string(),
            source_task_number: Some("16".to_string()),
            priority: 1,
            preferred_model: "codex".to_string(),
            role: "coding_worker".to_string(),
            parallelizable: true,
            max_attempts: None,
            dependencies: None,
        })
        .unwrap();
    assert_eq!(worker.status, "starting");
    let app = router(db_path);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/workers/codex-1/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    for (uri, body) in [
        (
            format!("/tasks/{}/assign", task.id),
            json!({"workerId": "codex-1"}).to_string(),
        ),
        (format!("/tasks/{}/ack", task.id), "{}".to_string()),
        (format!("/tasks/{}/start", task.id), "{}".to_string()),
        (
            format!("/tasks/{}/progress", task.id),
            json!({"summary": "halfway"}).to_string(),
        ),
        (
            format!("/tasks/{}/report", task.id),
            json!({
                "summary": "done",
                "filesInspected": ["src/service.rs"],
                "changedFiles": ["src/service.rs"],
                "testsRun": ["cargo test"],
                "verification": "passed",
                "risks": "none",
                "rawReport": "passed"
            })
            .to_string(),
        ),
        (
            format!("/tasks/{}/verify", task.id),
            json!({"summary": "verified"}).to_string(),
        ),
        (format!("/tasks/{}/complete", task.id), "{}".to_string()),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}

#[tokio::test]
async fn service_http_pull_queue_routes_work() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("messages.db");
    let store = Store::open(&db_path).unwrap();
    register_service_worker(&store, "codex-1", "codex");
    let task = create_service_task(&store, "HTTP pull queue", "codex", 1);
    let app = router(db_path.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/tasks/next")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"workerId": "codex-1", "leaseSecs": 900}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let claimed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(claimed["id"], task.id);
    assert_eq!(claimed["state"], "acked");
    assert_eq!(claimed["leaseOwner"], "codex-1");

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/tasks/{}/touch", task.id))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"workerId": "codex-1", "leaseSecs": 900}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tasks/stats?windowSecs=3600")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let stats: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(stats["totalInFlight"], 1);

    let expired_at = chrono::Utc::now()
        .checked_sub_signed(chrono::Duration::seconds(5))
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "UPDATE service_tasks SET lease_expires_at = ?1 WHERE id = ?2",
        rusqlite::params![expired_at, task.id],
    )
    .unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/tasks/reap")
                .header("content-type", "application/json")
                .body(Body::from(json!({"staleAfterSecs": 60}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let reaped: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(reaped["leaseExpired"].as_array().unwrap().len(), 1);
}

#[test]
fn service_pull_queue_normalizes_provider_lanes() {
    assert_eq!(
        normalize_service_provider_lane(Some(" Claude, CODEX ")).as_deref(),
        Some("claude,codex")
    );
    assert_eq!(normalize_service_provider_lane(Some("any")), None);
    assert_eq!(normalize_service_provider_lane(Some(" , ")), None);
    assert_eq!(normalize_service_provider_lane(None), None);
}

#[test]
fn service_pull_queue_claims_direct_assignment_before_higher_priority_queue_work() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    register_service_worker(&store, "codex-1", "codex");

    let direct = create_service_task(&store, "Direct task", "codex", 1);
    let urgent = create_service_task(&store, "Urgent queue task", "codex", 99);
    store
        .service_assign_task(&direct.id, Some("codex-1"))
        .unwrap();

    let claimed = store
        .service_claim_next_task(
            "codex-1",
            DEFAULT_SERVICE_CLAIM_LEASE_SECS,
            DEFAULT_SERVICE_LANE_ESCAPE_SECS,
        )
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, direct.id);
    assert_eq!(claimed.state, "acked");
    assert_eq!(claimed.assigned_worker_id.as_deref(), Some("codex-1"));
    assert_eq!(claimed.lease_owner.as_deref(), Some("codex-1"));
    assert!(claimed.lease_expires_at.is_some());
    assert!(claimed.claimed_at.is_some());
    assert!(claimed.acked_at.is_some());

    let urgent = store.service_get_task(&urgent.id).unwrap().unwrap();
    assert_eq!(urgent.state, "queued");
    assert_eq!(urgent.assigned_worker_id, None);
}

#[test]
fn service_pull_queue_releases_unstarted_claim_without_consuming_retry() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    register_service_worker(&store, "codex-1", "codex");
    let task = create_service_task(&store, "Graph node lease", "codex", 1);
    let claimed = store
        .service_claim_next_task("codex-1", 900, DEFAULT_SERVICE_LANE_ESCAPE_SECS)
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, task.id);

    let released = store
        .service_release_task_claim("codex-1", &task.id, "execution graph checkout conflict")
        .unwrap();
    assert_eq!(released.state, "queued");
    assert_eq!(released.assigned_worker_id, None);
    assert_eq!(released.lease_owner, None);
    assert_eq!(released.retry_count, 0);
    assert_eq!(released.attempt, 0);
    assert_eq!(
        store.service_get_worker("codex-1").unwrap().unwrap().status,
        "ready"
    );
    assert!(store
        .service_list_events(Some(&task.id), None, 10)
        .unwrap()
        .iter()
        .any(|event| event.event_type == "task.claim_released"));
}

#[test]
fn service_pull_queue_orders_by_direct_assignment_then_priority() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    register_service_worker(&store, "codex-1", "codex");

    let low_codex = create_service_task(&store, "Low codex", "codex", 1);
    let high_codex = create_service_task(&store, "High codex", "codex", 10);

    let first = store
        .service_claim_next_task("codex-1", 900, DEFAULT_SERVICE_LANE_ESCAPE_SECS)
        .unwrap()
        .unwrap();
    assert_eq!(first.id, high_codex.id);

    let second_worker = register_service_worker(&store, "codex-2", "codex");
    assert_eq!(second_worker.status, "ready");
    let second = store
        .service_claim_next_task("codex-2", 900, DEFAULT_SERVICE_LANE_ESCAPE_SECS)
        .unwrap()
        .unwrap();
    assert_eq!(second.id, low_codex.id);
}

#[test]
fn service_pull_queue_prefers_lane_match_before_open_lane_at_same_priority() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("messages.db");
    let store = Store::open(&db_path).unwrap();
    register_service_worker(&store, "codex-1", "codex");

    let open = create_service_task(&store, "Open task", "manual", 5);
    let codex = create_service_task(&store, "Codex task", "codex", 5);
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "UPDATE service_tasks SET eligible_providers = NULL, scheduling_pool = 'any' WHERE id = ?1",
        rusqlite::params![open.id],
    )
    .unwrap();

    let first = store
        .service_claim_next_task("codex-1", 900, DEFAULT_SERVICE_LANE_ESCAPE_SECS)
        .unwrap()
        .unwrap();
    assert_eq!(first.id, codex.id);

    register_service_worker(&store, "codex-2", "codex");
    let second = store
        .service_claim_next_task("codex-2", 900, DEFAULT_SERVICE_LANE_ESCAPE_SECS)
        .unwrap()
        .unwrap();
    assert_eq!(second.id, open.id);
}

#[test]
fn service_pull_queue_lane_escape_keeps_work_conserving() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("messages.db");
    let store = Store::open(&db_path).unwrap();
    register_service_worker(&store, "claude-1", "claude");

    let codex_task = create_service_task(&store, "Aging codex task", "codex", 1);
    assert!(store
        .service_claim_next_task("claude-1", 900, DEFAULT_SERVICE_LANE_ESCAPE_SECS)
        .unwrap()
        .is_none());

    let stale_created_at = chrono::Utc::now()
        .checked_sub_signed(chrono::Duration::seconds(
            DEFAULT_SERVICE_LANE_ESCAPE_SECS + 5,
        ))
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "UPDATE service_tasks SET created_at = ?1 WHERE id = ?2",
        rusqlite::params![stale_created_at, codex_task.id],
    )
    .unwrap();

    let claimed = store
        .service_claim_next_task("claude-1", 900, DEFAULT_SERVICE_LANE_ESCAPE_SECS)
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, codex_task.id);
    assert_eq!(claimed.assigned_worker_id.as_deref(), Some("claude-1"));
}

#[test]
fn service_pull_queue_touch_reap_and_stats_work() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("messages.db");
    let store = Store::open(&db_path).unwrap();
    register_service_worker(&store, "codex-1", "codex");
    register_service_worker(&store, "codex-2", "codex");

    let leased = create_service_task(&store, "Leased task", "codex", 1);
    let claimed = store
        .service_claim_next_task("codex-1", 1, DEFAULT_SERVICE_LANE_ESCAPE_SECS)
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, leased.id);
    let touched = store
        .service_touch_task("codex-1", &leased.id, 900)
        .unwrap();
    assert_eq!(touched.lease_owner.as_deref(), Some("codex-1"));
    assert!(touched.lease_expires_at.is_some());

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let expired_at = chrono::Utc::now()
        .checked_sub_signed(chrono::Duration::seconds(5))
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    conn.execute(
        "UPDATE service_tasks SET lease_expires_at = ?1 WHERE id = ?2",
        rusqlite::params![expired_at, leased.id],
    )
    .unwrap();
    let reaped = store.service_reap_queue(60).unwrap();
    assert_eq!(reaped.lease_expired.len(), 1);
    assert_eq!(reaped.lease_expired[0].state, "queued");
    assert_eq!(reaped.lease_expired[0].assigned_worker_id, None);
    assert_eq!(reaped.lease_expired[0].lease_owner, None);

    let reassigned = store
        .service_claim_next_task("codex-2", 900, DEFAULT_SERVICE_LANE_ESCAPE_SECS)
        .unwrap()
        .unwrap();
    assert_eq!(reassigned.id, leased.id);
    assert_eq!(reassigned.assigned_worker_id.as_deref(), Some("codex-2"));

    let stats = store.service_queue_stats(3600).unwrap();
    assert_eq!(stats.total_in_flight, 1);
    assert!(stats
        .providers
        .iter()
        .any(|provider| provider.worker_kind == "codex" && provider.in_flight == 1));
}

#[test]
fn service_pull_queue_releases_stale_direct_assignments() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("messages.db");
    let store = Store::open(&db_path).unwrap();
    register_service_worker(&store, "codex-1", "codex");
    register_service_worker(&store, "codex-2", "codex");

    let task = create_service_task(&store, "Direct stale", "codex", 1);
    store
        .service_assign_task(&task.id, Some("codex-1"))
        .unwrap();
    let stale_at = chrono::Utc::now()
        .checked_sub_signed(chrono::Duration::seconds(3600))
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "UPDATE service_tasks SET updated_at = ?1 WHERE id = ?2",
        rusqlite::params![stale_at, task.id],
    )
    .unwrap();

    let reaped = store.service_reap_queue(60).unwrap();
    assert_eq!(reaped.assignment_released.len(), 1);
    assert_eq!(reaped.assignment_released[0].assigned_worker_id, None);
    let claimed = store
        .service_claim_next_task("codex-2", 900, DEFAULT_SERVICE_LANE_ESCAPE_SECS)
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, task.id);
    assert_eq!(claimed.assigned_worker_id.as_deref(), Some("codex-2"));
}

#[test]
fn service_pull_queue_concurrent_claims_do_not_duplicate_task() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("messages.db");
    let store = Store::open(&db_path).unwrap();
    register_service_worker(&store, "codex-1", "codex");
    register_service_worker(&store, "codex-2", "codex");
    let task = create_service_task(&store, "Single concurrent task", "codex", 1);
    drop(store);

    let mut handles = Vec::new();
    for worker_id in ["codex-1", "codex-2"] {
        let db_path = db_path.clone();
        handles.push(thread::spawn(move || {
            let store = Store::open(&db_path).unwrap();
            store
                .service_claim_next_task(worker_id, 900, DEFAULT_SERVICE_LANE_ESCAPE_SECS)
                .unwrap()
                .map(|claimed| (worker_id.to_string(), claimed.id))
        }));
    }

    let claimed = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .flatten()
        .collect::<Vec<_>>();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].1, task.id);

    let store = Store::open(&db_path).unwrap();
    let final_task = store.service_get_task(&task.id).unwrap().unwrap();
    assert_eq!(final_task.state, "acked");
    assert_eq!(
        final_task.lease_owner.as_deref(),
        Some(claimed[0].0.as_str())
    );
}
