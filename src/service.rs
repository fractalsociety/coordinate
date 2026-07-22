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
    ServiceWorkerInput, Store,
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
        .route("/tasks/:task_id/progress", post(progress_task))
        .route(
            "/tasks/:task_id/report",
            get(get_task_report).post(report_task),
        )
        .route("/tasks/:task_id/verify", post(verify_task))
        .route("/tasks/:task_id/retry", post(retry_task))
        .route("/tasks/:task_id/complete", post(complete_task))
        .route("/tasks/:task_id/fail", post(fail_task))
        .route("/events", get(list_events))
        .route("/prds", get(list_prds))
        .route("/prds/import", post(import_prd))
        .route("/prds/sync", post(sync_prd))
        .route("/prds/:prd_path/tasks", get(prd_tasks))
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

async fn retry_task(
    State(state): State<ServiceState>,
    Path(task_id): Path<String>,
    Json(input): Json<RetryTaskRequest>,
) -> Result<Json<impl Serialize>, ServiceError> {
    Ok(Json(
        open_store(&state)?.service_retry_task(&task_id, &input.reason)?,
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
