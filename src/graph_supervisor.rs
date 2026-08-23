//! Compiled execution-graph walker backed by Coordinate's existing pull queue.
//!
//! A reconciliation pass creates only graph nodes whose incoming dependencies
//! have completed. Stable task ids make repeated or concurrent passes
//! idempotent; workers continue to claim through the normal lease queue.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::store::{ServiceTaskInput, ServiceTaskRecord, Store};

pub const EXECUTION_GRAPH_SCHEMA_V1: &str = "fractal.execution_graph.v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompiledExecutionGraph {
    pub schema: String,
    pub graph_id: String,
    pub work_hash: String,
    pub harness_hash: String,
    pub compiler_version: String,
    pub target: String,
    pub nodes: Vec<CompiledGraphNode>,
    pub edges: Vec<CompiledGraphEdge>,
    pub graph_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompiledGraphNode {
    pub id: String,
    pub kind: String,
    pub capability: String,
    pub memory_scopes: Vec<String>,
    #[serde(default)]
    pub route_candidates: Vec<String>,
    pub sandbox_profile: Option<String>,
    #[serde(default)]
    pub requires: Vec<String>,
    /// Concrete, human-readable instruction for the worker executing this node
    /// (carried from the compiled graph). When present it becomes the worker
    /// task's description so the worker performs the real work, not an abstract
    /// "execute node N".
    #[serde(default)]
    pub instruction: Option<String>,
    pub budget: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompiledGraphEdge {
    pub from: String,
    pub to: String,
    pub condition: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphReconcileResult {
    pub graph_id: String,
    pub graph_hash: String,
    pub enqueued: Vec<GraphNodeQueueRecord>,
    pub already_enqueued: Vec<GraphNodeQueueRecord>,
    pub blocked_node_ids: Vec<String>,
    pub complete_node_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphNodeQueueRecord {
    pub node_id: String,
    pub task_id: String,
}

impl CompiledExecutionGraph {
    pub fn validate(&self) -> Result<()> {
        if self.schema != EXECUTION_GRAPH_SCHEMA_V1 {
            anyhow::bail!("unsupported execution graph schema {}", self.schema);
        }
        for (field, value) in [
            ("graph_id", self.graph_id.as_str()),
            ("compiler_version", self.compiler_version.as_str()),
            ("target", self.target.as_str()),
        ] {
            if value.trim().is_empty() {
                anyhow::bail!("execution graph {field} cannot be empty");
            }
        }
        for (field, value) in [
            ("work_hash", self.work_hash.as_str()),
            ("harness_hash", self.harness_hash.as_str()),
            ("graph_hash", self.graph_hash.as_str()),
        ] {
            validate_hash(field, value)?;
        }
        if self.nodes.is_empty() {
            anyhow::bail!("execution graph nodes cannot be empty");
        }
        let mut node_ids = BTreeSet::new();
        for node in &self.nodes {
            if node.id.trim().is_empty() || node.capability.trim().is_empty() {
                anyhow::bail!("execution graph node id and capability cannot be empty");
            }
            if !matches!(
                node.kind.as_str(),
                "inference"
                    | "container"
                    | "verification"
                    | "retrieval"
                    | "human"
                    | "control"
                    | "cursor"
                    | "codex"
                    | "claude"
            ) {
                anyhow::bail!(
                    "execution graph node {} has invalid kind {}",
                    node.id,
                    node.kind
                );
            }
            if !node.budget.is_object() {
                anyhow::bail!("execution graph node {} budget must be an object", node.id);
            }
            if !node_ids.insert(node.id.as_str()) {
                anyhow::bail!("duplicate execution graph node {}", node.id);
            }
        }
        for edge in &self.edges {
            if !node_ids.contains(edge.from.as_str()) {
                anyhow::bail!("execution graph edge has unknown source {}", edge.from);
            }
            if !node_ids.contains(edge.to.as_str()) {
                anyhow::bail!("execution graph edge has unknown target {}", edge.to);
            }
            if edge.from == edge.to {
                anyhow::bail!("execution graph node {} depends on itself", edge.from);
            }
            if edge.condition.trim().is_empty() {
                anyhow::bail!("execution graph edge condition cannot be empty");
            }
        }
        ensure_acyclic(self, &node_ids)
    }
}

/// Reconcile one immutable graph snapshot into ready pull-queue tasks.
pub fn reconcile_ready_graph_nodes(
    store: &Store,
    graph: &CompiledExecutionGraph,
) -> Result<GraphReconcileResult> {
    graph.validate()?;
    let source = graph_source(graph);
    let existing = store.service_prd_tasks(&source)?;
    let mut tasks_by_node = BTreeMap::<String, ServiceTaskRecord>::new();
    for task in existing {
        let node_id = task
            .source_task_number
            .clone()
            .context("graph queue task is missing source node id")?;
        if tasks_by_node.insert(node_id.clone(), task).is_some() {
            anyhow::bail!("graph node {node_id} has duplicate queue tasks");
        }
    }

    let dependencies = graph_dependencies(graph);
    let mut enqueued = Vec::new();
    let mut already_enqueued = Vec::new();
    let mut blocked_node_ids = Vec::new();
    let mut complete_node_ids = Vec::new();
    for node in &graph.nodes {
        if let Some(task) = tasks_by_node.get(&node.id) {
            let record = GraphNodeQueueRecord {
                node_id: node.id.clone(),
                task_id: task.id.clone(),
            };
            if task.state == "complete" {
                complete_node_ids.push(node.id.clone());
            }
            already_enqueued.push(record);
            continue;
        }
        let predecessor_ids = dependencies.get(&node.id).cloned().unwrap_or_default();
        let predecessor_tasks = predecessor_ids
            .iter()
            .map(|node_id| tasks_by_node.get(node_id))
            .collect::<Option<Vec<_>>>();
        let ready = predecessor_tasks
            .as_ref()
            .is_some_and(|tasks| tasks.iter().all(|task| task.state == "complete"));
        if !ready {
            blocked_node_ids.push(node.id.clone());
            continue;
        }

        let dependency_task_ids = predecessor_tasks
            .unwrap_or_default()
            .into_iter()
            .map(|task| task.id.clone())
            .collect::<Vec<_>>();
        let task_id = stable_graph_task_id(&graph.graph_hash, &node.id);
        let task = match store.service_create_task_with_id(
            task_id.clone(),
            graph_node_task_input(graph, node, dependency_task_ids),
        ) {
            Ok(task) => task,
            Err(error) => store
                .service_get_task(&task_id)?
                .filter(|task| {
                    task.source_prd_path == source
                        && task.source_task_number.as_deref() == Some(node.id.as_str())
                })
                .with_context(|| format!("failed to enqueue graph node {}: {error}", node.id))?,
        };
        tasks_by_node.insert(node.id.clone(), task);
        enqueued.push(GraphNodeQueueRecord {
            node_id: node.id.clone(),
            task_id,
        });
    }
    Ok(GraphReconcileResult {
        graph_id: graph.graph_id.clone(),
        graph_hash: graph.graph_hash.clone(),
        enqueued,
        already_enqueued,
        blocked_node_ids,
        complete_node_ids,
    })
}

fn graph_node_task_input(
    graph: &CompiledExecutionGraph,
    node: &CompiledGraphNode,
    dependencies: Vec<String>,
) -> ServiceTaskInput {
    let description = match node.instruction.as_deref() {
        Some(instruction) if !instruction.trim().is_empty() => format!(
            "{instruction}\n\n(Execute compiled graph node {} ({}) for graph {} under its lease and sandbox.)",
            node.id, node.kind, graph.graph_id
        ),
        _ => format!(
            "Execute compiled graph node {} ({}) for graph {} under its lease and sandbox.",
            node.id, node.kind, graph.graph_id
        ),
    };
    let mut acceptance_criteria = Vec::new();
    if let Some(instruction) = node.instruction.as_deref() {
        if !instruction.trim().is_empty() {
            acceptance_criteria.push(instruction.trim().to_string());
        }
    }
    acceptance_criteria.push(format!(
        "Capability {} completes successfully.",
        node.capability
    ));
    acceptance_criteria.push("Required evidence is reported to Coordinate.".to_string());
    ServiceTaskInput {
        title: format!("Graph node {} · {}", node.id, node.capability),
        description,
        acceptance_criteria,
        source_prd_path: graph_source(graph),
        source_task_number: Some(node.id.clone()),
        priority: 0,
        preferred_model: preferred_provider(node).to_string(),
        role: "coding_worker".to_string(),
        parallelizable: true,
        max_attempts: Some(3),
        dependencies: Some(dependencies),
    }
}

fn preferred_provider(node: &CompiledGraphNode) -> &'static str {
    for candidate in &node.route_candidates {
        match candidate.split("://").next().unwrap_or_default() {
            "cursor" => return "cursor",
            "codex" => return "codex",
            "claude" => return "claude",
            "local" | "mlx" | "control" => return "local",
            _ => {}
        }
    }
    match node.kind.as_str() {
        "control" | "retrieval" => "local",
        _ => "codex",
    }
}

fn graph_source(graph: &CompiledExecutionGraph) -> String {
    format!("fractal-graph:{}:{}", graph.graph_id, graph.graph_hash)
}

fn stable_graph_task_id(graph_hash: &str, node_id: &str) -> String {
    let digest = Sha256::digest(format!("{graph_hash}\0{node_id}"));
    format!("graph-node:{digest:x}")
}

fn graph_dependencies(graph: &CompiledExecutionGraph) -> BTreeMap<String, Vec<String>> {
    let mut dependencies = BTreeMap::<String, Vec<String>>::new();
    for edge in &graph.edges {
        dependencies
            .entry(edge.to.clone())
            .or_default()
            .push(edge.from.clone());
    }
    for values in dependencies.values_mut() {
        values.sort();
        values.dedup();
    }
    dependencies
}

fn ensure_acyclic(graph: &CompiledExecutionGraph, node_ids: &BTreeSet<&str>) -> Result<()> {
    let dependencies = graph_dependencies(graph);
    let mut remaining = node_ids
        .iter()
        .map(|id| (*id).to_string())
        .collect::<BTreeSet<_>>();
    loop {
        let ready = remaining
            .iter()
            .filter(|node_id| {
                dependencies.get(*node_id).is_none_or(|deps| {
                    deps.iter()
                        .all(|dependency| !remaining.contains(dependency))
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        if ready.is_empty() {
            break;
        }
        for node_id in ready {
            remaining.remove(&node_id);
        }
    }
    if remaining.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(
            "execution graph contains a cycle involving {}",
            remaining.into_iter().collect::<Vec<_>>().join(",")
        )
    }
}

fn validate_hash(field: &str, value: &str) -> Result<()> {
    let hex = value
        .strip_prefix("sha256:")
        .with_context(|| format!("{field} must use sha256"))?;
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        anyhow::bail!("{field} must be lowercase canonical sha256");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// INT-084 — live execution path
//
// Everything below wires the INT-078 lease-boundary contract into the actual
// Coordinate execution path (HTTP router and `squad graph-supervisor`): a
// compiled graph is validated, persisted durably, reconciled into the pull
// queue, leased, reported, independently verified or retried, and projected to
// a terminal state.
// ---------------------------------------------------------------------------

pub const GRAPH_LIVE_PATH_SCHEMA_V1: &str = "coordinate.graph_live_path.v1";

/// Verifier decision applied through the live path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VerifierDecision {
    Accept,
    Reject,
}

/// Result of compiling and persisting one graph through the live path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphCompileResult {
    pub schema: String,
    pub graph_id: String,
    pub graph_hash: String,
    pub graph_source: String,
    /// False when the identical graph was already persisted (idempotent replay).
    pub persisted: bool,
    pub content_hash: String,
    pub reconcile: GraphReconcileResult,
    pub projection: GraphTerminalProjection,
}

/// Deterministic outcome of one worker report on a graph node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphNodeReportOutcome {
    pub schema: String,
    pub graph_id: String,
    pub graph_hash: String,
    pub node_id: String,
    pub task_id: String,
    pub lease_owner: String,
    pub report_hash: String,
    /// True when this report repeated an already-recorded identical report.
    pub duplicate: bool,
    pub task: ServiceTaskRecord,
}

/// Outcome of one independent verification of a reported graph node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphNodeVerificationOutcome {
    pub schema: String,
    pub graph_id: String,
    pub graph_hash: String,
    pub node_id: String,
    pub task_id: String,
    pub verifier_id: String,
    pub decision: VerifierDecision,
    pub evidence_hash: String,
    pub attempt: i64,
    pub max_attempts: i64,
    /// True when a rejection consumed the last retry and escalated the node.
    pub retries_exhausted: bool,
    pub binding: crate::fractal_runtime::GraphSupervisorLeaseBinding,
    pub task: ServiceTaskRecord,
    pub reconcile: GraphReconcileResult,
    pub projection: GraphTerminalProjection,
}

/// Outcome of recovering expired leases for one graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphLeaseRecoveryOutcome {
    pub schema: String,
    pub graph_id: String,
    pub graph_hash: String,
    pub recovered: crate::store::ServiceQueueReapResult,
    pub reconcile: GraphReconcileResult,
    pub projection: GraphTerminalProjection,
}

/// Terminal projection of one node in the live path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphNodeProjection {
    pub node_id: String,
    pub task_id: Option<String>,
    /// `pending` until the node is enqueued, otherwise the queue task state.
    pub state: String,
    pub terminal: bool,
    pub attempt: i64,
    pub max_attempts: i64,
    pub lease_owner: Option<String>,
    pub lease_expires_at: Option<String>,
    pub report_hash: Option<String>,
    pub evidence_hash: Option<String>,
    pub last_error: Option<String>,
}

