use assert_cmd::Command;
use serde_json::json;
use squad::graph_supervisor::{
    reconcile_ready_graph_nodes, CompiledExecutionGraph, CompiledGraphEdge, CompiledGraphNode,
    EXECUTION_GRAPH_SCHEMA_V1,
};
use squad::store::{ServiceTaskProgressInput, ServiceTaskReportInput, ServiceWorkerInput, Store};

fn hash(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

fn node(id: &str, capability: &str) -> CompiledGraphNode {
    CompiledGraphNode {
        id: id.to_string(),
        kind: "container".to_string(),
        capability: capability.to_string(),
        memory_scopes: vec!["project:test".to_string()],
        route_candidates: vec!["codex://coordinate-worker".to_string()],
        sandbox_profile: Some("workspace-write".to_string()),
        requires: Vec::new(),
        instruction: None,
        budget: json!({"timeout_ms": 60_000}),
    }
}

fn graph() -> CompiledExecutionGraph {
    CompiledExecutionGraph {
        schema: EXECUTION_GRAPH_SCHEMA_V1.to_string(),
        graph_id: "graph:test-cli".to_string(),
        work_hash: hash('a'),
        harness_hash: hash('b'),
        compiler_version: "fractal-harnessc:0.1.0".to_string(),
        target: "darwin".to_string(),
        nodes: vec![
            node("plan", "work.plan"),
            node("implement", "code.write"),
            node("test", "code.test"),
            node("complete", "control.complete"),
        ],
        edges: vec![
            CompiledGraphEdge {
                from: "plan".to_string(),
                to: "implement".to_string(),
                condition: "success".to_string(),
            },
            CompiledGraphEdge {
                from: "plan".to_string(),
                to: "test".to_string(),
                condition: "success".to_string(),
            },
            CompiledGraphEdge {
                from: "implement".to_string(),
                to: "complete".to_string(),
                condition: "success".to_string(),
            },
            CompiledGraphEdge {
                from: "test".to_string(),
                to: "complete".to_string(),
                condition: "success".to_string(),
            },
        ],
        graph_hash: hash('c'),
    }
}

fn graph_source() -> String {
    format!("fractal-graph:graph:test-cli:{}", hash('c'))
}

fn complete_task(store: &Store, task_id: &str) {
    store
        .service_register_worker(ServiceWorkerInput {
            id: "codex-graph-worker".to_string(),
            kind: "codex".to_string(),
            role: "coding_worker".to_string(),
            status: None,
            capacity: Some(1),
            metadata: None,
        })
        .unwrap();
    store
        .service_assign_task(task_id, Some("codex-graph-worker"))
        .unwrap();
    store.service_ack_task(task_id).unwrap();
    store
        .service_report_task(
            task_id,
            ServiceTaskReportInput {
                summary: "node complete".to_string(),
                files_inspected: Vec::new(),
                changed_files: Vec::new(),
                tests_run: vec!["fixture".to_string()],
                verification: "verified fixture evidence".to_string(),
                risks: "none".to_string(),
                raw_report: "passed".to_string(),
            },
        )
        .unwrap();
    store
        .service_verify_task(
            task_id,
            ServiceTaskProgressInput {
                summary: "evidence accepted".to_string(),
                decision: None,
                evidence_hash: None,
                evidence: None,
            },
        )
        .unwrap();
    store.service_complete_task(task_id).unwrap();
}

#[test]
fn graph_walker_enqueues_only_ready_nodes_and_is_idempotent() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(&directory.path().join("coordinate.sqlite3")).unwrap();
    let graph = graph();

    let first = reconcile_ready_graph_nodes(&store, &graph).unwrap();
    assert_eq!(
        first
            .enqueued
            .iter()
            .map(|record| record.node_id.as_str())
            .collect::<Vec<_>>(),
        ["plan"]
    );
    assert_eq!(first.blocked_node_ids, ["implement", "test", "complete"]);

    let repeated = reconcile_ready_graph_nodes(&store, &graph).unwrap();
    assert!(repeated.enqueued.is_empty());
    assert_eq!(store.service_prd_tasks(&graph_source()).unwrap().len(), 1);

    complete_task(&store, &first.enqueued[0].task_id);
    let second = reconcile_ready_graph_nodes(&store, &graph).unwrap();
    assert_eq!(
        second
            .enqueued
            .iter()
            .map(|record| record.node_id.as_str())
            .collect::<Vec<_>>(),
        ["implement", "test"]
    );
    assert_eq!(second.blocked_node_ids, ["complete"]);
    assert_eq!(second.complete_node_ids, ["plan"]);
    assert!(second.enqueued.iter().all(|record| {
        store
            .service_get_task(&record.task_id)
            .unwrap()
            .unwrap()
            .dependencies
            == vec![first.enqueued[0].task_id.clone()]
    }));
}

