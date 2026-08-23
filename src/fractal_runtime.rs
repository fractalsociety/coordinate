//! Durable Coordinate adapter for long-running Fractal execution graphs.
//!
//! Coordinate owns the global job lifecycle. This module persists the fields
//! needed to correlate that lifecycle with one `fractald` execution and emits
//! typed daemon API requests. Polling is deliberately limited to durable work
//! state and evidence-chain endpoints; raw model and container logs are not an
//! adapter input.

use std::{
    error::Error,
    fmt,
    path::Path,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
#[cfg(unix)]
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
};

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
    /// Graph node authorized by the signed work lease.
    pub graph_node_id: String,
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

/// Schema marker binding a Coordinate graph-node lease to runtime verification
/// handoff state (INT-078 lease boundary).
pub const GRAPH_SUPERVISOR_LEASE_BINDING_SCHEMA: &str =
    "coordinate.graph_supervisor_lease_binding.v1";

/// Durable correlation between a Coordinate pull-queue lease and the verifier
/// handoff that may only proceed while the lease owner remains exclusive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphSupervisorLeaseBinding {
    pub schema: String,
    pub coordinate_task_id: String,
    pub graph_id: String,
    pub graph_node_id: String,
    pub graph_hash: String,
    pub lease_owner: String,
    pub lease_expires_at: String,
    pub verification_state: VerificationState,
    pub evidence_root: Option<String>,
}

impl GraphSupervisorLeaseBinding {
    /// Build a pending binding from an exclusive Coordinate lease.
    pub fn from_active_lease(
        coordinate_task_id: impl Into<String>,
        graph_id: impl Into<String>,
        graph_node_id: impl Into<String>,
        graph_hash: impl Into<String>,
        lease_owner: impl Into<String>,
        lease_expires_at: impl Into<String>,
    ) -> Result<Self, RuntimeAdapterError> {
        let graph_hash = graph_hash.into();
        validate_hash("graph_hash", &graph_hash)?;
        let lease_owner = lease_owner.into();
        let lease_expires_at = lease_expires_at.into();
        let coordinate_task_id = coordinate_task_id.into();
        let graph_id = graph_id.into();
        let graph_node_id = graph_node_id.into();
        for (name, value) in [
            ("coordinate_task_id", coordinate_task_id.as_str()),
            ("graph_id", graph_id.as_str()),
            ("graph_node_id", graph_node_id.as_str()),
            ("lease_owner", lease_owner.as_str()),
            ("lease_expires_at", lease_expires_at.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(RuntimeAdapterError::InvalidRecord(format!(
                    "{name} cannot be empty"
                )));
            }
        }
        Ok(Self {
            schema: GRAPH_SUPERVISOR_LEASE_BINDING_SCHEMA.to_string(),
            coordinate_task_id,
            graph_id,
            graph_node_id,
            graph_hash,
            lease_owner,
            lease_expires_at,
            verification_state: VerificationState::Pending,
            evidence_root: None,
        })
    }

    /// Apply an independent verifier handoff. Rejects empty evidence and
    /// preserves fail-closed pending state on rejection.
    pub fn apply_verifier_handoff(
        &mut self,
        accepted: bool,
        evidence_root: impl Into<String>,
    ) -> Result<(), RuntimeAdapterError> {
        let evidence_root = evidence_root.into();
        validate_hash("evidence_root", &evidence_root)?;
        if accepted {
            self.verification_state = VerificationState::Verified;
            self.evidence_root = Some(evidence_root);
        } else {
            self.verification_state = VerificationState::Rejected;
            self.evidence_root = Some(evidence_root);
        }
        Ok(())
    }

    /// True when verifier handoff completed successfully.
    pub fn handoff_verified(&self) -> bool {
        self.verification_state == VerificationState::Verified && self.evidence_root.is_some()
    }
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

/// Transport-neutral daemon response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FractaldResponse {
    /// HTTP status returned by `fractald`.
    pub status: u16,
    /// Parsed JSON response body.
    pub body: Value,
}

/// One self-contained admission document accepted by the Coordinate runner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeJobSubmission {
    /// Durable Coordinate correlation record.
    pub job: FractalRuntimeJob,
    /// Signed `fractal.work_lease.v1` envelope.
    pub signed_lease: Value,
    /// Compiled `fractal.execution_graph.v1`.
    pub graph_json: Value,
    /// Compiler identity pinned into daemon admission.
    pub compiler_version: String,
}

/// Minimal request boundary used by the poll loop and deterministic tests.
pub trait FractaldTransport {
    /// Execute one typed daemon request.
    fn execute(&self, request: &FractaldRequest) -> Result<FractaldResponse, RuntimeAdapterError>;
}

