use squad::store::Store;
use tempfile::TempDir;

#[test]
fn test_task_lifecycle_and_requeue() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    store.register_agent("manager", "manager").unwrap();
    store.register_agent("worker", "worker").unwrap();
    store.register_agent("worker-2", "worker").unwrap();

    let task_id = store
        .create_task("manager", "worker", "Implement auth", "Add JWT flow")
        .unwrap();

    let created = store.get_task(&task_id).unwrap().unwrap();
    assert_eq!(created.id, task_id);
    assert_eq!(created.title, "Implement auth");
    assert_eq!(created.body, "Add JWT flow");
    assert_eq!(created.created_by, "manager");
    assert_eq!(created.assigned_to.as_deref(), Some("worker"));
    assert_eq!(created.status, "queued");
    assert_eq!(created.lease_owner, None);
    assert_eq!(created.lease_expires_at, None);
    assert_eq!(created.result_summary, None);
    assert_eq!(created.completed_at, None);

    store.ack_task("worker", &task_id).unwrap();
    let acked = store.get_task(&task_id).unwrap().unwrap();
    assert_eq!(acked.status, "acked");
    assert_eq!(acked.assigned_to.as_deref(), Some("worker"));
    assert_eq!(acked.lease_owner.as_deref(), Some("worker"));
    assert!(acked.lease_expires_at.is_some());
    assert!(acked.completed_at.is_none());

    store.complete_task("worker", &task_id, "done").unwrap();
    let completed = store.get_task(&task_id).unwrap().unwrap();
    assert_eq!(completed.status, "completed");
    assert_eq!(completed.lease_owner.as_deref(), Some("worker"));
    assert_eq!(completed.result_summary.as_deref(), Some("done"));
    assert!(completed.completed_at.is_some());

    store.requeue_task(&task_id, None).unwrap();
    let requeued = store.get_task(&task_id).unwrap().unwrap();
    assert_eq!(requeued.status, "queued");
    assert_eq!(requeued.assigned_to, None);
    assert_eq!(requeued.lease_owner, None);
    assert_eq!(requeued.lease_expires_at, None);
    assert_eq!(requeued.completed_at, None);
    assert_eq!(requeued.result_summary, None);

    store.requeue_task(&task_id, Some("worker-2")).unwrap();
    let reassigned = store.get_task(&task_id).unwrap().unwrap();
    assert_eq!(reassigned.status, "queued");
    assert_eq!(reassigned.assigned_to.as_deref(), Some("worker-2"));
    assert_eq!(reassigned.lease_owner, None);
    assert_eq!(reassigned.lease_expires_at, None);
    assert_eq!(reassigned.completed_at, None);
    assert_eq!(reassigned.result_summary, None);
}

#[test]
fn test_create_task_dual_writes_assignment_envelope() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    store.register_agent("manager", "manager").unwrap();
    store.register_agent("worker", "worker").unwrap();

    let task_id = store
        .create_task("manager", "worker", "Implement auth", "Add JWT flow")
        .unwrap();

    let messages = store.receive_messages("worker").unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].kind, "task_assigned");
    assert_eq!(messages[0].task_id.as_deref(), Some(task_id.as_str()));
    assert_eq!(messages[0].reply_to, None);
    assert_eq!(messages[0].content, "Implement auth");
}

#[test]
fn test_create_task_rolls_back_when_envelope_write_fails() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("messages.db");
    let store = Store::open(&db_path).unwrap();
    store.register_agent("manager", "manager").unwrap();
    store.register_agent("worker", "worker").unwrap();

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER fail_task_envelope
         BEFORE INSERT ON messages
         WHEN NEW.kind = 'task_assigned'
         BEGIN
             SELECT RAISE(FAIL, 'envelope write failed');
         END;",
    )
    .unwrap();
    drop(conn);

    let result = store.create_task("manager", "worker", "Implement auth", "Add JWT flow");
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("envelope write failed"));

    let tasks = store.list_tasks(None, None).unwrap();
    assert!(tasks.is_empty());
    let messages = store.all_messages(None).unwrap();
    assert!(messages.is_empty());
}

#[test]
fn test_create_task_rejects_archived_participants_without_writing_rows() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    store.register_agent("manager", "manager").unwrap();
    store.register_agent("worker", "worker").unwrap();
    store.unregister_agent("worker").unwrap();

    let result = store.create_task("manager", "worker", "Implement auth", "Add JWT flow");
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("worker is archived"));

    let tasks = store.list_tasks(None, None).unwrap();
    assert!(tasks.is_empty());
    let messages = store.all_messages(None).unwrap();
    assert!(messages.is_empty());
}

#[test]
fn test_create_task_rejects_archived_creator_without_writing_rows() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    store.register_agent("manager", "manager").unwrap();
    store.register_agent("worker", "worker").unwrap();
    store.unregister_agent("manager").unwrap();

    let result = store.create_task("manager", "worker", "Implement auth", "Add JWT flow");
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("manager is archived"));

    let tasks = store.list_tasks(None, None).unwrap();
    assert!(tasks.is_empty());
    let messages = store.all_messages(None).unwrap();
    assert!(messages.is_empty());
}