/// Terminal projection of a whole graph in the live path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphTerminalProjection {
    pub schema: String,
    pub graph_id: String,
    pub graph_hash: String,
    pub graph_source: String,
    pub node_count: usize,
    pub complete: usize,
    pub failed: usize,
    pub blocked: usize,
    pub in_flight: usize,
    pub pending: usize,
    /// True when no further progress is possible without operator action.
    pub terminal: bool,
    /// `pending`, `running`, `complete`, or `failed`.
    pub status: String,
    pub nodes: Vec<GraphNodeProjection>,
}

/// Content hash of one compiled graph, used to make persistence idempotent and
/// to reject a different body replayed under the same graph hash.
pub fn graph_content_hash(graph: &CompiledExecutionGraph) -> Result<String> {
    let encoded = serde_json::to_vec(graph).context("failed to encode compiled execution graph")?;
    let digest = Sha256::digest(encoded);
    Ok(format!("sha256:{digest:x}"))
}

/// Queue source key for one compiled graph.
pub fn graph_source_key(graph: &CompiledExecutionGraph) -> String {
    graph_source(graph)
}

/// Validate, durably persist, and reconcile one compiled graph.
///
/// This is the live path's entry point: it is the only place that admits a new
/// compiled graph, and it is idempotent under replay of the identical graph.
pub fn compile_and_persist_graph(
    store: &Store,
    graph: &CompiledExecutionGraph,
) -> Result<GraphCompileResult> {
    graph.validate()?;
    let content_hash = graph_content_hash(graph)?;
    let source = graph_source(graph);
    let outcome = store.service_persist_execution_graph(crate::store::ExecutionGraphInput {
        graph_id: graph.graph_id.clone(),
        graph_hash: graph.graph_hash.clone(),
        graph_source: source.clone(),
        schema: graph.schema.clone(),
        work_hash: graph.work_hash.clone(),
        harness_hash: graph.harness_hash.clone(),
        compiler_version: graph.compiler_version.clone(),
        target: graph.target.clone(),
        node_count: graph.nodes.len() as i64,
        content_hash: content_hash.clone(),
        document: serde_json::to_value(graph)
            .context("failed to encode compiled execution graph")?,
    })?;
    let reconcile = reconcile_ready_graph_nodes(store, graph)?;
    let projection = project_graph_terminal_state(store, graph)?;
    Ok(GraphCompileResult {
        schema: GRAPH_LIVE_PATH_SCHEMA_V1.to_string(),
        graph_id: graph.graph_id.clone(),
        graph_hash: graph.graph_hash.clone(),
        graph_source: source,
        persisted: outcome.created,
        content_hash,
        reconcile,
        projection,
    })
}