/// Blocking HTTP transport for a `fractald` API endpoint.
pub struct HttpFractaldClient {
    base_url: String,
    client: reqwest::blocking::Client,
}

impl HttpFractaldClient {
    /// Construct a client without accepting URL paths, queries, or credentials
    /// in the configured daemon origin.
    pub fn new(base_url: &str) -> Result<Self, RuntimeAdapterError> {
        let base_url = base_url.trim().trim_end_matches('/');
        let parsed = reqwest::Url::parse(base_url)
            .map_err(|error| RuntimeAdapterError::Transport(error.to_string()))?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.host_str().is_none()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || parsed.username() != ""
            || parsed.password().is_some()
            || parsed.path() != "/"
        {
            return Err(RuntimeAdapterError::InvalidRecord(
                "fractald URL must be an http(s) origin without credentials, path, query, or fragment"
                    .to_string(),
            ));
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|error| RuntimeAdapterError::Transport(error.to_string()))?;
        Ok(Self {
            base_url: base_url.to_string(),
            client,
        })
    }
}

impl FractaldTransport for HttpFractaldClient {
    fn execute(&self, request: &FractaldRequest) -> Result<FractaldResponse, RuntimeAdapterError> {
        let url = format!("{}{}", self.base_url, request.path);
        let response = match request.method.as_str() {
            "GET" => self.client.get(url).send(),
            "POST" => self.client.post(url).json(&request.body).send(),
            method => {
                return Err(RuntimeAdapterError::InvalidRecord(format!(
                    "unsupported fractald method {method}"
                )))
            }
        }
        .map_err(|error| RuntimeAdapterError::Transport(error.to_string()))?;
        let status = response.status().as_u16();
        let body = response
            .json::<Value>()
            .map_err(|error| RuntimeAdapterError::Transport(error.to_string()))?;
        Ok(FractaldResponse { status, body })
    }
}

/// Native transport for the private newline-delimited JSON Unix socket exposed
/// by `fractald`. Each request uses a fresh authenticated local connection.
pub struct UdsFractaldClient {
    socket_path: std::path::PathBuf,
}

impl UdsFractaldClient {
    /// Point the client at the daemon's protected socket.
    pub fn new(socket_path: impl Into<std::path::PathBuf>) -> Result<Self, RuntimeAdapterError> {
        let socket_path = socket_path.into();
        if !socket_path.is_absolute() {
            return Err(RuntimeAdapterError::InvalidRecord(
                "fractald socket path must be absolute".to_string(),
            ));
        }
        Ok(Self { socket_path })
    }
}

impl FractaldTransport for UdsFractaldClient {
    fn execute(&self, request: &FractaldRequest) -> Result<FractaldResponse, RuntimeAdapterError> {
        #[cfg(not(unix))]
        {
            let _ = (request, &self.socket_path);
            return Err(RuntimeAdapterError::Transport(
                "fractald Unix socket transport is unavailable on this platform".to_string(),
            ));
        }
        #[cfg(unix)]
        {
            let mut stream = UnixStream::connect(&self.socket_path)
                .map_err(|error| RuntimeAdapterError::Transport(error.to_string()))?;
            serde_json::to_writer(&mut stream, request)
                .map_err(|error| RuntimeAdapterError::Transport(error.to_string()))?;
            stream
                .write_all(b"\n")
                .and_then(|()| stream.flush())
                .map_err(|error| RuntimeAdapterError::Transport(error.to_string()))?;
            let mut line = String::new();
            BufReader::new(stream)
                .read_line(&mut line)
                .map_err(|error| RuntimeAdapterError::Transport(error.to_string()))?;
            if line.trim().is_empty() {
                return Err(RuntimeAdapterError::Transport(
                    "fractald closed the socket without a response".to_string(),
                ));
            }
            serde_json::from_str(&line)
                .map_err(|error| RuntimeAdapterError::Transport(error.to_string()))
        }
    }
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
    /// HTTP transport or response decoding failed.
    Transport(String),
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
            Self::Transport(message) => write!(formatter, "fractald transport failed: {message}"),
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
            ("graph_node_id", self.graph_node_id.as_str()),
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
        self.validate_admission_binding(&signed_lease, &graph_json)?;
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