#[test]
fn graph_walker_executes_the_complete_dag_in_dependency_order() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(&directory.path().join("coordinate.sqlite3")).unwrap();
    let graph = graph();

    let roots = reconcile_ready_graph_nodes(&store, &graph).unwrap();
    assert_eq!(roots.enqueued.len(), 1);
    assert_eq!(roots.enqueued[0].node_id, "plan");
    complete_task(&store, &roots.enqueued[0].task_id);

    let parallel = reconcile_ready_graph_nodes(&store, &graph).unwrap();
    let parallel_ids = parallel
        .enqueued
        .iter()
        .map(|record| record.node_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(parallel_ids, ["implement", "test"]);
    assert_eq!(parallel.blocked_node_ids, ["complete"]);

    let implement = parallel
        .enqueued
        .iter()
        .find(|record| record.node_id == "implement")
        .unwrap();
    complete_task(&store, &implement.task_id);
    let still_blocked = reconcile_ready_graph_nodes(&store, &graph).unwrap();
    assert!(still_blocked.enqueued.is_empty());
    assert_eq!(still_blocked.blocked_node_ids, ["complete"]);

    let test = parallel
        .enqueued
        .iter()
        .find(|record| record.node_id == "test")
        .unwrap();
    complete_task(&store, &test.task_id);
    let join = reconcile_ready_graph_nodes(&store, &graph).unwrap();
    assert_eq!(join.enqueued.len(), 1);
    assert_eq!(join.enqueued[0].node_id, "complete");
    let join_task = store
        .service_get_task(&join.enqueued[0].task_id)
        .unwrap()
        .unwrap();
    let mut expected_dependencies = vec![implement.task_id.clone(), test.task_id.clone()];
    expected_dependencies.sort();
    let mut actual_dependencies = join_task.dependencies.clone();
    actual_dependencies.sort();
    assert_eq!(actual_dependencies, expected_dependencies);

    complete_task(&store, &join.enqueued[0].task_id);
    let finished = reconcile_ready_graph_nodes(&store, &graph).unwrap();
    assert!(finished.enqueued.is_empty());
    assert!(finished.blocked_node_ids.is_empty());
    assert_eq!(
        finished.complete_node_ids,
        ["plan", "implement", "test", "complete"]
    );
    assert_eq!(store.service_prd_tasks(&graph_source()).unwrap().len(), 4);
}

#[test]
fn node_instruction_becomes_the_worker_task_description() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(&directory.path().join("coordinate.sqlite3")).unwrap();
    let mut graph = graph();
    let instruction =
        "Build reverse.py in the workspace so it reverses a string and passes the acceptance test.";
    // Attach a concrete instruction to the root node.
    graph
        .nodes
        .iter_mut()
        .find(|node| node.id == "plan")
        .unwrap()
        .instruction = Some(instruction.to_string());

    let result = reconcile_ready_graph_nodes(&store, &graph).unwrap();
    let plan = result
        .enqueued
        .iter()
        .find(|record| record.node_id == "plan")
        .unwrap();
    let task = store.service_get_task(&plan.task_id).unwrap().unwrap();

    // The worker receives the real instruction, not just "execute node N".
    assert!(
        task.description.contains(instruction),
        "{}",
        task.description
    );
    assert!(task.acceptance_criteria.iter().any(|c| c == instruction));
}

#[test]
fn graph_validation_rejects_cycles_before_queue_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(&directory.path().join("coordinate.sqlite3")).unwrap();
    let mut cyclic = graph();
    cyclic.edges.push(CompiledGraphEdge {
        from: "complete".to_string(),
        to: "plan".to_string(),
        condition: "always".to_string(),
    });

    let error = reconcile_ready_graph_nodes(&store, &cyclic).unwrap_err();
    assert!(error.to_string().contains("cycle"));
    assert!(store.service_list_tasks(None, None).unwrap().is_empty());
}

#[test]
fn graph_validation_accepts_explicit_host_worker_kinds() {
    for provider in ["cursor", "codex", "claude"] {
        let mut provider_graph = graph();
        provider_graph.nodes[0].kind = provider.to_string();
        provider_graph.nodes[0].route_candidates = vec![format!("{provider}://coordinate-worker")];
        provider_graph
            .validate()
            .unwrap_or_else(|error| panic!("{provider} should be valid: {error}"));
    }
}

#[test]
fn coordinate_binary_reconciles_a_compiled_graph_into_the_pull_queue() {
    let directory = tempfile::tempdir().unwrap();
    let graph_path = directory.path().join("graph.json");
    let database_path = directory.path().join("coordinate.sqlite3");
    std::fs::write(&graph_path, serde_json::to_vec(&graph()).unwrap()).unwrap();

    let output = Command::cargo_bin("squad")
        .unwrap()
        .args([
            "graph-supervisor",
            "--graph",
            graph_path.to_str().unwrap(),
            "--db",
            database_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["enqueued"][0]["nodeId"], "plan");
    assert_eq!(result["blockedNodeIds"].as_array().unwrap().len(), 3);
    let store = Store::open(&database_path).unwrap();
    let tasks = store.service_prd_tasks(&graph_source()).unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].source_task_number.as_deref(), Some("plan"));
}
