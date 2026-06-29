use squad::autopilot::ingest_prd_file;
use tempfile::TempDir;

#[test]
fn test_ingest_prd_file_reads_content_and_metadata() {
    let tmp = TempDir::new().unwrap();
    let prd_path = tmp.path().join("PRD.md");
    std::fs::write(
        &prd_path,
        "# Product Goal\nBuild Squad Autopilot.\n\n# Acceptance Criteria\n- Works.\n",
    )
    .unwrap();

    let document = ingest_prd_file(&prd_path).unwrap();

    assert_eq!(document.path, prd_path);
    assert!(document.display_path.ends_with("PRD.md"));
    assert!(document.content.contains("Build Squad Autopilot."));
    assert_eq!(document.line_count, 5);
    assert_eq!(document.byte_len, document.content.len());
}

#[test]
fn test_ingest_prd_file_rejects_missing_file() {
    let tmp = TempDir::new().unwrap();
    let missing_path = tmp.path().join("missing.md");

    let error = ingest_prd_file(&missing_path).unwrap_err().to_string();

    assert!(error.contains("PRD file does not exist"));
}

#[test]
fn test_ingest_prd_file_rejects_directory() {
    let tmp = TempDir::new().unwrap();

    let error = ingest_prd_file(tmp.path()).unwrap_err().to_string();

    assert!(error.contains("PRD path is not a file"));
}

#[test]
fn test_ingest_prd_file_rejects_empty_content() {
    let tmp = TempDir::new().unwrap();
    let prd_path = tmp.path().join("PRD.md");
    std::fs::write(&prd_path, "  \n\n").unwrap();

    let error = ingest_prd_file(&prd_path).unwrap_err().to_string();

    assert!(error.contains("PRD file is empty"));
}

#[test]
fn test_prd_acceptance_criteria_propagate_to_each_task() {
    // The PRD format defines acceptance criteria run-wide; each parsed task
    // should inherit them so it is neither flagged `missing_acceptance_criteria`
    // nor dispatched to a worker as "Not specified".
    use squad::autopilot::extract_prd_task_graph_basics;

    let prd = "# Product Goal\nBuild it.\n\
\n## Implementation Task Checklist\n\
- [ ] 1. First task\n\
- [ ] 2. Second task\n\
\n## Acceptance Criteria\n\
- Plan reports tasks.\n\
- Run creates one run.\n";
    let graph = extract_prd_task_graph_basics("PRD.md", prd);

    assert_eq!(graph.tasks.len(), 2, "tasks should be parsed");
    assert_eq!(
        graph.acceptance_criteria,
        vec![
            "Plan reports tasks.".to_string(),
            "Run creates one run.".to_string(),
        ],
    );
    for task in &graph.tasks {
        assert_eq!(
            task.acceptance_criteria, graph.acceptance_criteria,
            "task {} should inherit the run-level acceptance criteria",
            task.id,
        );
    }
}

#[test]
fn test_prd_checklist_emits_exactly_ten_tasks_ignoring_completion_notes() {
    // Regression for the autopilot release BLOCK: the `## Completion Notes`
    // heading was unrecognized, so it and its bullets were absorbed as task-11+.
    // The checklist must yield exactly the ten implementation tasks and nothing
    // from Completion Notes (or any other non-task section).
    use squad::autopilot::extract_prd_task_graph_basics;

    let prd = r#"# AMC Test PRD

## Implementation Task Checklist
[x] 1. Confirm the autopilot config loads a 50/50 Claude/Codex model mix - Parallel
[x] 2. Parse this PRD into exactly ten implementation tasks - Parallel
[x] 3. Preserve product goals, milestones, acceptance criteria, tests, and risks - Parallel
[x] 4. Generate specialized worker roles from the PRD context - Parallel
[x] 5. Apply the model mix to generated worker roles - Parallel
[x] 6. Persist the autopilot run, agents, tasks, and terminal sessions - Sequential
[x] 7. Assign ready parallel tasks to available generated agents - Sequential
[x] 8. Produce a dry launch plan without opening terminal windows - Sequential
[x] 9. Record the initialized test checkpoint and changed-file summary - Sequential
[x] 10. Write the final autopilot report and release readiness notes with the model mix and unresolved risks - Sequential

## Completion Notes
- Run 2 validated 10 tasks, 10 generated agents, and a 5 Claude / 5 Codex session split.
- macOS Terminal wet launch opened real Terminal windows.
- Ready autopilot assignments were bridged into normal squad task messages so workers had work in their inboxes.
- Codex startup handling was increased to avoid missed `$squad` command injection.
"#;
    let graph = extract_prd_task_graph_basics("./amc-test-prd.md", prd);

    assert_eq!(graph.tasks.len(), 10, "expected exactly 10 tasks");
    let ids: Vec<&str> = graph.tasks.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "task-1", "task-2", "task-3", "task-4", "task-5", "task-6", "task-7", "task-8",
            "task-9", "task-10",
        ],
    );
    // No heading or Completion Notes bullet leaked into a task title.
    assert!(
        !graph
            .tasks
            .iter()
            .any(|t| t.title.contains("Completion Notes") || t.title.contains("wet launch")),
        "non-task prose must not become a task: {:?}",
        graph.tasks.iter().map(|t| &t.title).collect::<Vec<_>>(),
    );
}

#[test]
fn test_numbered_checklist_lines_outside_task_section_are_ignored() {
    use squad::autopilot::extract_prd_task_graph_basics;

    let prd = r#"
## Completion Notes
[x] 1. This looks like a task but is not in the task checklist.

## Implementation Task Checklist
[x] 1. Real task - Parallel

## Acceptance Criteria
[x] 2. This also looks like a task but is acceptance prose.
"#;

    let graph = extract_prd_task_graph_basics("./PRD.md", prd);

    assert_eq!(graph.tasks.len(), 1);
    assert_eq!(graph.tasks[0].id, "task-1");
    assert_eq!(graph.tasks[0].title, "Real task");
}
