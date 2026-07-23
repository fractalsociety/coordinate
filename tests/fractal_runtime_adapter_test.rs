use assert_cmd::Command;
use serde_json::json;
use squad::fractal_runtime::{
    run_runtime_job, CancellationState, FractalRuntimeJob, FractaldRequest, FractaldResponse,
    FractaldTransport, HttpFractaldClient, RuntimeAdapterError, RuntimeJobMode, RuntimeJobState,
    RuntimeJobStore, RuntimeJobSubmission, UdsFractaldClient, VerificationState,
    RUNTIME_JOB_SCHEMA_V1,
};
use std::collections::VecDeque;
#[cfg(unix)]
use std::io::{BufRead, BufReader};
use std::io::{Read, Write};
use std::net::TcpListener;
#[cfg(unix)]
use std::os::unix::net::UnixListener;
use std::sync::Mutex;
use std::time::Duration;

fn hash(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

fn job() -> FractalRuntimeJob {
    FractalRuntimeJob {
        schema: RUNTIME_JOB_SCHEMA_V1.to_string(),
        coordinate_job_id: "coordinate:job-1".to_string(),
        work_id: "work:repair-1".to_string(),
        work_hash: hash('a'),
        graph_id: "graph:repair-1".to_string(),
        graph_node_id: "repair".to_string(),
        graph_hash: hash('b'),
        provider_node: "node:mac-m3".to_string(),
        local_execution_id: "local:exec-1".to_string(),
        lease_expires_at_ms: 200_000,
        resume_checkpoint_hash: None,
        resource_class: "standard".to_string(),
        mode: RuntimeJobMode::Background,
        runtime_state: RuntimeJobState::Submitted,
        cancellation_state: CancellationState::None,
        evidence_root: None,
        verification_state: VerificationState::Pending,
        updated_at_ms: 100_000,
    }
}

fn signed_lease(job: &FractalRuntimeJob) -> serde_json::Value {
    json!({
        "lease": {
            "schema": "fractal.work_lease.v1",
            "lease_id": "lease:repair-1",
            "work_hash": job.work_hash,
            "graph_hash": job.graph_hash,
            "node_id": job.graph_node_id,
            "provider_node": job.provider_node,
            "allowed_capability_ids": ["code.repair"],
            "memory_scopes": ["project:test"],
            "tool_scopes": ["workspace.write"],
            "input_artifact_hashes": [],
            "output_artifact_root": "artifact:repair-1",
            "max_tokens": 1000,
            "max_time_ms": 60000,
            "max_cpu_millis": 60000,
            "max_memory_mib": 512,
            "max_disk_mib": 128,
            "max_cost_microunits": 1000,
            "not_before_ms": 100000,
            "expires_at_ms": job.lease_expires_at_ms,
            "nonce": "nonce:repair-1",
            "issuer": "coordinate:test"
        },
        "signature_algorithm": "ed25519",
        "issuer_public_key": "1".repeat(64),
        "signature": "2".repeat(128)
    })
}

fn graph_json(job: &FractalRuntimeJob) -> serde_json::Value {
    json!({
        "schema": "fractal.execution_graph.v1",
        "graph_hash": job.graph_hash
    })
}

#[test]
fn emits_typed_admit_and_poll_requests_without_log_scraping() {
    let job = job();
    let admit = job
        .admit_request(
            signed_lease(&job),
            graph_json(&job),
            "fractal-harnessc/0.1.0",
        )
        .unwrap();
    assert_eq!(admit.method, "POST");
    assert_eq!(admit.path, "/v1/work/admit");
    assert_eq!(admit.body["work_hash"], job.work_hash);
    assert_eq!(admit.body["graph_hash"], job.graph_hash);
    assert_eq!(admit.body["lease"]["lease"]["node_id"], job.graph_node_id);
    assert_eq!(
        admit.body["lease"]["signature_algorithm"],
        serde_json::Value::String("ed25519".to_string())
    );

    let poll = job.poll_request().unwrap();
    let evidence = job.evidence_request().unwrap();
    assert_eq!(poll.path, "/v1/work/work:repair-1");
    assert_eq!(evidence.path, "/v1/work/work:repair-1/events");
    assert!(!serde_json::to_string(&(poll, evidence))
        .unwrap()
        .contains("log"));
}

#[test]
fn admission_rejects_unsigned_or_identity_substituted_node_leases() {
    let job = job();
    let unsigned = job.admit_request(
        json!({"lease": {"schema": "fractal.work_lease.v1"}}),
        graph_json(&job),
        "fractal-harnessc/0.1.0",
    );
    assert!(matches!(
        unsigned,
        Err(RuntimeAdapterError::IdentityMismatch(_)) | Err(RuntimeAdapterError::InvalidRecord(_))
    ));

    let mut substituted = signed_lease(&job);
    substituted["lease"]["node_id"] = json!("other-node");
    assert!(matches!(
        job.admit_request(
            substituted,
            graph_json(&job),
            "fractal-harnessc/0.1.0"
        ),
        Err(RuntimeAdapterError::IdentityMismatch(field))
            if field == "signed lease node_id"
    ));

    let mut malformed_signature = signed_lease(&job);
    malformed_signature["signature"] = json!("not-a-signature");
    assert!(matches!(
        job.admit_request(
            malformed_signature,
            graph_json(&job),
            "fractal-harnessc/0.1.0"
        ),
        Err(RuntimeAdapterError::InvalidRecord(_))
    ));
}

#[test]
fn poll_updates_lifecycle_but_rejects_identity_or_lease_broadening() {
    let mut job = job();
    job.apply_status_response(
        200,
        &json!({
            "work_id": job.work_id,
            "work_hash": job.work_hash,
            "state": "executing",
            "lease_expires_at_ms": 200_000
        }),
        101_000,
    )
    .unwrap();
    assert_eq!(job.runtime_state, RuntimeJobState::Executing);
    assert_eq!(job.updated_at_ms, 101_000);

    let mismatch = job.apply_status_response(
        200,
        &json!({"work_id": "work:other", "state": "completed"}),
        102_000,
    );
    assert!(matches!(
        mismatch,
        Err(RuntimeAdapterError::IdentityMismatch(_))
    ));
    let broadened = job.apply_status_response(
        200,
        &json!({
            "work_id": job.work_id,
            "state": "executing",
            "lease_expires_at_ms": 200_001
        }),
        102_000,
    );
    assert!(matches!(
        broadened,
        Err(RuntimeAdapterError::IdentityMismatch(_))
    ));
}

#[test]
fn cancellation_and_interrupted_resume_are_durable_explicit_states() {
    let mut job = job();
    let cancel = job.cancel_request("operator", 110_000).unwrap();
    assert_eq!(cancel.path, "/v1/work/work:repair-1/cancel");
    assert_eq!(job.cancellation_state, CancellationState::Requested);

    job.apply_status_response(
        200,
        &json!({"work_id": job.work_id, "state": "cancelling"}),
        111_000,
    )
    .unwrap();
    assert_eq!(job.cancellation_state, CancellationState::Cancelling);
    assert!(matches!(
        job.record_resume_checkpoint(&hash('c'), 112_000),
        Err(RuntimeAdapterError::CheckpointNotResumable)
    ));

    job.apply_status_response(
        200,
        &json!({"work_id": job.work_id, "state": "interrupted"}),
        113_000,
    )
    .unwrap();
    job.record_resume_checkpoint(&hash('c'), 114_000).unwrap();
    assert_eq!(
        job.resume_checkpoint_hash.as_deref(),
        Some(hash('c').as_str())
    );
}

#[test]
fn evidence_poll_retains_only_a_verified_chain_root() {
    let mut job = job();
    job.apply_evidence_response(
        200,
        &json!({
            "work_id": job.work_id,
            "chain_verified": true,
            "events": [
                {"event_hash": hash('d'), "payload": {"state": "admitted"}},
                {"event_hash": hash('e'), "payload": {"state": "completed"}}
            ]
        }),
        120_000,
    )
    .unwrap();
    assert_eq!(job.evidence_root.as_deref(), Some(hash('e').as_str()));
    assert_eq!(job.verification_state, VerificationState::Verified);

    let rejected = job.apply_evidence_response(
        200,
        &json!({"work_id": job.work_id, "chain_verified": false, "events": []}),
        121_000,
    );
    assert!(rejected.is_err());
    assert_eq!(job.verification_state, VerificationState::Rejected);
}

#[test]
fn sqlite_round_trip_preserves_record_and_rejects_identity_substitution() {
    let directory = tempfile::tempdir().unwrap();
    let store = RuntimeJobStore::open(&directory.path().join("coordinate.sqlite3")).unwrap();
    let mut job = job();
    store.put(&job).unwrap();
    assert_eq!(
        store.get(&job.coordinate_job_id).unwrap(),
        Some(job.clone())
    );

    job.runtime_state = RuntimeJobState::Executing;
    job.updated_at_ms += 1;
    store.put(&job).unwrap();
    assert_eq!(
        store
            .get(&job.coordinate_job_id)
            .unwrap()
            .unwrap()
            .runtime_state,
        RuntimeJobState::Executing
    );

    job.graph_hash = hash('f');
    assert!(matches!(
        store.put(&job),
        Err(RuntimeAdapterError::IdentityMismatch(_))
    ));
}

#[test]
fn unsafe_work_identifier_cannot_change_daemon_api_path() {
    let mut job = job();
    job.work_id = "work/other/events".to_string();
    assert!(matches!(
        job.poll_request(),
        Err(RuntimeAdapterError::InvalidRecord(_))
    ));
}

struct MockFractald {
    responses: Mutex<VecDeque<FractaldResponse>>,
    requests: Mutex<Vec<FractaldRequest>>,
}

impl FractaldTransport for MockFractald {
    fn execute(&self, request: &FractaldRequest) -> Result<FractaldResponse, RuntimeAdapterError> {
        self.requests.lock().unwrap().push(request.clone());
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| RuntimeAdapterError::Transport("missing mock response".to_string()))
    }
}

