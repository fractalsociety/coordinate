//! P3.4 — interchangeability proof.
//!
//! One node contract, executed by two different providers (Cursor via
//! `stream-json` and a second worker via a tmux-pane text stream), must yield
//! *equivalent evidence*: after normalization both collapse to the same
//! `BridgeTaskReport` schema fields and the same verifier verdict, so the
//! providers are substitutable. Provider-specific metadata (the raw evidence
//! blob / stream format) is allowed to differ. The proof also shows Cursor is
//! an execute-only worker that never drives planning. Fully offline — no
//! `cursor-agent`, no network, no spawned verifier process.

use sha2::{Digest, Sha256};
use squad::host_bridge::{
    cursor_worker_command, parse_latest_marker, BridgeTaskReport, PaneMarker,
};
use squad::node_verifier::{
    evaluate_evidence, CheckEvidence, NodeEvidence, VerificationDecision, VerifierVerdict,
};

/// The one shared node contract's report body. `{RAW}` stands in for the
/// provider-specific raw evidence blob, which is expected to differ.
fn report_body(raw: &str) -> String {
    format!(
        concat!(
            r#"{{"summary":"reverse-cli implemented","#,
            r#""filesInspected":["src/main.rs"],"#,
            r#""changedFiles":["src/main.rs","tests/reverse.rs"],"#,
            r#""testsRun":["cargo test"],"#,
            r#""verification":"passed","risks":"none","rawReport":"{raw}"}}"#
        ),
        raw = raw
    )
}

/// Cursor delivers evidence as a `cursor-agent --output-format stream-json`
/// event whose `result` string carries the report marker.
fn cursor_stream_output() -> String {
    serde_json::json!({
        "type": "result",
        "result": format!("Work complete.\nCOORDINATE_REPORT_JSON: {}", report_body("cursor stream evidence")),
    })
    .to_string()
}

/// A second provider (codex/claude) delivers the same report as plain lines in
/// a tmux pane.
fn tmux_pane_output() -> String {
    format!(
        "running node…\n⏺ COORDINATE_REPORT_JSON: {}\n",
        report_body("tmux pane evidence")
    )
}

fn report_of(output: &str) -> BridgeTaskReport {
    match parse_latest_marker(output)
        .expect("marker parses")
        .expect("a report marker is present")
    {
        PaneMarker::Report(report) => report,
        other => panic!("expected a report marker, got {other:?}"),
    }
}

/// A provider-independent evidence root over only the schema fields — the raw
/// blob is deliberately excluded, so equivalent work hashes identically no
/// matter which worker produced it.
fn evidence_root(report: &BridgeTaskReport) -> String {
    let mut hasher = Sha256::new();
    let mut field = |bytes: &[u8]| {
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    };
    field(report.summary.as_bytes());
    for value in &report.files_inspected {
        field(value.as_bytes());
    }
    for value in &report.changed_files {
        field(value.as_bytes());
    }
    for value in &report.tests_run {
        field(value.as_bytes());
    }
    field(report.verification.as_bytes());
    field(report.risks.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

/// The same deterministic mapping from a normalized report to node evidence,
/// applied identically to every provider.
fn evidence_from(report: &BridgeTaskReport) -> NodeEvidence {
    let root = evidence_root(report);
    let passed = report.verification.eq_ignore_ascii_case("passed");
    NodeEvidence {
        public_check: CheckEvidence {
            passed,
            evidence_hash: root.clone(),
            detail: report.summary.clone(),
        },
        hidden_regression: Some(CheckEvidence {
            passed,
            evidence_hash: root.clone(),
            detail: "hidden regression".to_string(),
        }),
        verifier_verdicts: vec![VerifierVerdict {
            verifier_id: "fractal-verify".to_string(),
            verdict: "pass".to_string(),
            confidence_bp: 9_000,
            evidence_hash: root,
        }],
    }
}

#[test]
fn same_contract_yields_equivalent_evidence_across_providers() {
    let cursor = report_of(&cursor_stream_output());
    let codex = report_of(&tmux_pane_output());

    // Schema fields are provider-independent…
    assert_eq!(cursor.summary, codex.summary);
    assert_eq!(cursor.files_inspected, codex.files_inspected);
    assert_eq!(cursor.changed_files, codex.changed_files);
    assert_eq!(cursor.tests_run, codex.tests_run);
    assert_eq!(cursor.verification, codex.verification);
    assert_eq!(cursor.risks, codex.risks);

    // …while the raw provider evidence blob is allowed to differ.
    assert_ne!(cursor.raw_report, codex.raw_report);

    // The evidence root over the schema fields is byte-identical.
    assert_eq!(evidence_root(&cursor), evidence_root(&codex));

    // And the verifier reaches the same verdict from either provider.
    let cursor_outcome =
        evaluate_evidence(evidence_from(&cursor), 1, true).expect("cursor verdict");
    let codex_outcome = evaluate_evidence(evidence_from(&codex), 1, true).expect("codex verdict");
    assert_eq!(cursor_outcome.decision, VerificationDecision::Complete);
    assert_eq!(codex_outcome.decision, VerificationDecision::Complete);
    assert_eq!(cursor_outcome.evidence_hash, codex_outcome.evidence_hash);
    assert_eq!(cursor_outcome.evidence, codex_outcome.evidence);
}

#[test]
fn cursor_worker_is_execution_only() {
    let command = cursor_worker_command("execute leased node patch for graph g#node:patch");

    // Executes the leased node headlessly under a sandbox…
    assert!(command.starts_with("cursor-agent -p "));
    assert!(command.contains("--output-format stream-json"));
    assert!(command.contains("--sandbox enabled"));

    // …and never drives intent/planning.
    let lowered = command.to_ascii_lowercase();
    assert!(!lowered.contains("--mode plan"));
    assert!(!lowered.contains(" plan"));
    assert!(!lowered.contains("intent"));
}
