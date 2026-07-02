use squad::autopilot::{
    classify_task_graph_statuses, extract_prd_task_graph_basics, synthesize_role_specs_from_prd,
    write_science_swarm_artifacts, PrdRoleContext, RiskLevel, TaskGraph, TaskGraphStatus,
    TaskGraphTask,
};
use tempfile::TempDir;

fn graph_task(id: &str, status: TaskGraphStatus, depends_on: Vec<String>) -> TaskGraphTask {
    TaskGraphTask {
        id: id.to_string(),
        title: format!("Task {id}"),
        description: "Do the work".to_string(),
        assigned_role: Some("rust_backend".to_string()),
        status,
        priority: 10,
        risk_level: RiskLevel::Medium,
        acceptance_criteria: vec!["Behavior is covered by tests".to_string()],
        likely_files: vec!["src/main.rs".to_string()],
        test_requirements: vec!["cargo test".to_string()],
        depends_on,
    }
}

#[test]
fn test_extract_science_swarm_prd_tasks_and_spawn_plan() {
    let content = r#"
PRD: BioLatent Science Swarm

3. Product Goal
A user gives one prompt to the manager and receives a verified evidence bundle.

21. New Build Checklist
T001 [SERIES] Inspect current BioLatent codebase and confirm which checked tasks are actually implemented.
T002 [SERIES] Inspect current Fractalwork router, task graph, model provider, and trace schema.
T004 [PARALLEL] Identify existing CLI command patterns.
T025 [PARALLEL] Add background evidence search task template.
T058 [PARALLEL] Spawn Codex workers for coding tasks.
T079 [SERIES] Add biolatent local-model install.
T090 [SERIES] Finish dual-use refusal path.
T094 [SERIES] Implement adversarial critic across process boundary.
T107 [SERIES] Generate final scientific report.
"#;

    let graph = extract_prd_task_graph_basics("science-swarm.rtf", content);

    assert_eq!(graph.tasks.len(), 9);
    assert_eq!(
        graph.objective,
        "A user gives one prompt to the manager and receives a verified evidence bundle."
    );
    assert_eq!(graph.tasks[0].id, "task-1");
    assert_eq!(graph.tasks[0].status, TaskGraphStatus::Sequential);
    assert_eq!(graph.tasks[2].id, "task-4");
    assert_eq!(graph.tasks[2].status, TaskGraphStatus::ReadyParallel);
    assert_eq!(
        graph.tasks[3].assigned_role.as_deref(),
        Some("literature_worker")
    );
    assert_eq!(graph.tasks[5].assigned_role.as_deref(), Some("router"));
    assert_eq!(
        graph.tasks[7].assigned_role.as_deref(),
        Some("adversarial_critic")
    );
    assert!(!graph.hypotheses.is_empty());
    assert!(!graph.parallel_groups.is_empty());
    assert!(graph.spawn_plan.providers.contains_key("openrouter_free"));
    graph.validate().unwrap();
}

#[test]
fn test_write_science_swarm_artifacts_creates_evidence_bundle_files() {
    let tmp = TempDir::new().unwrap();
    let graph = extract_prd_task_graph_basics(
        "science-swarm.md",
        r#"
Product Goal
Run a science swarm.

21. New Build Checklist
T001 [SERIES] Generate falsifiable hypotheses.
T002 [SERIES] Freeze preregistered protocol.
T003 [PARALLEL] Run independent verification checks.
"#,
    );
    let context = PrdRoleContext {
        prd_path: graph.prd_path.clone(),
        product_goal: graph.product_goals.join("\n"),
        implementation_tasks: graph.tasks.iter().map(|task| task.title.clone()).collect(),
        ..PrdRoleContext::default()
    };
    let specs = synthesize_role_specs_from_prd(&context);

    let paths = write_science_swarm_artifacts(tmp.path(), &graph, &specs).unwrap();

    assert_eq!(paths.len(), 9);
    let plan = std::fs::read_to_string(tmp.path().join(".squad/autopilot/plan.md")).unwrap();
    assert!(plan.contains("# BioLatent Science Swarm Plan"));
    assert!(plan.contains("- [x] task-1"));
    let protocol =
        std::fs::read_to_string(tmp.path().join(".squad/autopilot/frozen_protocol.json")).unwrap();
    assert!(protocol.contains("hypotheses_not_claims"));
    let crate_manifest =
        std::fs::read_to_string(tmp.path().join(".squad/autopilot/ro-crate-metadata.json"))
            .unwrap();
    assert!(crate_manifest.contains("BioLatent Science Swarm Evidence Bundle"));
}

