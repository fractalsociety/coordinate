//! Durable Coordinate adapter for long-running Fractal execution graphs.
//!
//! Coordinate owns the global job lifecycle. This module persists the fields
//! needed to correlate that lifecycle with one `fractald` execution and emits
//! typed daemon API requests. Polling is deliberately limited to durable work
//! state and evidence-chain endpoints; raw model and container logs are not an
//! adapter input.

use std::{error::Error, fmt, path::Path};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Version marker for Coordinate's runtime-job record.
pub const RUNTIME_JOB_SCHEMA_V1: &str = "coordinate.fractal_runtime_job.v1";

/// Scheduling mode retained by Coordinate across daemon restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeJobMode {
    /// User-facing latency-sensitive work.
    Interactive,
    /// Durable unattended graph execution.
    Background,
    /// Explicitly authorized training work.
    Training,
}

/// Coordinate's view of the daemon-owned local lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeJobState {
    /// Coordinate has prepared but not yet observed daemon admission.
    Submitted,
    /// `fractald` accepted the signed lease and graph.
    Admitted,
    /// Local graph execution is active.
    Executing,
    /// Local execution is reaching a safe cancellation boundary.
    Cancelling,
    /// Local execution was cancelled.
    Cancelled,
    /// Local execution completed successfully.
    Completed,
    /// Local execution failed.
    Failed,
    /// A daemon restart interrupted execution and a checkpoint may be resumed.
    Interrupted,
}

/// Durable cancellation handshake state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancellationState {
    /// No cancellation has been requested.
    None,
    /// Coordinate emitted a cancellation request.
    Requested,
    /// `fractald` acknowledged cancellation in progress.
    Cancelling,
    /// `fractald` confirmed the terminal cancelled state.
    Cancelled,
}

/// Independent evidence verification state retained by Coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationState {
    /// No verified evidence root has been observed.
    Pending,
    /// The returned append-only evidence chain verified.
    Verified,
    /// The daemon reported an invalid evidence chain.
    Rejected,
}

/// Coordinate's durable correlation record for one runtime graph execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FractalRuntimeJob {
    /// Contract version marker.
    pub schema: String,
    /// Coordinate-owned job identifier.
    pub coordinate_job_id: String,
    /// FractalWork identifier.
    pub work_id: String,
    /// Canonical work content hash.
    pub work_hash: String,
    /// FractalWork graph identifier.
    pub graph_id: String,
    /// Canonical compiled graph hash.
    pub graph_hash: String,
    /// Provider node authorized by the signed lease.
    pub provider_node: String,
    /// Daemon-local execution correlation identifier.
    pub local_execution_id: String,
    /// Signed lease expiry in Unix epoch milliseconds.
    pub lease_expires_at_ms: u64,
    /// Latest resumable checkpoint hash, when interrupted.
    pub resume_checkpoint_hash: Option<String>,
    /// Backend-neutral resource class selected by FractalWork.
    pub resource_class: String,
    /// Interactive, background, or training scheduling mode.
    pub mode: RuntimeJobMode,
    /// Last durable daemon lifecycle state observed by polling.
    pub runtime_state: RuntimeJobState,
    /// Coordinate-to-daemon cancellation handshake state.
    pub cancellation_state: CancellationState,
    /// Verified append-only evidence root.
    pub evidence_root: Option<String>,
    /// Evidence verification outcome.
    pub verification_state: VerificationState,
    /// Coordinate update time in Unix epoch milliseconds.
    pub updated_at_ms: u64,
}

/// Newline-delimited JSON request envelope understood by `fractald`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FractaldRequest {
    /// HTTP-like method routed over the private UDS.
    pub method: String,
    /// Versioned daemon API path.
    pub path: String,
    /// Typed request body; GET requests use JSON null.
    pub body: Value,
}

/// Fail-closed adapter or persistence error.
#[derive(Debug)]
pub enum RuntimeAdapterError {
    /// A required identity, hash, time, or schema field is invalid.
    InvalidRecord(String),
    /// Daemon response status or shape is invalid.
    InvalidResponse(String),
    /// A response attempts to change immutable job identity or authorization.
    IdentityMismatch(String),
    /// A checkpoint was supplied outside an interrupted lifecycle.
    CheckpointNotResumable,
    /// SQLite persistence failed.
    Storage(rusqlite::Error),
}