/// Load a previously persisted compiled graph by hash.
pub fn load_persisted_graph(store: &Store, graph_hash: &str) -> Result<CompiledExecutionGraph> {
    let persisted = store
        .service_get_execution_graph(graph_hash)?
        .with_context(|| format!("execution graph does not exist: {graph_hash}"))?;
    let graph: CompiledExecutionGraph = serde_json::from_value(persisted.document)
        .with_context(|| format!("persisted execution graph {graph_hash} is not decodable"))?;
    graph.validate()?;
    Ok(graph)
}

/// Reconcile a persisted graph and project its terminal state.
pub fn advance_persisted_graph(store: &Store, graph_hash: &str) -> Result<GraphCompileResult> {
    let graph = load_persisted_graph(store, graph_hash)?;
    let reconcile = reconcile_ready_graph_nodes(store, &graph)?;
    let projection = project_graph_terminal_state(store, &graph)?;
    Ok(GraphCompileResult {
        schema: GRAPH_LIVE_PATH_SCHEMA_V1.to_string(),
        graph_id: graph.graph_id.clone(),
        graph_hash: graph.graph_hash.clone(),
        graph_source: graph_source(&graph),
        persisted: false,
        content_hash: graph_content_hash(&graph)?,
        reconcile,
        projection,
    })
}