    fn validate_admission_binding(
        &self,
        signed_lease: &Value,
        graph_json: &Value,
    ) -> Result<(), RuntimeAdapterError> {
        let lease = signed_lease
            .get("lease")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                RuntimeAdapterError::InvalidRecord(
                    "signed lease must contain a lease object".to_string(),
                )
            })?;
        for (field, expected) in [
            ("schema", "fractal.work_lease.v1"),
            ("work_hash", self.work_hash.as_str()),
            ("graph_hash", self.graph_hash.as_str()),
            ("node_id", self.graph_node_id.as_str()),
            ("provider_node", self.provider_node.as_str()),
        ] {
            if lease.get(field).and_then(Value::as_str) != Some(expected) {
                return Err(RuntimeAdapterError::IdentityMismatch(format!(
                    "signed lease {field}"
                )));
            }
        }
        if lease.get("expires_at_ms").and_then(Value::as_u64) != Some(self.lease_expires_at_ms) {
            return Err(RuntimeAdapterError::IdentityMismatch(
                "signed lease expires_at_ms".to_string(),
            ));
        }
        for field in ["lease_id", "nonce", "issuer"] {
            if lease
                .get(field)
                .and_then(Value::as_str)
                .is_none_or(|value| value.trim().is_empty())
            {
                return Err(RuntimeAdapterError::InvalidRecord(format!(
                    "signed lease {field} must be non-empty"
                )));
            }
        }
        if signed_lease
            .get("signature_algorithm")
            .and_then(Value::as_str)
            != Some("ed25519")
        {
            return Err(RuntimeAdapterError::InvalidRecord(
                "signed lease signature_algorithm must be ed25519".to_string(),
            ));
        }
        validate_lower_hex_field(signed_lease, "issuer_public_key", 64)?;
        validate_lower_hex_field(signed_lease, "signature", 128)?;
        if graph_json.get("schema").and_then(Value::as_str) != Some("fractal.execution_graph.v1")
            || graph_json.get("graph_hash").and_then(Value::as_str)
                != Some(self.graph_hash.as_str())
        {
            return Err(RuntimeAdapterError::IdentityMismatch(
                "compiled graph schema or graph_hash".to_string(),
            ));
        }
        Ok(())
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

/// Admit one signed graph and durably poll state until a terminal lifecycle or
/// the caller's bounded poll count. Terminal jobs also fetch and verify the
/// append-only evidence chain before returning.
pub fn run_runtime_job(
    transport: &impl FractaldTransport,
    store: &RuntimeJobStore,
    submission: RuntimeJobSubmission,
    poll_interval: Duration,
    max_polls: usize,
) -> Result<FractalRuntimeJob, RuntimeAdapterError> {
    if max_polls == 0 {
        return Err(RuntimeAdapterError::InvalidRecord(
            "max_polls must be positive".to_string(),
        ));
    }
    let RuntimeJobSubmission {
        mut job,
        signed_lease,
        graph_json,
        compiler_version,
    } = submission;
    let admission =
        transport.execute(&job.admit_request(signed_lease, graph_json, &compiler_version)?)?;
    job.apply_status_response(admission.status, &admission.body, unix_time_ms()?)?;
    store.put(&job)?;

    for poll_index in 0..max_polls {
        let response = transport.execute(&job.poll_request()?)?;
        job.apply_status_response(response.status, &response.body, unix_time_ms()?)?;
        store.put(&job)?;
        if runtime_state_is_terminal(job.runtime_state) {
            let evidence = transport.execute(&job.evidence_request()?)?;
            job.apply_evidence_response(evidence.status, &evidence.body, unix_time_ms()?)?;
            store.put(&job)?;
            return Ok(job);
        }
        if poll_index + 1 < max_polls && !poll_interval.is_zero() {
            thread::sleep(poll_interval);
        }
    }
    Ok(job)
}

fn runtime_state_is_terminal(state: RuntimeJobState) -> bool {
    matches!(
        state,
        RuntimeJobState::Cancelled | RuntimeJobState::Completed | RuntimeJobState::Failed
    )
}

fn unix_time_ms() -> Result<u64, RuntimeAdapterError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| RuntimeAdapterError::InvalidRecord(error.to_string()))?
        .as_millis();
    u64::try_from(millis)
        .map_err(|_| RuntimeAdapterError::InvalidRecord("system time exceeds u64".to_string()))
}

fn ensure_same_identity(
    previous: &FractalRuntimeJob,
    current: &FractalRuntimeJob,
) -> Result<(), RuntimeAdapterError> {
    for (name, same) in [
        ("work_id", previous.work_id == current.work_id),
        ("work_hash", previous.work_hash == current.work_hash),
        ("graph_id", previous.graph_id == current.graph_id),
        (
            "graph_node_id",
            previous.graph_node_id == current.graph_node_id,
        ),
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

fn validate_lower_hex_field(
    value: &Value,
    field: &str,
    expected_len: usize,
) -> Result<(), RuntimeAdapterError> {
    let text = value.get(field).and_then(Value::as_str).ok_or_else(|| {
        RuntimeAdapterError::InvalidRecord(format!("signed lease {field} is required"))
    })?;
    if text.len() != expected_len
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(RuntimeAdapterError::InvalidRecord(format!(
            "signed lease {field} must be {expected_len} lowercase hex characters"
        )));
    }
    Ok(())
}