impl fmt::Display for RuntimeAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRecord(message) => write!(formatter, "invalid runtime job: {message}"),
            Self::InvalidResponse(message) => {
                write!(formatter, "invalid fractald response: {message}")
            }
            Self::IdentityMismatch(field) => {
                write!(formatter, "fractald response changed immutable {field}")
            }
            Self::CheckpointNotResumable => {
                formatter.write_str("resume checkpoint requires interrupted runtime state")
            }
            Self::Storage(error) => write!(formatter, "runtime job storage failed: {error}"),
        }
    }
}

impl Error for RuntimeAdapterError {}

impl From<rusqlite::Error> for RuntimeAdapterError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error)
    }
}

impl FractalRuntimeJob {
    /// Validate the stable correlation and authorization fields.
    pub fn validate(&self) -> Result<(), RuntimeAdapterError> {
        if self.schema != RUNTIME_JOB_SCHEMA_V1 {
            return Err(RuntimeAdapterError::InvalidRecord(
                "unsupported schema".to_string(),
            ));
        }
        for (name, value) in [
            ("coordinate_job_id", self.coordinate_job_id.as_str()),
            ("work_id", self.work_id.as_str()),
            ("graph_id", self.graph_id.as_str()),
            ("provider_node", self.provider_node.as_str()),
            ("local_execution_id", self.local_execution_id.as_str()),
            ("resource_class", self.resource_class.as_str()),
        ] {
            if !safe_identifier(value) {
                return Err(RuntimeAdapterError::InvalidRecord(format!(
                    "{name} is empty or unsafe"
                )));
            }
        }
        validate_hash("work_hash", &self.work_hash)?;
        validate_hash("graph_hash", &self.graph_hash)?;
        if let Some(checkpoint) = &self.resume_checkpoint_hash {
            validate_hash("resume_checkpoint_hash", checkpoint)?;
        }
        if let Some(evidence_root) = &self.evidence_root {
            validate_hash("evidence_root", evidence_root)?;
        }
        if self.lease_expires_at_ms == 0 {
            return Err(RuntimeAdapterError::InvalidRecord(
                "lease_expires_at_ms must be non-zero".to_string(),
            ));
        }
        Ok(())
    }

    /// Build the signed graph-admission request for `fractald`.
    pub fn admit_request(
        &self,
        signed_lease: Value,
        graph_json: Value,
        compiler_version: &str,
    ) -> Result<FractaldRequest, RuntimeAdapterError> {
        self.validate()?;
        if compiler_version.trim().is_empty()
            || !signed_lease.is_object()
            || !graph_json.is_object()
        {
            return Err(RuntimeAdapterError::InvalidRecord(
                "admission requires signed lease, graph object, and compiler version".to_string(),
            ));
        }
        Ok(FractaldRequest {
            method: "POST".to_string(),
            path: "/v1/work/admit".to_string(),
            body: json!({
                "lease": signed_lease,
                "work_id": self.work_id,
                "work_hash": self.work_hash,
                "graph_hash": self.graph_hash,
                "graph_json": graph_json,
                "compiler_version": compiler_version,
            }),
        })
    }

    /// Build the durable state poll. No raw-log endpoint is used.
    pub fn poll_request(&self) -> Result<FractaldRequest, RuntimeAdapterError> {
        self.validate()?;
        Ok(FractaldRequest {
            method: "GET".to_string(),
            path: format!("/v1/work/{}", self.work_id),
            body: Value::Null,
        })
    }

    /// Build the append-only evidence-chain poll. No raw-log endpoint is used.
    pub fn evidence_request(&self) -> Result<FractaldRequest, RuntimeAdapterError> {
        self.validate()?;
        Ok(FractaldRequest {
            method: "GET".to_string(),
            path: format!("/v1/work/{}/events", self.work_id),
            body: Value::Null,
        })
    }

    /// Build a cancellation request and record Coordinate's requested state.
    pub fn cancel_request(
        &mut self,
        reason: &str,
        now_ms: u64,
    ) -> Result<FractaldRequest, RuntimeAdapterError> {
        self.validate()?;
        if reason.trim().is_empty() {
            return Err(RuntimeAdapterError::InvalidRecord(
                "cancellation reason must be non-empty".to_string(),
            ));
        }
        self.cancellation_state = CancellationState::Requested;
        self.updated_at_ms = now_ms;
        Ok(FractaldRequest {
            method: "POST".to_string(),
            path: format!("/v1/work/{}/cancel", self.work_id),
            body: json!({"reason": reason}),
        })
    }