/// Resolve the queue task backing one node of a persisted graph.
fn require_node_task(
    store: &Store,
    graph: &CompiledExecutionGraph,
    node_id: &str,
) -> Result<ServiceTaskRecord> {
    if !graph.nodes.iter().any(|node| node.id == node_id) {
        anyhow::bail!("graph {} has no node {node_id}", graph.graph_id);
    }
    let task_id = stable_graph_task_id(&graph.graph_hash, node_id);
    store
        .service_get_task(&task_id)?
        .filter(|task| task.source_prd_path == graph_source(graph))
        .with_context(|| format!("graph node {node_id} is not enqueued yet"))
}

/// Take the dependency-aware exclusive lease on one graph node.
pub fn checkout_graph_node_lease(
    store: &Store,
    graph_hash: &str,
    node_id: &str,
    worker_id: &str,
    lease_secs: i64,
) -> Result<crate::store::GraphNodeLeaseCheckout> {
    let graph = load_persisted_graph(store, graph_hash)?;
    let task = require_node_task(store, &graph, node_id)?;
    store.service_checkout_graph_node_lease(worker_id, &task.id, lease_secs)
}

/// Record one worker report against a leased graph node.
///
/// Duplicate delivery is deterministic: an identical report replayed by the
/// same lease owner returns the recorded report with `duplicate: true` and
/// changes no state, while a *different* body after a report is a conflict.
pub fn report_graph_node(
    store: &Store,
    graph_hash: &str,
    node_id: &str,
    worker_id: &str,
    input: crate::store::ServiceTaskReportInput,
) -> Result<GraphNodeReportOutcome> {
    let graph = load_persisted_graph(store, graph_hash)?;
    let task = require_node_task(store, &graph, node_id)?;
    let expected_report_hash = crate::store::service_report_input_hash(&input)?;
    let lease_owner = task
        .lease_owner
        .clone()
        .with_context(|| format!("graph node {node_id} has no active lease to report against"))?;
    if lease_owner != worker_id {
        anyhow::bail!("graph node {node_id} is leased by {lease_owner}, not {worker_id}");
    }

    if task.state == "reported" || task.state == "verified" {
        let recorded_report_hash = task.report_hash.as_deref().with_context(|| {
            format!("graph node {node_id} is reported without a stored report hash")
        })?;
        if recorded_report_hash != expected_report_hash {
            anyhow::bail!(
                "graph node {node_id} conflicts with the report already recorded for this attempt"
            );
        }
        return Ok(GraphNodeReportOutcome {
            schema: GRAPH_LIVE_PATH_SCHEMA_V1.to_string(),
            graph_id: graph.graph_id.clone(),
            graph_hash: graph.graph_hash.clone(),
            node_id: node_id.to_string(),
            task_id: task.id.clone(),
            lease_owner,
            report_hash: task.report_hash.clone().unwrap_or_default(),
            duplicate: true,
            task,
        });
    }

    if task.state == "acked" {
        store.service_start_task(&task.id)?;
    }
    let reported = store.service_report_task(&task.id, input)?;
    Ok(GraphNodeReportOutcome {
        schema: GRAPH_LIVE_PATH_SCHEMA_V1.to_string(),
        graph_id: graph.graph_id.clone(),
        graph_hash: graph.graph_hash.clone(),
        node_id: node_id.to_string(),
        task_id: reported.id.clone(),
        lease_owner,
        report_hash: reported.report_hash.clone().unwrap_or_default(),
        duplicate: false,
        task: reported,
    })
}