#[test]
fn test_task_graph_format_serializes_statuses_and_dependencies() {
    let graph = TaskGraph {
        prd_path: "./PRD.md".to_string(),
        product_goals: vec!["Automate PRD-driven squad execution".to_string()],
        milestones: vec!["MVP 3: Task Graph".to_string()],
        acceptance_criteria: vec!["Plan command shows blocked tasks".to_string()],
        test_requirements: vec!["cargo test".to_string()],
        risky_areas: vec!["Terminal spawning".to_string()],
        tasks: vec![
            graph_task("schema", TaskGraphStatus::Sequential, Vec::new()),
            graph_task("plan", TaskGraphStatus::Blocked, vec!["schema".to_string()]),
            graph_task("docs", TaskGraphStatus::ReadyParallel, Vec::new()),
        ],
        ..TaskGraph::default()
    };

    graph.validate().unwrap();
    let json = serde_json::to_string_pretty(&graph).unwrap();

    assert!(json.contains(r#""prd_path": "./PRD.md""#));
    assert!(json.contains(r#""status": "SEQUENTIAL""#));
    assert!(json.contains(r#""status": "BLOCKED""#));
    assert!(json.contains(r#""status": "READY_PARALLEL""#));
    assert!(json.contains(r#""risk_level": "medium""#));
    assert!(json.contains(r#""acceptance_criteria": ["#));
    assert!(json.contains("Plan command shows blocked tasks"));
    assert!(json.contains(r#""test_requirements": ["#));
    assert!(json.contains("cargo test"));
    assert!(json.contains(r#""risky_areas": ["#));
    assert!(json.contains(r#""depends_on": ["#));

    let parsed: TaskGraph = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, graph);
}

#[test]
fn test_task_graph_validation_rejects_duplicate_ids() {
    let graph = TaskGraph {
        tasks: vec![
            graph_task("schema", TaskGraphStatus::Sequential, Vec::new()),
            graph_task("schema", TaskGraphStatus::ReadyParallel, Vec::new()),
        ],
        ..TaskGraph::default()
    };

    let error = graph.validate().unwrap_err().to_string();

    assert!(error.contains("duplicate task graph task id: schema"));
}

#[test]
fn test_task_graph_validation_rejects_missing_dependency() {
    let graph = TaskGraph {
        tasks: vec![graph_task(
            "plan",
            TaskGraphStatus::Blocked,
            vec!["schema".to_string()],
        )],
        ..TaskGraph::default()
    };

    let error = graph.validate().unwrap_err().to_string();

    assert!(error.contains("depends on missing task 'schema'"));
}

#[test]
fn test_task_graph_validation_rejects_self_dependency() {
    let graph = TaskGraph {
        tasks: vec![graph_task(
            "schema",
            TaskGraphStatus::Blocked,
            vec!["schema".to_string()],
        )],
        ..TaskGraph::default()
    };

    let error = graph.validate().unwrap_err().to_string();

    assert!(error.contains("cannot depend on itself"));
}

#[test]
fn test_classify_task_graph_statuses_covers_ready_blocked_review_done_and_failed() {
    let mut review_task = graph_task("review", TaskGraphStatus::Sequential, Vec::new());
    review_task.risk_level = RiskLevel::High;
    let mut graph = TaskGraph {
        tasks: vec![
            graph_task("done", TaskGraphStatus::Done, Vec::new()),
            graph_task(
                "parallel",
                TaskGraphStatus::Blocked,
                vec!["done".to_string()],
            ),
            graph_task(
                "blocked",
                TaskGraphStatus::ReadyParallel,
                vec!["parallel".to_string()],
            ),
            graph_task("sequential", TaskGraphStatus::Sequential, Vec::new()),
            review_task,
            graph_task("failed", TaskGraphStatus::Failed, Vec::new()),
        ],
        ..TaskGraph::default()
    };

    classify_task_graph_statuses(&mut graph).unwrap();

    assert_eq!(graph.tasks[0].status, TaskGraphStatus::Done);
    assert_eq!(graph.tasks[1].status, TaskGraphStatus::ReadyParallel);
    assert_eq!(graph.tasks[2].status, TaskGraphStatus::Blocked);
    assert_eq!(graph.tasks[3].status, TaskGraphStatus::Sequential);
    assert_eq!(graph.tasks[4].status, TaskGraphStatus::ReviewRequired);
    assert_eq!(graph.tasks[5].status, TaskGraphStatus::Failed);
}

#[test]
fn test_classify_task_graph_statuses_rejects_invalid_graph() {
    let mut graph = TaskGraph {
        tasks: vec![graph_task(
            "blocked",
            TaskGraphStatus::ReadyParallel,
            vec!["missing".to_string()],
        )],
        ..TaskGraph::default()
    };

    let error = classify_task_graph_statuses(&mut graph)
        .unwrap_err()
        .to_string();

    assert!(error.contains("depends on missing task 'missing'"));
}

#[test]
fn test_extract_prd_task_graph_basics_from_autopilot_prd_shape() {
    let content = r#"
Product Goal
Create an automated AI build manager that reads a PRD and supervises execution.
Working name:
Squad Autopilot

MVP Milestones
MVP 1: Config + Schema
Add squad autopilot init
MVP 2: Team Generator
Generate role prompts from PRD

Implementation Task Checklist
[x] 16. Define task graph extraction format - Parallel
[ ] 17. Extract product goals, milestones, implementation tasks - Parallel
[ ] 18. Extract acceptance criteria and test requirements - Parallel
[ ] 19. Extract dependencies and risky areas - Parallel
[ ] 20. Store extracted tasks in autopilot_tasks - Sequential

Risky Areas
Terminal spawning
SQLite migrations

Dependencies
20 depends on 17, 18, 19
"#;

    let graph = extract_prd_task_graph_basics("./PRD.md", content);

    assert_eq!(graph.prd_path, "./PRD.md");
    assert_eq!(
        graph.product_goals,
        vec!["Create an automated AI build manager that reads a PRD and supervises execution."]
    );
    assert_eq!(
        graph.milestones,
        vec![
            "MVP 1: Config + Schema".to_string(),
            "MVP 2: Team Generator".to_string()
        ]
    );
    assert_eq!(
        graph.risky_areas,
        vec![
            "Terminal spawning".to_string(),
            "SQLite migrations".to_string()
        ]
    );
    assert_eq!(graph.tasks.len(), 5);
    assert_eq!(graph.tasks[0].id, "task-16");
    assert_eq!(graph.tasks[0].title, "Define task graph extraction format");
    assert_eq!(graph.tasks[0].status, TaskGraphStatus::ReadyParallel);
    assert_eq!(
        graph.tasks[1].title,
        "Extract product goals, milestones, implementation tasks"
    );
    assert_eq!(graph.tasks[1].status, TaskGraphStatus::ReadyParallel);
    assert_eq!(graph.tasks[2].id, "task-18");
    assert_eq!(graph.tasks[3].id, "task-19");
    assert_eq!(graph.tasks[4].id, "task-20");
    assert_eq!(graph.tasks[4].status, TaskGraphStatus::Sequential);
    assert_eq!(
        graph.tasks[4].depends_on,
        vec![
            "task-17".to_string(),
            "task-18".to_string(),
            "task-19".to_string()
        ]
    );
    graph.validate().unwrap();
}

#[test]
fn test_extract_prd_task_graph_basics_from_markdown_sections() {
    let content = r#"
## Product Goals
- Ship a PRD-driven manager loop.
- Keep existing squad commands usable.

## Milestones
- MVP 1: Config + Schema
- MVP 3: Task Graph

## Implementation Tasks
- Parse the PRD
- Build the graph

## Risks
- Terminal spawning can fail.

## Dependencies
- 2 depends on 1
"#;

    let graph = extract_prd_task_graph_basics("docs/PRD.md", content);

    assert_eq!(
        graph.product_goals,
        vec![
            "Ship a PRD-driven manager loop.".to_string(),
            "Keep existing squad commands usable.".to_string()
        ]
    );
    assert_eq!(
        graph.milestones,
        vec![
            "MVP 1: Config + Schema".to_string(),
            "MVP 3: Task Graph".to_string()
        ]
    );
    assert_eq!(graph.tasks.len(), 2);
    assert_eq!(graph.tasks[0].id, "task-1");
    assert_eq!(graph.tasks[0].title, "Parse the PRD");
    assert_eq!(graph.tasks[0].status, TaskGraphStatus::Sequential);
    assert_eq!(graph.tasks[1].id, "task-2");
    assert_eq!(graph.tasks[1].title, "Build the graph");
    assert_eq!(
        graph.risky_areas,
        vec!["Terminal spawning can fail.".to_string()]
    );
    assert_eq!(graph.tasks[1].depends_on, vec!["task-1".to_string()]);
}

#[test]
fn test_extract_prd_task_graph_basics_from_main_sequential_chain() {
    let content = r#"
Implementation Task Checklist
[ ] 1. Create schema - Sequential
[ ] 2. Generate team - Parallel
[ ] 3. Store tasks - Parallel

Main sequential chain: 1 -> 2 -> 3
"#;

    let graph = extract_prd_task_graph_basics("./PRD.md", content);

    assert_eq!(graph.tasks[0].depends_on, Vec::<String>::new());
    assert_eq!(graph.tasks[1].depends_on, vec!["task-1".to_string()]);
    assert_eq!(graph.tasks[1].status, TaskGraphStatus::Blocked);
    assert_eq!(graph.tasks[2].depends_on, vec!["task-2".to_string()]);
    assert_eq!(graph.tasks[2].status, TaskGraphStatus::Blocked);
    graph.validate().unwrap();
}

#[test]
fn test_extract_prd_task_graph_basics_includes_acceptance_and_tests() {
    let content = r#"
## Product Goals
- Build Squad Autopilot.

## Acceptance Criteria
- `squad autopilot plan ./PRD.md` shows parallel and blocked tasks.
- Final report includes files changed.

## Test Requirements
- Run `cargo test`.
- Add integration coverage for the plan command.

## Implementation Tasks
- Build the graph
"#;

    let graph = extract_prd_task_graph_basics("./PRD.md", content);

    assert_eq!(
        graph.acceptance_criteria,
        vec![
            "`squad autopilot plan ./PRD.md` shows parallel and blocked tasks.".to_string(),
            "Final report includes files changed.".to_string()
        ]
    );
    assert_eq!(
        graph.test_requirements,
        vec![
            "Run `cargo test`.".to_string(),
            "Add integration coverage for the plan command.".to_string()
        ]
    );
    assert_eq!(graph.tasks.len(), 1);
    assert_eq!(graph.tasks[0].title, "Build the graph");
}