    /// Apply one successful durable-state response from `fractald`.
    pub fn apply_status_response(
        &mut self,
        status: u16,
        body: &Value,
        now_ms: u64,
    ) -> Result<(), RuntimeAdapterError> {
        if status != 200 && status != 201 {
            return Err(RuntimeAdapterError::InvalidResponse(format!(
                "unexpected status {status}"
            )));
        }
        require_equal(body, "work_id", &self.work_id)?;
        if let Some(work_hash) = body.get("work_hash").and_then(Value::as_str) {
            if work_hash != self.work_hash {
                return Err(RuntimeAdapterError::IdentityMismatch(
                    "work_hash".to_string(),
                ));
            }
        }
        if let Some(expiry) = body.get("lease_expires_at_ms").and_then(Value::as_u64) {
            if expiry > self.lease_expires_at_ms {
                return Err(RuntimeAdapterError::IdentityMismatch(
                    "lease_expires_at_ms".to_string(),
                ));
            }
        }
        let state = body
            .get("state")
            .and_then(Value::as_str)
            .ok_or_else(|| RuntimeAdapterError::InvalidResponse("missing state".to_string()))?;
        self.runtime_state = parse_runtime_state(state)?;
        match self.runtime_state {
            RuntimeJobState::Cancelling => self.cancellation_state = CancellationState::Cancelling,
            RuntimeJobState::Cancelled => self.cancellation_state = CancellationState::Cancelled,
            _ => {}
        }
        self.updated_at_ms = now_ms;
        Ok(())
    }

    /// Verify and retain the root of one daemon evidence-chain response.
    pub fn apply_evidence_response(
        &mut self,
        status: u16,
        body: &Value,
        now_ms: u64,
    ) -> Result<(), RuntimeAdapterError> {
        if status != 200 {
            return Err(RuntimeAdapterError::InvalidResponse(format!(
                "unexpected evidence status {status}"
            )));
        }
        require_equal(body, "work_id", &self.work_id)?;
        if body.get("chain_verified").and_then(Value::as_bool) != Some(true) {
            self.verification_state = VerificationState::Rejected;
            self.updated_at_ms = now_ms;
            return Err(RuntimeAdapterError::InvalidResponse(
                "evidence chain is not verified".to_string(),
            ));
        }
        let events = body
            .get("events")
            .and_then(Value::as_array)
            .ok_or_else(|| RuntimeAdapterError::InvalidResponse("missing events".to_string()))?;
        let root = events
            .last()
            .and_then(|event| event.get("event_hash"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                RuntimeAdapterError::InvalidResponse("missing evidence root".to_string())
            })?;
        validate_hash("evidence_root", root)?;
        self.evidence_root = Some(root.to_string());
        self.verification_state = VerificationState::Verified;
        self.updated_at_ms = now_ms;
        Ok(())
    }

    /// Retain a content-addressed checkpoint only after an interruption.
    pub fn record_resume_checkpoint(
        &mut self,
        checkpoint_hash: &str,
        now_ms: u64,
    ) -> Result<(), RuntimeAdapterError> {
        if self.runtime_state != RuntimeJobState::Interrupted {
            return Err(RuntimeAdapterError::CheckpointNotResumable);
        }
        validate_hash("resume_checkpoint_hash", checkpoint_hash)?;
        self.resume_checkpoint_hash = Some(checkpoint_hash.to_string());
        self.updated_at_ms = now_ms;
        Ok(())
    }
}

/// Durable SQLite storage for runtime-job correlation records.
pub struct RuntimeJobStore {
    connection: Connection,
}