/// Apply an independent verifier decision to a reported graph node.
///
/// The verifier must not be the lease owner. Acceptance persists the handoff
/// evidence and completes the node so successors become ready; rejection
/// consumes one bounded retry and escalates when the budget is exhausted.
pub fn verify_graph_node(
    store: &Store,
    graph_hash: &str,
    node_id: &str,
    verifier_id: &str,
    decision: VerifierDecision,
    summary: &str,
    evidence_hash: &str,
) -> Result<GraphNodeVerificationOutcome> {
    let graph = load_persisted_graph(store, graph_hash)?;
    let task = require_node_task(store, &graph, node_id)?;
    if task.state != "reported" {
        anyhow::bail!(
            "graph node {node_id} cannot be verified from state {}",
            task.state
        );
    }
    let lease_owner = task
        .lease_owner
        .clone()
        .with_context(|| format!("graph node {node_id} has no active lease to verify"))?;
    if lease_owner == verifier_id {
        anyhow::bail!(
            "graph node {node_id} cannot be verified by its own lease owner {verifier_id}"
        );
    }
    let lease_expires_at = task
        .lease_expires_at
        .clone()
        .with_context(|| format!("graph node {node_id} lease is missing an expiry"))?;

    let mut binding = crate::fractal_runtime::GraphSupervisorLeaseBinding::from_active_lease(
        task.id.clone(),
        graph.graph_id.clone(),
        node_id.to_string(),
        graph.graph_hash.clone(),
        lease_owner.clone(),
        lease_expires_at,
    )
    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    binding
        .apply_verifier_handoff(decision == VerifierDecision::Accept, evidence_hash)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;

    let (settled, retries_exhausted) = match decision {
        VerifierDecision::Accept => {
            store.service_handoff_graph_node_verification(
                &task.id,
                summary,
                evidence_hash,
                serde_json::json!({
                    "bindingSchema": binding.schema,
                    "evidenceRoot": evidence_hash,
                    "graphId": graph.graph_id,
                    "graphHash": graph.graph_hash,
                    "nodeId": node_id,
                    "verifierId": verifier_id,
                }),
            )?;
            let completed = store.service_complete_task(&task.id)?;
            let _ = store.service_mark_worker_ready(&lease_owner);
            (completed, false)
        }
        VerifierDecision::Reject => {
            let rejected = store.service_reject_task(&task.id, summary)?;
            let exhausted = matches!(rejected.state.as_str(), "blocked" | "failed");
            let _ = store.service_mark_worker_ready(&lease_owner);
            (rejected, exhausted)
        }
    };

    let reconcile = reconcile_ready_graph_nodes(store, &graph)?;
    let projection = project_graph_terminal_state(store, &graph)?;
    Ok(GraphNodeVerificationOutcome {
        schema: GRAPH_LIVE_PATH_SCHEMA_V1.to_string(),
        graph_id: graph.graph_id.clone(),
        graph_hash: graph.graph_hash.clone(),
        node_id: node_id.to_string(),
        task_id: settled.id.clone(),
        verifier_id: verifier_id.to_string(),
        decision,
        evidence_hash: evidence_hash.to_string(),
        attempt: settled.attempt,
        max_attempts: settled.max_attempts,
        retries_exhausted,
        binding,
        task: settled,
        reconcile,
        projection,
    })
}