#[test]
fn runner_admits_polls_terminal_state_and_persists_verified_evidence() {
    let directory = tempfile::tempdir().unwrap();
    let store = RuntimeJobStore::open(&directory.path().join("coordinate.sqlite3")).unwrap();
    let transport = MockFractald {
        responses: Mutex::new(VecDeque::from([
            FractaldResponse {
                status: 201,
                body: json!({
                    "work_id": "work:repair-1",
                    "state": "admitted",
                    "lease_expires_at_ms": 200_000
                }),
            },
            FractaldResponse {
                status: 200,
                body: json!({
                    "work_id": "work:repair-1",
                    "work_hash": hash('a'),
                    "state": "completed",
                    "lease_expires_at_ms": 200_000
                }),
            },
            FractaldResponse {
                status: 200,
                body: json!({
                    "work_id": "work:repair-1",
                    "chain_verified": true,
                    "events": [{"event_hash": hash('e')}]
                }),
            },
        ])),
        requests: Mutex::new(Vec::new()),
    };
    let completed = run_runtime_job(
        &transport,
        &store,
        RuntimeJobSubmission {
            job: job(),
            signed_lease: signed_lease(&job()),
            graph_json: graph_json(&job()),
            compiler_version: "fractal-harnessc/0.1.0".to_string(),
        },
        Duration::ZERO,
        2,
    )
    .unwrap();

    assert_eq!(completed.runtime_state, RuntimeJobState::Completed);
    assert_eq!(completed.verification_state, VerificationState::Verified);
    assert_eq!(completed.evidence_root.as_deref(), Some(hash('e').as_str()));
    assert_eq!(
        store
            .get("coordinate:job-1")
            .unwrap()
            .unwrap()
            .runtime_state,
        RuntimeJobState::Completed
    );
    assert_eq!(
        transport
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.path.as_str())
            .collect::<Vec<_>>(),
        [
            "/v1/work/admit",
            "/v1/work/work:repair-1",
            "/v1/work/work:repair-1/events"
        ]
    );
    let requests = transport.requests.lock().unwrap();
    assert_eq!(
        requests[0].body["lease"]["lease"]["node_id"],
        serde_json::Value::String("repair".to_string())
    );
    assert_eq!(
        requests[0].body["lease"]["lease"]["provider_node"],
        serde_json::Value::String("node:mac-m3".to_string())
    );
}

