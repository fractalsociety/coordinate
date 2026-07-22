use serde_json::json;
use squad::fractal_runtime::{
    CancellationState, FractalRuntimeJob, RuntimeAdapterError, RuntimeJobMode, RuntimeJobState,
    RuntimeJobStore, VerificationState, RUNTIME_JOB_SCHEMA_V1,
};

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

#[test]
fn emits_typed_admit_and_poll_requests_without_log_scraping() {
    let job = job();
    let admit = job
        .admit_request(
            json!({"lease": "signed"}),
            json!({"schema": "fractal.execution_graph.v1"}),
            "fractal-harnessc/0.1.0",
        )
        .unwrap();
    assert_eq!(admit.method, "POST");
    assert_eq!(admit.path, "/v1/work/admit");
    assert_eq!(admit.body["work_hash"], job.work_hash);
    assert_eq!(admit.body["graph_hash"], job.graph_hash);

    let poll = job.poll_request().unwrap();
    let evidence = job.evidence_request().unwrap();
    assert_eq!(poll.path, "/v1/work/work:repair-1");
    assert_eq!(evidence.path, "/v1/work/work:repair-1/events");
    assert!(!serde_json::to_string(&(poll, evidence))
        .unwrap()
        .contains("log"));
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
