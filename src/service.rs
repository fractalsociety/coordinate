use anyhow::{Context, Result};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;

use crate::store::{
    ServiceTaskInput, ServiceTaskProgressInput, ServiceTaskRecord, ServiceTaskReportInput,
    ServiceWorkerInput, Store, DEFAULT_SERVICE_CLAIM_LEASE_SECS,
};

#[derive(Clone)]
pub struct ServiceState {
    db_path: PathBuf,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Debug)]
struct ServiceError(anyhow::Error);

impl IntoResponse for ServiceError {
    fn into_response(self) -> Response {
        let message = self.0.to_string();
        let status = if message.contains("does not exist") {
            StatusCode::NOT_FOUND
        } else if message.contains("cannot")
            || message.contains("not ready")
            || message.contains("not leased")
            || message.contains("not configured")
            || message.contains("invalid")
            || message.contains("empty")
            || message.contains("positive")
            || message.contains("exceeded max attempts")
            || message.contains("conflicts with")
            || message.contains("no node")
        {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        (status, Json(ErrorResponse { error: message })).into_response()
    }
}

impl From<anyhow::Error> for ServiceError {
    fn from(error: anyhow::Error) -> Self {
        Self(error)
    }
}

#[derive(Debug, Deserialize)]
pub struct ServeOptions {
    pub db_path: PathBuf,
    pub bind: SocketAddr,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskListQuery {
    state: Option<String>,
    worker_id: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventListQuery {
    task_id: Option<String>,
    worker_id: Option<String>,
    limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssignTaskRequest {
    worker_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimNextTaskRequest {
    worker_id: String,
    lease_secs: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TouchTaskRequest {
    worker_id: String,
    lease_secs: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseTaskClaimRequest {
    worker_id: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueStatsQuery {
    window_secs: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatRequest {
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FailTaskRequest {
    reason: String,
    #[serde(default)]
    blocked: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryTaskRequest {
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequeueStaleRequest {
    stale_after_secs: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPrdRequest {
    path: PathBuf,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncPrdResponse {
    updated_checkboxes: usize,
}

pub async fn serve(options: ServeOptions) -> Result<()> {
    let app = router(options.db_path);
    let listener = tokio::net::TcpListener::bind(options.bind)
        .await
        .with_context(|| format!("failed to bind Coordinate HTTP service on {}", options.bind))?;
    axum::serve(listener, app).await?;
    Ok(())
}

pub fn router(db_path: PathBuf) -> Router {
    let state = ServiceState { db_path };
    Router::new()
        .route("/health", get(health))
        .route("/workers", get(list_workers))
        .route("/workers/register", post(register_worker))
        .route("/workers/:worker_id/heartbeat", post(heartbeat_worker))
        .route("/workers/:worker_id/ready", post(ready_worker))
        .route("/workers/:worker_id/offline", post(offline_worker))
        .route("/workers/expire-stale", post(expire_stale_workers))
        .route("/tasks", get(list_tasks).post(create_task))
        .route("/tasks/next", post(claim_next_task))
        .route("/tasks/stats", get(task_stats))
        .route("/tasks/reap", post(reap_queue))
        .route("/tasks/requeue-stale", post(requeue_stale_tasks))
        .route("/tasks/:task_id", get(get_task))
        .route("/tasks/:task_id/assign", post(assign_task))
        .route("/tasks/:task_id/ack", post(ack_task))
        .route("/tasks/:task_id/start", post(start_task))
        .route("/tasks/:task_id/touch", post(touch_task))
        .route("/tasks/:task_id/release-claim", post(release_task_claim))
        .route("/tasks/:task_id/progress", post(progress_task))
        .route(
            "/tasks/:task_id/report",
            get(get_task_report).post(report_task),
        )
        .route(
            "/tasks/:task_id/verify",
            get(get_task_verification).post(verify_task),
        )
        .route("/tasks/:task_id/reject", post(reject_task))
        .route("/tasks/:task_id/retry", post(retry_task))
        .route("/tasks/:task_id/complete", post(complete_task))
        .route("/tasks/:task_id/fail", post(fail_task))
        .route("/events", get(list_events))
        .route("/prds", get(list_prds))
        .route("/prds/import", post(import_prd))
        .route("/prds/sync", post(sync_prd))
        .route("/prds/:prd_path/tasks", get(prd_tasks))
        .route("/graphs", get(list_graphs))
        .route("/graphs/compile", post(compile_graph))
        .route("/graphs/:graph_hash", get(get_graph))
        .route("/graphs/:graph_hash/reconcile", post(reconcile_graph))
        .route("/graphs/:graph_hash/projection", get(graph_projection))
        .route(
            "/graphs/:graph_hash/leases/recover",
            post(recover_graph_leases),
        )
        .route(
            "/graphs/:graph_hash/nodes/:node_id/lease",
            post(lease_graph_node),
        )
        .route(
            "/graphs/:graph_hash/nodes/:node_id/report",
            post(report_graph_node),
        )
        .route(
            "/graphs/:graph_hash/nodes/:node_id/verify",
            post(verify_graph_node),
        )
        .with_state(state)
}

fn open_store(state: &ServiceState) -> Result<Store> {
    Store::open(&state.db_path)
}

async fn health(State(state): State<ServiceState>) -> Result<Json<impl Serialize>, ServiceError> {
    Ok(Json(open_store(&state)?.service_health()?))
}

async fn list_workers(
    State(state): State<ServiceState>,
) -> Result<Json<impl Serialize>, ServiceError> {
    Ok(Json(open_store(&state)?.service_list_workers()?))
}

async fn register_worker(
    State(state): State<ServiceState>,
    Json(input): Json<ServiceWorkerInput>,
) -> Result<Json<impl Serialize>, ServiceError> {
    Ok(Json(open_store(&state)?.service_register_worker(input)?))
}

async fn heartbeat_worker(
    State(state): State<ServiceState>,
    Path(worker_id): Path<String>,
    Json(input): Json<HeartbeatRequest>,
) -> Result<Json<impl Serialize>, ServiceError> {
    Ok(Json(open_store(&state)?.service_heartbeat_worker(
        &worker_id,
        input.status.as_deref(),
    )?))
}

async fn ready_worker(
    State(state): State<ServiceState>,
    Path(worker_id): Path<String>,
) -> Result<Json<impl Serialize>, ServiceError> {
    Ok(Json(
        open_store(&state)?.service_mark_worker_ready(&worker_id)?,
    ))
}

async fn offline_worker(
    State(state): State<ServiceState>,
    Path(worker_id): Path<String>,
) -> Result<Json<impl Serialize>, ServiceError> {
    Ok(Json(
        open_store(&state)?.service_mark_worker_offline(&worker_id)?,
    ))
}

async fn create_task(
    State(state): State<ServiceState>,
    Json(input): Json<ServiceTaskInput>,
) -> Result<Json<impl Serialize>, ServiceError> {
    Ok(Json(open_store(&state)?.service_create_task(input)?))
}

async fn list_tasks(
    State(state): State<ServiceState>,
    Query(query): Query<TaskListQuery>,
) -> Result<Json<Vec<ServiceTaskRecord>>, ServiceError> {
    Ok(Json(
        open_store(&state)?
            .service_list_tasks(query.state.as_deref(), query.worker_id.as_deref())?
            .into_iter()
            .take(query.limit.unwrap_or(500).min(1000))
            .collect::<Vec<_>>(),
    ))
}

async fn get_task(
    State(state): State<ServiceState>,
    Path(task_id): Path<String>,
) -> Result<Json<impl Serialize>, ServiceError> {
    let task = open_store(&state)?
        .service_get_task(&task_id)?
        .with_context(|| format!("service task does not exist: {task_id}"))?;
    Ok(Json(task))
}

async fn assign_task(
    State(state): State<ServiceState>,
    Path(task_id): Path<String>,
    Json(input): Json<AssignTaskRequest>,
) -> Result<Json<impl Serialize>, ServiceError> {
    Ok(Json(open_store(&state)?.service_assign_task(
        &task_id,
        input.worker_id.as_deref(),
    )?))
}

async fn claim_next_task(
    State(state): State<ServiceState>,
    Json(input): Json<ClaimNextTaskRequest>,
) -> Result<Json<Option<ServiceTaskRecord>>, ServiceError> {
    let worker_id = input.worker_id.trim();
    if worker_id.is_empty() {
        return Err(anyhow::anyhow!("workerId cannot be empty").into());
    }
    Ok(Json(
        open_store(&state)?.service_claim_next_task(
            worker_id,
            input
                .lease_secs
                .unwrap_or(crate::store::DEFAULT_SERVICE_CLAIM_LEASE_SECS),
            crate::store::DEFAULT_SERVICE_LANE_ESCAPE_SECS,
        )?,
    ))
}

async fn touch_task(
    State(state): State<ServiceState>,
    Path(task_id): Path<String>,
    Json(input): Json<TouchTaskRequest>,
) -> Result<Json<impl Serialize>, ServiceError> {
    let worker_id = input.worker_id.trim();
    if worker_id.is_empty() {
        return Err(anyhow::anyhow!("workerId cannot be empty").into());
    }
    Ok(Json(
        open_store(&state)?.service_touch_task(
            worker_id,
            &task_id,
            input
                .lease_secs
                .unwrap_or(crate::store::DEFAULT_SERVICE_CLAIM_LEASE_SECS),
        )?,
    ))
}

async fn release_task_claim(
    State(state): State<ServiceState>,
    Path(task_id): Path<String>,
    Json(input): Json<ReleaseTaskClaimRequest>,
) -> Result<Json<impl Serialize>, ServiceError> {
    let worker_id = input.worker_id.trim();
    if worker_id.is_empty() {
        return Err(anyhow::anyhow!("workerId cannot be empty").into());
    }
    Ok(Json(open_store(&state)?.service_release_task_claim(
        worker_id,
        &task_id,
        &input.reason,
    )?))
}

async fn ack_task(
    State(state): State<ServiceState>,
    Path(task_id): Path<String>,
) -> Result<Json<impl Serialize>, ServiceError> {
    Ok(Json(open_store(&state)?.service_ack_task(&task_id)?))
}

async fn start_task(
    State(state): State<ServiceState>,
    Path(task_id): Path<String>,
) -> Result<Json<impl Serialize>, ServiceError> {
    Ok(Json(open_store(&state)?.service_start_task(&task_id)?))
}

async fn progress_task(
    State(state): State<ServiceState>,
    Path(task_id): Path<String>,
    Json(input): Json<ServiceTaskProgressInput>,
) -> Result<Json<impl Serialize>, ServiceError> {
    Ok(Json(
        open_store(&state)?.service_progress_task(&task_id, input)?,
    ))
}

async fn report_task(
    State(state): State<ServiceState>,
    Path(task_id): Path<String>,
    Json(input): Json<ServiceTaskReportInput>,
) -> Result<Json<impl Serialize>, ServiceError> {
    Ok(Json(
        open_store(&state)?.service_report_task(&task_id, input)?,
    ))
}

async fn get_task_report(
    State(state): State<ServiceState>,
    Path(task_id): Path<String>,
) -> Result<Json<impl Serialize>, ServiceError> {
    let report = open_store(&state)?
        .service_get_task_report(&task_id)?
        .with_context(|| format!("service task report does not exist: {task_id}"))?;
    Ok(Json(report))
}

async fn verify_task(
    State(state): State<ServiceState>,
    Path(task_id): Path<String>,
    Json(input): Json<ServiceTaskProgressInput>,
) -> Result<Json<impl Serialize>, ServiceError> {
    Ok(Json(
        open_store(&state)?.service_verify_task(&task_id, input)?,
    ))
}

async fn get_task_verification(
    State(state): State<ServiceState>,
    Path(task_id): Path<String>,
) -> Result<Json<impl Serialize>, ServiceError> {
    let verification = open_store(&state)?
        .service_get_task_verification(&task_id)?
        .with_context(|| format!("service task verification does not exist: {task_id}"))?;
    Ok(Json(verification))
}

async fn retry_task(
    State(state): State<ServiceState>,
    Path(task_id): Path<String>,
    Json(input): Json<RetryTaskRequest>,
) -> Result<Json<impl Serialize>, ServiceError> {
    Ok(Json(
        open_store(&state)?.service_retry_task(&task_id, &input.reason)?,
    ))
}

async fn reject_task(
    State(state): State<ServiceState>,
    Path(task_id): Path<String>,
    Json(input): Json<RetryTaskRequest>,
) -> Result<Json<impl Serialize>, ServiceError> {
    Ok(Json(
        open_store(&state)?.service_reject_task(&task_id, &input.reason)?,
    ))
}

async fn complete_task(
    State(state): State<ServiceState>,
    Path(task_id): Path<String>,
) -> Result<Json<impl Serialize>, ServiceError> {
    Ok(Json(open_store(&state)?.service_complete_task(&task_id)?))
}

async fn fail_task(
    State(state): State<ServiceState>,
    Path(task_id): Path<String>,
    Json(input): Json<FailTaskRequest>,
) -> Result<Json<impl Serialize>, ServiceError> {
    Ok(Json(open_store(&state)?.service_fail_task(
        &task_id,
        input.blocked,
        &input.reason,
    )?))
}

async fn requeue_stale_tasks(
    State(state): State<ServiceState>,
    Json(input): Json<RequeueStaleRequest>,
) -> Result<Json<impl Serialize>, ServiceError> {
    Ok(Json(open_store(&state)?.service_requeue_stale_tasks(
        input.stale_after_secs.unwrap_or(900),
    )?))
}

async fn reap_queue(
    State(state): State<ServiceState>,
    Json(input): Json<RequeueStaleRequest>,
) -> Result<Json<impl Serialize>, ServiceError> {
    Ok(Json(open_store(&state)?.service_reap_queue(
        input.stale_after_secs.unwrap_or(900),
    )?))
}

async fn task_stats(
    State(state): State<ServiceState>,
    Query(query): Query<QueueStatsQuery>,
) -> Result<Json<impl Serialize>, ServiceError> {
    Ok(Json(
        open_store(&state)?.service_queue_stats(query.window_secs.unwrap_or(3600))?,
    ))
}

async fn expire_stale_workers(
    State(state): State<ServiceState>,
    Json(input): Json<RequeueStaleRequest>,
) -> Result<Json<impl Serialize>, ServiceError> {
    Ok(Json(open_store(&state)?.service_expire_stale_workers(
        input.stale_after_secs.unwrap_or(900),
    )?))
}

async fn list_events(
    State(state): State<ServiceState>,
    Query(query): Query<EventListQuery>,
) -> Result<Json<impl Serialize>, ServiceError> {
    Ok(Json(open_store(&state)?.service_list_events(
        query.task_id.as_deref(),
        query.worker_id.as_deref(),
        query.limit.unwrap_or(100),
    )?))
}

async fn list_prds(
    State(state): State<ServiceState>,
) -> Result<Json<impl Serialize>, ServiceError> {
    Ok(Json(open_store(&state)?.service_list_prds()?))
}

async fn import_prd(
    State(state): State<ServiceState>,
    Json(input): Json<ImportPrdRequest>,
) -> Result<Json<impl Serialize>, ServiceError> {
    Ok(Json(open_store(&state)?.service_import_prd(&input.path)?))
}

async fn sync_prd(
    State(state): State<ServiceState>,
    Json(input): Json<ImportPrdRequest>,
) -> Result<Json<SyncPrdResponse>, ServiceError> {
    Ok(Json(SyncPrdResponse {
        updated_checkboxes: open_store(&state)?.service_sync_prd(&input.path)?,
    }))
}

async fn prd_tasks(
    State(state): State<ServiceState>,
    Path(prd_path): Path<String>,
) -> Result<Json<Vec<ServiceTaskRecord>>, ServiceError> {
    Ok(Json(open_store(&state)?.service_prd_tasks(&prd_path)?))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphNodeLeaseRequest {
    worker_id: String,
    lease_secs: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphNodeReportRequest {
    worker_id: String,
    #[serde(flatten)]
    report: ServiceTaskReportInput,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphNodeVerifyRequest {
    verifier_id: String,
    decision: crate::graph_supervisor::VerifierDecision,
    summary: String,
    evidence_hash: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphLeaseRecoveryRequest {
    steal_after_secs: Option<i64>,
}

/// Compile, validate, durably persist, and reconcile one execution graph.
async fn compile_graph(
    State(state): State<ServiceState>,
    Json(graph): Json<crate::graph_supervisor::CompiledExecutionGraph>,
) -> Result<Json<impl Serialize>, ServiceError> {
    Ok(Json(crate::graph_supervisor::compile_and_persist_graph(
        &open_store(&state)?,
        &graph,
    )?))
}

async fn list_graphs(
    State(state): State<ServiceState>,
) -> Result<Json<impl Serialize>, ServiceError> {
    Ok(Json(open_store(&state)?.service_list_execution_graphs()?))
}

async fn get_graph(
    State(state): State<ServiceState>,
    Path(graph_hash): Path<String>,
) -> Result<Json<impl Serialize>, ServiceError> {
    let graph = open_store(&state)?
        .service_get_execution_graph(&graph_hash)?
        .with_context(|| format!("execution graph does not exist: {graph_hash}"))?;
    Ok(Json(graph))
}

async fn reconcile_graph(
    State(state): State<ServiceState>,
    Path(graph_hash): Path<String>,
) -> Result<Json<impl Serialize>, ServiceError> {
    Ok(Json(crate::graph_supervisor::advance_persisted_graph(
        &open_store(&state)?,
        &graph_hash,
    )?))
}

async fn graph_projection(
    State(state): State<ServiceState>,
    Path(graph_hash): Path<String>,
) -> Result<Json<impl Serialize>, ServiceError> {
    let store = open_store(&state)?;
    let graph = crate::graph_supervisor::load_persisted_graph(&store, &graph_hash)?;
    Ok(Json(crate::graph_supervisor::project_graph_terminal_state(
        &store, &graph,
    )?))
}

async fn lease_graph_node(
    State(state): State<ServiceState>,
    Path((graph_hash, node_id)): Path<(String, String)>,
    Json(input): Json<GraphNodeLeaseRequest>,
) -> Result<Json<impl Serialize>, ServiceError> {
    let worker_id = input.worker_id.trim();
    if worker_id.is_empty() {
        return Err(anyhow::anyhow!("workerId cannot be empty").into());
    }
    Ok(Json(crate::graph_supervisor::checkout_graph_node_lease(
        &open_store(&state)?,
        &graph_hash,
        &node_id,
        worker_id,
        input.lease_secs.unwrap_or(DEFAULT_SERVICE_CLAIM_LEASE_SECS),
    )?))
}

async fn report_graph_node(
    State(state): State<ServiceState>,
    Path((graph_hash, node_id)): Path<(String, String)>,
    Json(input): Json<GraphNodeReportRequest>,
) -> Result<Json<impl Serialize>, ServiceError> {
    let worker_id = input.worker_id.trim();
    if worker_id.is_empty() {
        return Err(anyhow::anyhow!("workerId cannot be empty").into());
    }
    Ok(Json(crate::graph_supervisor::report_graph_node(
        &open_store(&state)?,
        &graph_hash,
        &node_id,
        worker_id,
        input.report,
    )?))
}

async fn verify_graph_node(
    State(state): State<ServiceState>,
    Path((graph_hash, node_id)): Path<(String, String)>,
    Json(input): Json<GraphNodeVerifyRequest>,
) -> Result<Json<impl Serialize>, ServiceError> {
    let verifier_id = input.verifier_id.trim();
    if verifier_id.is_empty() {
        return Err(anyhow::anyhow!("verifierId cannot be empty").into());
    }
    Ok(Json(crate::graph_supervisor::verify_graph_node(
        &open_store(&state)?,
        &graph_hash,
        &node_id,
        verifier_id,
        input.decision,
        &input.summary,
        &input.evidence_hash,
    )?))
}

async fn recover_graph_leases(
    State(state): State<ServiceState>,
    Path(graph_hash): Path<String>,
    Json(input): Json<GraphLeaseRecoveryRequest>,
) -> Result<Json<impl Serialize>, ServiceError> {
    Ok(Json(crate::graph_supervisor::recover_graph_leases(
        &open_store(&state)?,
        &graph_hash,
        input.steal_after_secs.unwrap_or(900),
    )?))
}

/// Observable proof flags for the INT-078 graph-supervisor lease boundary.
///
/// These types live on the service boundary so INT-084 can later wire HTTP
/// without changing the store contract. This module intentionally does not
/// register routes for the proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphSupervisorLeaseBoundaryProof {
    pub schema: String,
    pub graph_id: String,
    pub graph_hash: String,
    pub dependency_aware_checkout: bool,
    pub at_most_once_active_lease: bool,
    pub bounded_retry: bool,
    pub verifier_handoff: bool,
    pub expiry_recovery: bool,
    pub evidence_hash: String,
    pub notes: Vec<String>,
}

impl GraphSupervisorLeaseBoundaryProof {
    pub fn all_behaviors_proven(&self) -> bool {
        self.dependency_aware_checkout
            && self.at_most_once_active_lease
            && self.bounded_retry
            && self.verifier_handoff
            && self.expiry_recovery
    }
}

fn int078_evidence_hash(label: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("int078-lease-boundary:{label}").as_bytes());
    format!("sha256:{digest:x}")
}

fn register_boundary_worker(store: &Store, worker_id: &str) -> Result<()> {
    store.service_register_worker(ServiceWorkerInput {
        id: worker_id.to_string(),
        kind: "codex".to_string(),
        role: "coding_worker".to_string(),
        status: Some("ready".to_string()),
        capacity: Some(1),
        metadata: None,
    })?;
    Ok(())
}

fn report_boundary_task(store: &Store, task_id: &str, summary: &str) -> Result<()> {
    store.service_start_task(task_id)?;
    store.service_report_task(
        task_id,
        ServiceTaskReportInput {
            summary: summary.to_string(),
            files_inspected: vec!["squad-coordinate-sync/src/store.rs".to_string()],
            changed_files: vec!["squad-coordinate-sync/src/service.rs".to_string()],
            tests_run: vec!["graph_supervisor_lease_boundary_test".to_string()],
            verification: "fixture verification".to_string(),
            risks: "none".to_string(),
            raw_report: summary.to_string(),
        },
    )?;
    Ok(())
}

/// Run the frozen INT-078 graph fixture through the store/service lease
/// boundary without touching the HTTP router.
pub fn prove_graph_supervisor_lease_boundary(
    store: &Store,
) -> Result<GraphSupervisorLeaseBoundaryProof> {
    use crate::autopilot::{
        frozen_graph_supervisor_lease_fixture, frozen_graph_supervisor_lease_source,
        GRAPH_SUPERVISOR_LEASE_CONTRACT_SCHEMA, GRAPH_SUPERVISOR_LEASE_FIXTURE_MAX_ATTEMPTS,
    };
    use crate::fractal_runtime::GraphSupervisorLeaseBinding;
    use crate::graph_supervisor::reconcile_ready_graph_nodes;

    let graph = frozen_graph_supervisor_lease_fixture();
    let source = frozen_graph_supervisor_lease_source();
    let mut notes = Vec::new();

    register_boundary_worker(store, "int078-worker-a")?;
    register_boundary_worker(store, "int078-worker-b")?;

    let first = reconcile_ready_graph_nodes(store, &graph)?;
    anyhow::ensure!(
        first.enqueued.len() == 1 && first.enqueued[0].node_id == "compile",
        "frozen fixture must enqueue only compile first"
    );
    notes.push("compile enqueued as sole ready root".to_string());

    // Dependency-aware checkout: fabricate a blocked execute task and refuse it.
    let compile_task_id = first.enqueued[0].task_id.clone();
    let blocked = store.service_create_task(ServiceTaskInput {
        title: "Blocked execute probe".to_string(),
        description: "Must not checkout while compile is incomplete.".to_string(),
        acceptance_criteria: vec!["never leased".to_string()],
        source_prd_path: source.clone(),
        source_task_number: Some("execute-probe".to_string()),
        priority: 0,
        preferred_model: "codex".to_string(),
        role: "coding_worker".to_string(),
        parallelizable: true,
        max_attempts: Some(GRAPH_SUPERVISOR_LEASE_FIXTURE_MAX_ATTEMPTS),
        dependencies: Some(vec![compile_task_id.clone()]),
    })?;
    let blocked_err = store
        .service_checkout_graph_node_lease(
            "int078-worker-a",
            &blocked.id,
            DEFAULT_SERVICE_CLAIM_LEASE_SECS,
        )
        .expect_err("blocked dependency must fail closed");
    anyhow::ensure!(
        blocked_err
            .to_string()
            .contains("blocked by incomplete dependency"),
        "unexpected blocked checkout error: {blocked_err}"
    );
    // Remove the probe so it cannot pollute later claims.
    store.service_fail_task(&blocked.id, true, "int078 dependency probe complete")?;
    let dependency_aware_checkout = true;
    notes.push("dependency-aware checkout rejected incomplete predecessor".to_string());

    let checkout = store.service_checkout_graph_node_lease(
        "int078-worker-a",
        &compile_task_id,
        DEFAULT_SERVICE_CLAIM_LEASE_SECS,
    )?;
    anyhow::ensure!(
        checkout.lease_owner == "int078-worker-a",
        "compile lease owner mismatch"
    );

    let conflict = store
        .service_checkout_graph_node_lease(
            "int078-worker-b",
            &compile_task_id,
            DEFAULT_SERVICE_CLAIM_LEASE_SECS,
        )
        .expect_err("second active lease must fail");
    anyhow::ensure!(
        conflict
            .to_string()
            .contains("already holds an active lease"),
        "unexpected at-most-once error: {conflict}"
    );
    let active = store.service_active_graph_node_leases(&source)?;
    anyhow::ensure!(
        active.len() == 1 && active[0].lease_owner.as_deref() == Some("int078-worker-a"),
        "expected exactly one active lease"
    );
    let at_most_once_active_lease = true;
    notes.push("at-most-once active lease enforced for compile".to_string());

    report_boundary_task(store, &compile_task_id, "compile node complete")?;
    let evidence_hash = int078_evidence_hash("compile-handoff");
    let mut binding = GraphSupervisorLeaseBinding::from_active_lease(
        &compile_task_id,
        &graph.graph_id,
        "compile",
        &graph.graph_hash,
        &checkout.lease_owner,
        &checkout.lease_expires_at,
    )
    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    binding
        .apply_verifier_handoff(true, &evidence_hash)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let handoff = store.service_handoff_graph_node_verification(
        &compile_task_id,
        "compile evidence accepted",
        &evidence_hash,
        serde_json::json!({
            "bindingSchema": binding.schema,
            "evidenceRoot": evidence_hash,
            "nodeId": "compile",
        }),
    )?;
    anyhow::ensure!(
        handoff.task.state == "verified" && binding.handoff_verified(),
        "verifier handoff did not reach verified state"
    );
    store.service_complete_task(&compile_task_id)?;
    store.service_mark_worker_ready("int078-worker-a")?;
    let verifier_handoff = true;
    notes.push("verifier handoff persisted compile evidence".to_string());

    let second = reconcile_ready_graph_nodes(store, &graph)?;
    anyhow::ensure!(
        second.enqueued.len() == 1 && second.enqueued[0].node_id == "execute",
        "execute must become ready after compile completes"
    );
    let execute_id = second.enqueued[0].task_id.clone();
    // Cap attempts for bounded-retry proof.
    store.service_set_graph_node_max_attempts(
        &execute_id,
        GRAPH_SUPERVISOR_LEASE_FIXTURE_MAX_ATTEMPTS,
    )?;

    let execute_lease =
        store.service_checkout_graph_node_lease("int078-worker-a", &execute_id, 1)?;
    anyhow::ensure!(execute_lease.node_id == "execute");

    // Force lease expiry and recover.
    store.service_mark_graph_node_lease_expired(&execute_id)?;
    let recovered = store.service_recover_expired_graph_leases(60)?;
    anyhow::ensure!(
        recovered.lease_expired.len() == 1
            && recovered.lease_expired[0].id == execute_id
            && recovered.lease_expired[0].state == "queued"
            && recovered.lease_expired[0].lease_owner.is_none(),
        "expiry recovery did not requeue execute"
    );
    store.service_mark_worker_ready("int078-worker-a")?;
    let expiry_recovery = true;
    notes.push("expiry recovery requeued execute without an active lease".to_string());

    // Bounded retry: reclaim, report, reject until budget exhausted.
    let reclaimed = store.service_checkout_graph_node_lease(
        "int078-worker-b",
        &execute_id,
        DEFAULT_SERVICE_CLAIM_LEASE_SECS,
    )?;
    anyhow::ensure!(reclaimed.lease_owner == "int078-worker-b");
    report_boundary_task(store, &execute_id, "execute attempt needing retry")?;
    let before_reject = store
        .service_get_task(&execute_id)?
        .context("execute task missing before reject")?;
    let retried = store.service_reject_task(&execute_id, "fixture verifier rejection")?;
    anyhow::ensure!(
        retried.state == "queued" && retried.attempt == before_reject.attempt + 1,
        "first rejection should requeue within budget (attempt {} -> {})",
        before_reject.attempt,
        retried.attempt
    );
    store.service_mark_worker_ready("int078-worker-b")?;

    let final_claim = store.service_checkout_graph_node_lease(
        "int078-worker-b",
        &execute_id,
        DEFAULT_SERVICE_CLAIM_LEASE_SECS,
    )?;
    anyhow::ensure!(final_claim.task.id == execute_id);
    report_boundary_task(store, &execute_id, "execute attempt exhausting budget")?;
    let exhausted = store.service_reject_task(&execute_id, "fixture verifier rejection")?;
    anyhow::ensure!(
        exhausted.state == "blocked" || exhausted.state == "failed",
        "bounded retry must escalate after max attempts, got {}",
        exhausted.state
    );
    let bounded_retry = exhausted.attempt >= GRAPH_SUPERVISOR_LEASE_FIXTURE_MAX_ATTEMPTS
        || exhausted.state == "blocked"
        || exhausted.state == "failed";
    anyhow::ensure!(bounded_retry, "bounded retry proof failed");
    notes.push("bounded retry exhausted execute attempts and escalated".to_string());

    Ok(GraphSupervisorLeaseBoundaryProof {
        schema: GRAPH_SUPERVISOR_LEASE_CONTRACT_SCHEMA.to_string(),
        graph_id: graph.graph_id,
        graph_hash: graph.graph_hash,
        dependency_aware_checkout,
        at_most_once_active_lease,
        bounded_retry,
        verifier_handoff,
        expiry_recovery,
        evidence_hash,
        notes,
    })
}
