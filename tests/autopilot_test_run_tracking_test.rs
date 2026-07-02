use squad::autopilot::{
    read_tests_run, record_test_run, tests_run_path, tests_run_report_lines, TestRunRecord,
    TestRunStatus,
};
use tempfile::TempDir;

#[test]
fn test_record_test_run_appends_structured_records() {
    let tmp = TempDir::new().unwrap();

    let records = record_test_run(
        tmp.path(),
        TestRunRecord {
            command: " cargo test ".to_string(),
            status: TestRunStatus::Passed,
            exit_code: Some(0),
            task_id: Some(" task-48 ".to_string()),
            agent_id: Some(" test_engineer ".to_string()),
            notes: Some(" all tests passed ".to_string()),
        },
    )
    .unwrap();

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].command, "cargo test");
    assert_eq!(records[0].task_id.as_deref(), Some("task-48"));
    assert_eq!(records[0].agent_id.as_deref(), Some("test_engineer"));
    assert_eq!(records[0].notes.as_deref(), Some("all tests passed"));

    let records = record_test_run(
        tmp.path(),
        TestRunRecord {
            command: "cargo test --doc".to_string(),
            status: TestRunStatus::Failed,
            exit_code: Some(101),
            task_id: None,
            agent_id: Some("".to_string()),
            notes: None,
        },
    )
    .unwrap();

    assert_eq!(records.len(), 2);
    assert_eq!(records[1].agent_id, None);
    assert_eq!(read_tests_run(tmp.path()).unwrap(), records);
    assert_eq!(
        tests_run_path(tmp.path()),
        tmp.path()
            .join(".squad")
            .join("autopilot")
            .join("tests-run.json")
    );
}

#[test]
fn test_tests_run_report_lines_formats_records_for_final_report() {
    let lines = tests_run_report_lines(&[
        TestRunRecord {
            command: "cargo test".to_string(),
            status: TestRunStatus::Passed,
            exit_code: Some(0),
            task_id: Some("task-48".to_string()),
            agent_id: Some("test_engineer".to_string()),
            notes: Some("all green".to_string()),
        },
        TestRunRecord {
            command: "npm test".to_string(),
            status: TestRunStatus::Skipped,
            exit_code: None,
            task_id: None,
            agent_id: None,
            notes: Some("no frontend package".to_string()),
        },
    ]);

    assert_eq!(
        lines,
        vec![
            "cargo test - passed (exit 0) [task: task-48] [agent: test_engineer]: all green",
            "npm test - skipped: no frontend package",
        ]
    );
}

#[test]
fn test_record_test_run_rejects_empty_command() {
    let tmp = TempDir::new().unwrap();

    let error = record_test_run(
        tmp.path(),
        TestRunRecord {
            command: "   ".to_string(),
            status: TestRunStatus::Skipped,
            exit_code: None,
            task_id: None,
            agent_id: None,
            notes: None,
        },
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("test run command cannot be empty"));
}
