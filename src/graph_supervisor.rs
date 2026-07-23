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