#[test]
fn http_transport_calls_fractald_json_endpoint() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 2048];
        let count = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..count]);
        assert!(request.starts_with("GET /v1/work/work:repair-1 HTTP/1.1"));
        let body = r#"{"work_id":"work:repair-1","state":"executing"}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    });
    let client = HttpFractaldClient::new(&format!("http://{address}")).unwrap();
    let response = client
        .execute(&FractaldRequest {
            method: "GET".to_string(),
            path: "/v1/work/work:repair-1".to_string(),
            body: serde_json::Value::Null,
        })
        .unwrap();

    assert_eq!(response.status, 200);
    assert_eq!(response.body["state"], "executing");
    server.join().unwrap();
}

#[test]
fn coordinate_binary_runs_admission_poll_and_evidence_flow() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for index in 0..3 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 8192];
            let count = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..count]);
            let (status, body) = match index {
                0 => {
                    assert!(request.starts_with("POST /v1/work/admit HTTP/1.1"));
                    (
                        "201 Created",
                        json!({
                            "work_id": "work:repair-1",
                            "state": "admitted",
                            "lease_expires_at_ms": 200_000
                        }),
                    )
                }
                1 => {
                    assert!(request.starts_with("GET /v1/work/work:repair-1 HTTP/1.1"));
                    (
                        "200 OK",
                        json!({
                            "work_id": "work:repair-1",
                            "work_hash": hash('a'),
                            "state": "completed",
                            "lease_expires_at_ms": 200_000
                        }),
                    )
                }
                _ => {
                    assert!(request.starts_with("GET /v1/work/work:repair-1/events HTTP/1.1"));
                    (
                        "200 OK",
                        json!({
                            "work_id": "work:repair-1",
                            "chain_verified": true,
                            "events": [{"event_hash": hash('e')}]
                        }),
                    )
                }
            };
            let body = body.to_string();
            write!(
                stream,
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    });
    let directory = tempfile::tempdir().unwrap();
    let input_path = directory.path().join("submission.json");
    let database_path = directory.path().join("coordinate.sqlite3");
    std::fs::write(
        &input_path,
        serde_json::to_vec(&RuntimeJobSubmission {
            job: job(),
            signed_lease: signed_lease(&job()),
            graph_json: graph_json(&job()),
            compiler_version: "fractal-harnessc/0.1.0".to_string(),
        })
        .unwrap(),
    )
    .unwrap();

    let output = Command::cargo_bin("squad")
        .unwrap()
        .args([
            "fractal-runtime",
            "run",
            "--input",
            input_path.to_str().unwrap(),
            "--db",
            database_path.to_str().unwrap(),
            "--fractald-url",
            &format!("http://{address}"),
            "--poll-ms",
            "0",
            "--max-polls",
            "2",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let completed: FractalRuntimeJob = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(completed.runtime_state, RuntimeJobState::Completed);
    assert_eq!(completed.verification_state, VerificationState::Verified);
    server.join().unwrap();
}

#[test]
#[cfg(unix)]
fn native_transport_uses_fractald_newline_json_socket_protocol() {
    let directory = tempfile::tempdir().unwrap();
    let socket_path = directory.path().join("fractald.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        let request: FractaldRequest = serde_json::from_str(&line).unwrap();
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/v1/work/work:repair-1");
        writeln!(
            stream,
            "{}",
            json!({
                "status": 200,
                "body": {"work_id": "work:repair-1", "state": "executing"}
            })
        )
        .unwrap();
    });
    let client = UdsFractaldClient::new(socket_path).unwrap();
    let response = client
        .execute(&FractaldRequest {
            method: "GET".to_string(),
            path: "/v1/work/work:repair-1".to_string(),
            body: serde_json::Value::Null,
        })
        .unwrap();

    assert_eq!(response.status, 200);
    assert_eq!(response.body["state"], "executing");
    server.join().unwrap();
}