/// Recover expired leases for one persisted graph and re-project it.
pub fn recover_graph_leases(
    store: &Store,
    graph_hash: &str,
    steal_after_secs: i64,
) -> Result<GraphLeaseRecoveryOutcome> {
    let graph = load_persisted_graph(store, graph_hash)?;
    let source = graph_source(&graph);
    let recovered = store.service_recover_expired_graph_leases(steal_after_secs)?;
    let recovered = crate::store::ServiceQueueReapResult {
        lease_expired: recovered
            .lease_expired
            .into_iter()
            .filter(|task| task.source_prd_path == source)
            .collect(),
        assignment_released: recovered
            .assignment_released
            .into_iter()
            .filter(|task| task.source_prd_path == source)
            .collect(),
        failed: recovered
            .failed
            .into_iter()
            .filter(|task| task.source_prd_path == source)
            .collect(),
    };
    let reconcile = reconcile_ready_graph_nodes(store, &graph)?;
    let projection = project_graph_terminal_state(store, &graph)?;
    Ok(GraphLeaseRecoveryOutcome {
        schema: GRAPH_LIVE_PATH_SCHEMA_V1.to_string(),
        graph_id: graph.graph_id.clone(),
        graph_hash: graph.graph_hash.clone(),
        recovered,
        reconcile,
        projection,
    })
}

