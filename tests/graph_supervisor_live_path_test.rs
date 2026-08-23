use assert_cmd::Command;
use axum::body::{to_bytes, Body};
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use squad::autopilot::frozen_graph_supervisor_lease_fixture;
use squad::service::router;
use squad::store::Store;
use tempfile::TempDir;
use tower::ServiceExt;

async fn json_request(app: Router, method: Method, uri: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap();
    (status, body)
}

async fn register_worker(app: &Router, worker_id: &str) {
    let (status, body) = json_request(
        app.clone(),
        Method::POST,
        "/workers/register",
        json!({
            "id": worker_id,
            "kind": "codex",
            "role": "coding_worker",
            "capacity": 1
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "worker registration: {body}");
}

fn report_body(worker_id: &str, node_id: &str, attempt: &str) -> Value {
    json!({
        "workerId": worker_id,
        "summary": format!("{node_id} {attempt} complete"),
        "filesInspected": ["src/graph_supervisor.rs"],
        "changedFiles": [],
        "testsRun": ["cargo test --test graph_supervisor_live_path_test"],
        "verification": format!("{node_id} {attempt} evidence captured"),
        "risks": "none",
        "rawReport": format!("{node_id}:{attempt}")
    })
}

async fn lease_node(app: &Router, graph_hash: &str, node_id: &str, worker_id: &str) -> Value {
    let (status, body) = json_request(
        app.clone(),
        Method::POST,
        &format!("/graphs/{graph_hash}/nodes/{node_id}/lease"),
        json!({"workerId": worker_id, "leaseSecs": 300}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "lease {node_id}: {body}");
    assert_eq!(body["nodeId"], node_id);
    assert_eq!(body["leaseOwner"], worker_id);
    body
}

async fn report_node(
    app: &Router,
    graph_hash: &str,
    node_id: &str,
    worker_id: &str,
    attempt: &str,
) -> Value {
    let (status, body) = json_request(
        app.clone(),
        Method::POST,
        &format!("/graphs/{graph_hash}/nodes/{node_id}/report"),
        report_body(worker_id, node_id, attempt),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "report {node_id}: {body}");
    assert_eq!(body["nodeId"], node_id);
    assert_eq!(body["task"]["state"], "reported");
    body
}

async fn verify_node(
    app: &Router,
    graph_hash: &str,
    node_id: &str,
    verifier_id: &str,
    decision: &str,
    evidence_nibble: char,
) -> (StatusCode, Value) {
    json_request(
        app.clone(),
        Method::POST,
        &format!("/graphs/{graph_hash}/nodes/{node_id}/verify"),
        json!({
            "verifierId": verifier_id,
            "decision": decision,
            "summary": format!("{node_id} independently {decision}ed"),
            "evidenceHash": format!("sha256:{}", evidence_nibble.to_string().repeat(64))
        }),
    )
    .await
}

#[tokio::test]
async fn coordinate_http_live_path_survives_restart_retries_and_projects_terminal_state() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("coordinate.sqlite3");
    let graph = frozen_graph_supervisor_lease_fixture();
    let graph_hash = graph.graph_hash.clone();
    let app = router(db_path.clone());

    register_worker(&app, "live-worker").await;
    register_worker(&app, "live-verifier").await;

    let (status, compiled) = json_request(
        app.clone(),
        Method::POST,
        "/graphs/compile",
        serde_json::to_value(&graph).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "compile: {compiled}");
    assert_eq!(compiled["persisted"], true);
    assert_eq!(compiled["reconcile"]["enqueued"][0]["nodeId"], "compile");
    assert_eq!(compiled["projection"]["status"], "pending");

    // A fresh router represents a daemon restart. The graph and queue state
    // must replay from SQLite rather than relying on process-local state.
    let restarted = router(db_path.clone());
    let (status, replayed) = json_request(
        restarted.clone(),
        Method::POST,
        "/graphs/compile",
        serde_json::to_value(&graph).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "restart replay: {replayed}");
    assert_eq!(replayed["persisted"], false);
    assert_eq!(replayed["contentHash"], compiled["contentHash"]);
    assert_eq!(
        replayed["reconcile"]["alreadyEnqueued"][0]["nodeId"],
        "compile"
    );

    lease_node(&restarted, &graph_hash, "compile", "live-worker").await;
    let first_report =
        report_node(&restarted, &graph_hash, "compile", "live-worker", "first").await;
    assert_eq!(first_report["duplicate"], false);

    // Identical worker-report delivery is idempotent across HTTP retries.
    let duplicate = report_node(&restarted, &graph_hash, "compile", "live-worker", "first").await;
    assert_eq!(duplicate["duplicate"], true);
    assert_eq!(duplicate["reportHash"], first_report["reportHash"]);

    let mut altered_report = report_body("live-worker", "compile", "first");
    altered_report["testsRun"] = json!(["cargo test --all"]);
    let (status, altered) = json_request(
        restarted.clone(),
        Method::POST,
        &format!("/graphs/{graph_hash}/nodes/compile/report"),
        altered_report,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "altered report: {altered}");
    assert!(altered["error"]
        .as_str()
        .unwrap()
        .contains("conflicts with the report already recorded"));

    // The lease owner is not an independent verifier and must fail closed.
    let (status, self_verify) = verify_node(
        &restarted,
        &graph_hash,
        "compile",
        "live-worker",
        "accept",
        'd',
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "self verify: {self_verify}"
    );
    assert!(self_verify["error"]
        .as_str()
        .unwrap()
        .contains("cannot be verified by its own lease owner"));

    let (status, accepted) = verify_node(
        &restarted,
        &graph_hash,
        "compile",
        "live-verifier",
        "accept",
        'd',
    )
    .await;
    assert_eq!(status, StatusCode::OK, "compile verify: {accepted}");
    assert_eq!(accepted["task"]["state"], "complete");
    assert_eq!(accepted["reconcile"]["enqueued"][0]["nodeId"], "execute");

    lease_node(&restarted, &graph_hash, "execute", "live-worker").await;
    report_node(&restarted, &graph_hash, "execute", "live-worker", "first").await;
    let (status, rejected) = verify_node(
        &restarted,
        &graph_hash,
        "execute",
        "live-verifier",
        "reject",
        'e',
    )
    .await;
    assert_eq!(status, StatusCode::OK, "execute reject: {rejected}");
    assert_eq!(rejected["task"]["state"], "queued");
    assert_eq!(rejected["task"]["attempt"], 1);
    assert_eq!(rejected["retriesExhausted"], false);

    // Retry survives another service restart and can settle successfully.
    let restarted_again = router(db_path.clone());
    lease_node(&restarted_again, &graph_hash, "execute", "live-worker").await;
    report_node(
        &restarted_again,
        &graph_hash,
        "execute",
        "live-worker",
        "retry",
    )
    .await;
    let (status, accepted) = verify_node(
        &restarted_again,
        &graph_hash,
        "execute",
        "live-verifier",
        "accept",
        'f',
    )
    .await;
    assert_eq!(status, StatusCode::OK, "execute retry verify: {accepted}");
    assert_eq!(accepted["task"]["state"], "complete");
    assert_eq!(accepted["task"]["attempt"], 1);
    assert_eq!(accepted["reconcile"]["enqueued"][0]["nodeId"], "verify");

    lease_node(&restarted_again, &graph_hash, "verify", "live-worker").await;
    report_node(
        &restarted_again,
        &graph_hash,
        "verify",
        "live-worker",
        "first",
    )
    .await;
    let (status, terminal) = verify_node(
        &restarted_again,
        &graph_hash,
        "verify",
        "live-verifier",
        "accept",
        'a',
    )
    .await;
    assert_eq!(status, StatusCode::OK, "terminal verify: {terminal}");
    assert_eq!(terminal["projection"]["status"], "complete");
    assert_eq!(terminal["projection"]["terminal"], true);
    assert_eq!(terminal["projection"]["complete"], 3);
    assert_eq!(terminal["projection"]["pending"], 0);
    assert_eq!(terminal["projection"]["inFlight"], 0);

    let (status, projection) = json_request(
        restarted_again,
        Method::GET,
        &format!("/graphs/{graph_hash}/projection"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "projection: {projection}");
    assert_eq!(projection["status"], "complete");
    assert!(projection["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .all(|node| node["terminal"] == true && node["evidenceHash"].is_string()));
}

#[test]
fn graph_supervisor_main_path_persists_and_replays_projection() {
    let tmp = TempDir::new().unwrap();
    let graph_path = tmp.path().join("graph.json");
    let db_path = tmp.path().join("coordinate.sqlite3");
    let graph = frozen_graph_supervisor_lease_fixture();
    std::fs::write(&graph_path, serde_json::to_vec(&graph).unwrap()).unwrap();

    let first = Command::cargo_bin("squad")
        .unwrap()
        .args([
            "graph-supervisor",
            "--graph",
            graph_path.to_str().unwrap(),
            "--db",
            db_path.to_str().unwrap(),
            "--projection",
        ])
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first["persisted"], true);
    assert_eq!(first["projection"]["status"], "pending");

    let second = Command::cargo_bin("squad")
        .unwrap()
        .args([
            "graph-supervisor",
            "--graph",
            graph_path.to_str().unwrap(),
            "--db",
            db_path.to_str().unwrap(),
            "--projection",
        ])
        .output()
        .unwrap();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(second["persisted"], false);
    assert_eq!(second["contentHash"], first["contentHash"]);

    let store = Store::open(&db_path).unwrap();
    let persisted = store
        .service_get_execution_graph(&graph.graph_hash)
        .unwrap()
        .unwrap();
    assert_eq!(persisted.graph_id, graph.graph_id);
    assert_eq!(persisted.content_hash, first["contentHash"]);
}
