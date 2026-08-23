use squad::autopilot::{
    frozen_graph_supervisor_lease_fixture, frozen_graph_supervisor_lease_source,
    GRAPH_SUPERVISOR_LEASE_CONTRACT_SCHEMA, GRAPH_SUPERVISOR_LEASE_FIXTURE_GRAPH_ID,
};
use squad::fractal_runtime::{
    GraphSupervisorLeaseBinding, VerificationState, GRAPH_SUPERVISOR_LEASE_BINDING_SCHEMA,
};
use squad::service::prove_graph_supervisor_lease_boundary;
use squad::store::Store;

#[test]
fn frozen_graph_fixture_compiles_deterministically() {
    let first = frozen_graph_supervisor_lease_fixture();
    let second = frozen_graph_supervisor_lease_fixture();
    assert_eq!(first, second);
    assert_eq!(first.graph_id, GRAPH_SUPERVISOR_LEASE_FIXTURE_GRAPH_ID);
    first.validate().expect("frozen fixture must validate");
    assert_eq!(
        frozen_graph_supervisor_lease_source(),
        format!("fractal-graph:{}:{}", first.graph_id, first.graph_hash)
    );
}

#[test]
fn lease_binding_verifier_handoff_accepts_and_rejects() {
    let mut binding = GraphSupervisorLeaseBinding::from_active_lease(
        "graph-node:compile",
        GRAPH_SUPERVISOR_LEASE_FIXTURE_GRAPH_ID,
        "compile",
        &frozen_graph_supervisor_lease_fixture().graph_hash,
        "int078-worker-a",
        "2026-08-23T19:00:00Z",
    )
    .expect("binding should construct");
    assert_eq!(binding.schema, GRAPH_SUPERVISOR_LEASE_BINDING_SCHEMA);
    assert_eq!(binding.verification_state, VerificationState::Pending);

    let evidence = format!("sha256:{}", "a".repeat(64));
    binding
        .apply_verifier_handoff(true, &evidence)
        .expect("accept handoff");
    assert!(binding.handoff_verified());

    let mut rejected = GraphSupervisorLeaseBinding::from_active_lease(
        "graph-node:execute",
        GRAPH_SUPERVISOR_LEASE_FIXTURE_GRAPH_ID,
        "execute",
        &frozen_graph_supervisor_lease_fixture().graph_hash,
        "int078-worker-b",
        "2026-08-23T19:00:00Z",
    )
    .expect("binding should construct");
    rejected
        .apply_verifier_handoff(false, &evidence)
        .expect("reject handoff");
    assert_eq!(rejected.verification_state, VerificationState::Rejected);
    assert!(!rejected.handoff_verified());
}

#[test]
fn frozen_fixture_proves_graph_supervisor_lease_boundary() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(&directory.path().join("coordinate.sqlite3")).unwrap();

    let proof = prove_graph_supervisor_lease_boundary(&store)
        .expect("INT-078 lease boundary proof must succeed");

    assert_eq!(proof.schema, GRAPH_SUPERVISOR_LEASE_CONTRACT_SCHEMA);
    assert_eq!(proof.graph_id, GRAPH_SUPERVISOR_LEASE_FIXTURE_GRAPH_ID);
    assert!(
        proof.all_behaviors_proven(),
        "missing behaviors in proof notes: {:?}",
        proof.notes
    );
    assert!(proof.dependency_aware_checkout);
    assert!(proof.at_most_once_active_lease);
    assert!(proof.bounded_retry);
    assert!(proof.verifier_handoff);
    assert!(proof.expiry_recovery);
    assert!(proof.evidence_hash.starts_with("sha256:"));
    assert!(!proof.notes.is_empty());
}