impl RuntimeJobStore {
    /// Open a Coordinate database and create the additive adapter table.
    pub fn open(path: &Path) -> Result<Self, RuntimeAdapterError> {
        let connection = Connection::open(path)?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;
             CREATE TABLE IF NOT EXISTS fractal_runtime_jobs (
                 coordinate_job_id TEXT PRIMARY KEY NOT NULL,
                 work_id TEXT NOT NULL UNIQUE,
                 record_json TEXT NOT NULL,
                 updated_at_ms INTEGER NOT NULL
             ) STRICT;",
        )?;
        Ok(Self { connection })
    }

    /// Insert or update mutable state while rejecting identity substitution.
    pub fn put(&self, job: &FractalRuntimeJob) -> Result<(), RuntimeAdapterError> {
        job.validate()?;
        let updated_at_ms = i64::try_from(job.updated_at_ms).map_err(|_| {
            RuntimeAdapterError::InvalidRecord("updated_at_ms exceeds SQLite range".to_string())
        })?;
        let record_json = serde_json::to_string(job).map_err(|error| {
            RuntimeAdapterError::InvalidRecord(format!("record encoding failed: {error}"))
        })?;
        let existing: Option<String> = self
            .connection
            .query_row(
                "SELECT record_json FROM fractal_runtime_jobs WHERE coordinate_job_id = ?1",
                [&job.coordinate_job_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            let previous: FractalRuntimeJob = serde_json::from_str(&existing).map_err(|error| {
                RuntimeAdapterError::InvalidRecord(format!("stored record is invalid: {error}"))
            })?;
            ensure_same_identity(&previous, job)?;
        }
        self.connection.execute(
            "INSERT INTO fractal_runtime_jobs
             (coordinate_job_id, work_id, record_json, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(coordinate_job_id) DO UPDATE SET
                 record_json = excluded.record_json,
                 updated_at_ms = excluded.updated_at_ms",
            params![
                job.coordinate_job_id,
                job.work_id,
                record_json,
                updated_at_ms
            ],
        )?;
        Ok(())
    }

    /// Load one durable runtime job by Coordinate identifier.
    pub fn get(
        &self,
        coordinate_job_id: &str,
    ) -> Result<Option<FractalRuntimeJob>, RuntimeAdapterError> {
        let record: Option<String> = self
            .connection
            .query_row(
                "SELECT record_json FROM fractal_runtime_jobs WHERE coordinate_job_id = ?1",
                [coordinate_job_id],
                |row| row.get(0),
            )
            .optional()?;
        record
            .map(|record| {
                serde_json::from_str(&record).map_err(|error| {
                    RuntimeAdapterError::InvalidRecord(format!("stored record is invalid: {error}"))
                })
            })
            .transpose()
    }
}

fn ensure_same_identity(
    previous: &FractalRuntimeJob,
    current: &FractalRuntimeJob,
) -> Result<(), RuntimeAdapterError> {
    for (name, same) in [
        ("work_id", previous.work_id == current.work_id),
        ("work_hash", previous.work_hash == current.work_hash),
        ("graph_id", previous.graph_id == current.graph_id),
        ("graph_hash", previous.graph_hash == current.graph_hash),
        (
            "provider_node",
            previous.provider_node == current.provider_node,
        ),
        (
            "local_execution_id",
            previous.local_execution_id == current.local_execution_id,
        ),
        (
            "lease_expires_at_ms",
            previous.lease_expires_at_ms == current.lease_expires_at_ms,
        ),
    ] {
        if !same {
            return Err(RuntimeAdapterError::IdentityMismatch(name.to_string()));
        }
    }
    Ok(())
}

fn require_equal(body: &Value, field: &str, expected: &str) -> Result<(), RuntimeAdapterError> {
    match body.get(field).and_then(Value::as_str) {
        Some(actual) if actual == expected => Ok(()),
        Some(_) => Err(RuntimeAdapterError::IdentityMismatch(field.to_string())),
        None => Err(RuntimeAdapterError::InvalidResponse(format!(
            "missing {field}"
        ))),
    }
}

fn parse_runtime_state(value: &str) -> Result<RuntimeJobState, RuntimeAdapterError> {
    match value {
        "admitted" => Ok(RuntimeJobState::Admitted),
        "executing" => Ok(RuntimeJobState::Executing),
        "cancelling" => Ok(RuntimeJobState::Cancelling),
        "cancelled" => Ok(RuntimeJobState::Cancelled),
        "completed" => Ok(RuntimeJobState::Completed),
        "failed" => Ok(RuntimeJobState::Failed),
        "interrupted" => Ok(RuntimeJobState::Interrupted),
        _ => Err(RuntimeAdapterError::InvalidResponse(format!(
            "unknown runtime state {value:?}"
        ))),
    }
}

fn safe_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

fn validate_hash(name: &str, value: &str) -> Result<(), RuntimeAdapterError> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(RuntimeAdapterError::InvalidRecord(format!(
            "{name} must use sha256"
        )));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(RuntimeAdapterError::InvalidRecord(format!(
            "{name} must be lowercase canonical sha256"
        )));
    }
    Ok(())
}