#[test]
fn test_task_state_updates_fail_when_conditional_update_matches_no_rows() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("messages.db");
    let store = Store::open(&db_path).unwrap();
    store.register_agent("manager", "manager").unwrap();
    store.register_agent("worker", "worker").unwrap();

    let ack_task_id = store
        .create_task("manager", "worker", "Ack auth", "Add JWT flow")
        .unwrap();
    let complete_task_id = store
        .create_task("manager", "worker", "Complete auth", "Add JWT flow")
        .unwrap();
    let requeue_task_id = store
        .create_task("manager", "worker", "Requeue auth", "Add JWT flow")
        .unwrap();
    store.ack_task("worker", &complete_task_id).unwrap();
    store.ack_task("worker", &requeue_task_id).unwrap();

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER ignore_ack_update
         BEFORE UPDATE ON tasks
         WHEN OLD.id IN (SELECT id FROM tasks WHERE title = 'Ack auth')
              AND NEW.status = 'acked'
         BEGIN
             SELECT RAISE(IGNORE);
         END;
         CREATE TRIGGER ignore_complete_update
         BEFORE UPDATE ON tasks
         WHEN OLD.id IN (SELECT id FROM tasks WHERE title = 'Complete auth')
              AND NEW.status = 'completed'
         BEGIN
             SELECT RAISE(IGNORE);
         END;
         CREATE TRIGGER ignore_requeue_update
         BEFORE UPDATE ON tasks
         WHEN OLD.id IN (SELECT id FROM tasks WHERE title = 'Requeue auth')
              AND NEW.status = 'queued'
         BEGIN
             SELECT RAISE(IGNORE);
         END;",
    )
    .unwrap();
    drop(conn);

    let ack_result = store.ack_task("worker", &ack_task_id);
    assert!(ack_result.is_err());
    assert!(ack_result
        .unwrap_err()
        .to_string()
        .contains("stale task state"));
    let acked = store.get_task(&ack_task_id).unwrap().unwrap();
    assert_eq!(acked.status, "queued");

    let complete_result = store.complete_task("worker", &complete_task_id, "done");
    assert!(complete_result.is_err());
    assert!(complete_result
        .unwrap_err()
        .to_string()
        .contains("stale task state"));
    let completed = store.get_task(&complete_task_id).unwrap().unwrap();
    assert_eq!(completed.status, "acked");
    assert_eq!(completed.result_summary, None);

    let requeue_result = store.requeue_task(&requeue_task_id, None);
    assert!(requeue_result.is_err());
    assert!(requeue_result
        .unwrap_err()
        .to_string()
        .contains("stale task state"));
    let requeued = store.get_task(&requeue_task_id).unwrap().unwrap();
    assert_eq!(requeued.status, "acked");
    assert_eq!(requeued.assigned_to.as_deref(), Some("worker"));
}

#[test]
fn test_list_tasks_can_filter_by_agent_and_status() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(&tmp.path().join("messages.db")).unwrap();
    store.register_agent("manager", "manager").unwrap();
    store.register_agent("worker", "worker").unwrap();
    store.register_agent("worker-2", "worker").unwrap();

    let queued = store
        .create_task("manager", "worker", "Implement auth", "Add JWT flow")
        .unwrap();
    let acked = store
        .create_task("manager", "worker", "Review auth", "Check JWT flow")
        .unwrap();
    let completed = store
        .create_task("manager", "worker-2", "Ship auth", "Merge changes")
        .unwrap();

    store.ack_task("worker", &acked).unwrap();
    store.ack_task("worker-2", &completed).unwrap();
    store
        .complete_task("worker-2", &completed, "merged")
        .unwrap();

    let worker_tasks = store.list_tasks(Some("worker"), None).unwrap();
    assert_eq!(worker_tasks.len(), 2);
    assert_eq!(worker_tasks[0].title, "Implement auth");
    assert_eq!(worker_tasks[0].id, queued);
    assert_eq!(worker_tasks[1].title, "Review auth");
    assert_eq!(worker_tasks[1].id, acked);

    let acked_tasks = store.list_tasks(None, Some("acked")).unwrap();
    assert_eq!(acked_tasks.len(), 1);
    assert_eq!(acked_tasks[0].id, acked);

    let completed_tasks = store
        .list_tasks(Some("worker-2"), Some("completed"))
        .unwrap();
    assert_eq!(completed_tasks.len(), 1);
    assert_eq!(completed_tasks[0].id, completed);
}

#[test]
fn test_list_tasks_uses_deterministic_business_order_when_created_at_matches() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("messages.db");
    let store = Store::open(&db_path).unwrap();

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "INSERT INTO tasks (
             id, title, body, created_by, assigned_to, status,
             lease_owner, lease_expires_at, result_summary,
             created_at, updated_at, completed_at
         ) VALUES (?1, ?2, 'body', 'manager', 'worker', 'queued', NULL, NULL, NULL, 100, 100, NULL)",
        rusqlite::params!["task-b", "Bravo"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO tasks (
             id, title, body, created_by, assigned_to, status,
             lease_owner, lease_expires_at, result_summary,
             created_at, updated_at, completed_at
         ) VALUES (?1, ?2, 'body', 'manager', 'worker', 'queued', NULL, NULL, NULL, 100, 100, NULL)",
        rusqlite::params!["task-a2", "Alpha"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO tasks (
             id, title, body, created_by, assigned_to, status,
             lease_owner, lease_expires_at, result_summary,
             created_at, updated_at, completed_at
         ) VALUES (?1, ?2, 'body', 'manager', 'worker', 'queued', NULL, NULL, NULL, 100, 100, NULL)",
        rusqlite::params!["task-a1", "Alpha"],
    )
    .unwrap();
    drop(conn);

    let tasks = store.list_tasks(None, None).unwrap();
    let ordered: Vec<_> = tasks
        .into_iter()
        .map(|task| (task.title, task.id))
        .collect();
    assert_eq!(
        ordered,
        vec![
            ("Alpha".to_string(), "task-a1".to_string()),
            ("Alpha".to_string(), "task-a2".to_string()),
            ("Bravo".to_string(), "task-b".to_string()),
        ]
    );
}