/// Project the durable queue state of one graph into terminal node/graph state.
pub fn project_graph_terminal_state(
    store: &Store,
    graph: &CompiledExecutionGraph,
) -> Result<GraphTerminalProjection> {
    let source = graph_source(graph);
    let mut tasks_by_node = BTreeMap::<String, ServiceTaskRecord>::new();
    for task in store.service_prd_tasks(&source)? {
        if let Some(node_id) = task.source_task_number.clone() {
            tasks_by_node.insert(node_id, task);
        }
    }

    let mut nodes = Vec::with_capacity(graph.nodes.len());
    let (mut complete, mut failed, mut blocked, mut in_flight, mut pending) = (0, 0, 0, 0, 0);
    for node in &graph.nodes {
        let projection = match tasks_by_node.get(&node.id) {
            Some(task) => {
                match task.state.as_str() {
                    "complete" => complete += 1,
                    "failed" => failed += 1,
                    "blocked" => blocked += 1,
                    "queued" | "assigned" => pending += 1,
                    _ => in_flight += 1,
                }
                let evidence_hash = store
                    .service_get_task_verification(&task.id)?
                    .map(|verification| verification.evidence_hash);
                GraphNodeProjection {
                    node_id: node.id.clone(),
                    task_id: Some(task.id.clone()),
                    state: task.state.clone(),
                    terminal: matches!(task.state.as_str(), "complete" | "failed" | "blocked"),
                    attempt: task.attempt,
                    max_attempts: task.max_attempts,
                    lease_owner: task.lease_owner.clone(),
                    lease_expires_at: task.lease_expires_at.clone(),
                    report_hash: task.report_hash.clone(),
                    evidence_hash,
                    last_error: task.last_error.clone(),
                }
            }
            None => {
                pending += 1;
                GraphNodeProjection {
                    node_id: node.id.clone(),
                    task_id: None,
                    state: "pending".to_string(),
                    terminal: false,
                    attempt: 0,
                    max_attempts: 0,
                    lease_owner: None,
                    lease_expires_at: None,
                    report_hash: None,
                    evidence_hash: None,
                    last_error: None,
                }
            }
        };
        nodes.push(projection);
    }

    let node_count = graph.nodes.len();
    let status = if complete == node_count {
        "complete"
    } else if failed > 0 || blocked > 0 {
        "failed"
    } else if in_flight > 0 || complete > 0 {
        "running"
    } else {
        "pending"
    };
    Ok(GraphTerminalProjection {
        schema: GRAPH_LIVE_PATH_SCHEMA_V1.to_string(),
        graph_id: graph.graph_id.clone(),
        graph_hash: graph.graph_hash.clone(),
        graph_source: source,
        node_count,
        complete,
        failed,
        blocked,
        in_flight,
        pending,
        terminal: status == "complete" || status == "failed",
        status: status.to_string(),
        nodes,
    })
}
