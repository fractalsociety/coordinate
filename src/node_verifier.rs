use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NodeVerifierConfig {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub timeout_secs: Option<u64>,
    pub min_verifiers: Option<u32>,
    pub require_hidden_regression: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeVerificationRequest<'a, T: Serialize> {
    pub task_id: &'a str,
    pub node_id: &'a str,
    pub acceptance_criteria: &'a [String],
    pub report: &'a T,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CheckEvidence {
    pub passed: bool,
    pub evidence_hash: String,
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerifierVerdict {
    pub verifier_id: String,
    pub verdict: String,
    pub confidence_bp: u32,
    pub evidence_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NodeEvidence {
    pub public_check: CheckEvidence,
    pub hidden_regression: Option<CheckEvidence>,
    #[serde(default)]
    pub verifier_verdicts: Vec<VerifierVerdict>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerificationOutcome {
    pub decision: VerificationDecision,
    pub summary: String,
    pub evidence_hash: String,
    pub evidence: NodeEvidence,
    pub missing: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VerificationDecision {
    Complete,
    Retry,
    Escalate,
}

pub fn run_node_verifier<T: Serialize>(
    config: &NodeVerifierConfig,
    request: &NodeVerificationRequest<'_, T>,
) -> Result<VerificationOutcome> {
    if config.program.trim().is_empty() {
        bail!("node verifier program cannot be empty");
    }
    let input = serde_json::to_vec(request).context("failed to encode node verifier request")?;
    let mut child = Command::new(&config.program)
        .args(&config.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to start node verifier {}", config.program))?;
    let mut stdin = child
        .stdin
        .take()
        .context("node verifier stdin unavailable")?;
    let mut stdout = child
        .stdout
        .take()
        .context("node verifier stdout unavailable")?;
    let mut stderr = child
        .stderr
        .take()
        .context("node verifier stderr unavailable")?;
    // Write the request on a dedicated thread so stdout/stderr are drained
    // concurrently. Writing the (potentially large) request inline would
    // deadlock if the verifier fills its stdout pipe before draining stdin —
    // and that deadlock would precede the timeout loop below, hanging forever.
    let stdin_writer = std::thread::spawn(move || {
        let result = stdin.write_all(&input);
        drop(stdin); // send EOF regardless
        result
    });
    let stdout_reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        stdout.read_to_end(&mut output).map(|_| output)
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        stderr.read_to_end(&mut output).map(|_| output)
    });

    let timeout = Duration::from_secs(config.timeout_secs.unwrap_or(300).max(1));
    let started = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!("node verifier timed out after {}s", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let status = child.wait().context("failed to wait for node verifier")?;
    // Join the writer so the thread cannot outlive this call. A broken-pipe
    // error is benign when the verifier exited/was killed early, so it only
    // surfaces when the verifier itself succeeded.
    let write_result = stdin_writer
        .join()
        .map_err(|_| anyhow::anyhow!("node verifier stdin writer panicked"))?;
    let stdout = stdout_reader
        .join()
        .map_err(|_| anyhow::anyhow!("node verifier stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| anyhow::anyhow!("node verifier stderr reader panicked"))??;
    if status.success() {
        write_result.context("failed to write node verifier request")?;
    }
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        bail!("node verifier exited with {}: {}", status, stderr.trim());
    }
    let evidence: NodeEvidence =
        serde_json::from_slice(&stdout).context("node verifier returned invalid evidence")?;
    evaluate_evidence(
        evidence,
        config.min_verifiers.unwrap_or(1),
        config.require_hidden_regression.unwrap_or(true),
    )
}

pub fn evaluate_evidence(
    evidence: NodeEvidence,
    min_verifiers: u32,
    require_hidden_regression: bool,
) -> Result<VerificationOutcome> {
    validate_evidence_hash(&evidence.public_check.evidence_hash)?;
    if let Some(hidden) = &evidence.hidden_regression {
        validate_evidence_hash(&hidden.evidence_hash)?;
    }
    for verdict in &evidence.verifier_verdicts {
        validate_evidence_hash(&verdict.evidence_hash)?;
        if verdict.verifier_id.trim().is_empty() {
            bail!("verifier verdict is missing verifierId");
        }
        if verdict.confidence_bp > 10_000 {
            bail!("verifier confidenceBp must be between 0 and 10000");
        }
    }

    let mut missing = Vec::new();
    if !evidence.public_check.passed {
        missing.push("passing public check".to_string());
    }
    match &evidence.hidden_regression {
        Some(hidden) if !hidden.passed => {
            missing.push("passing hidden regression".to_string());
        }
        None if require_hidden_regression => {
            missing.push("hidden regression evidence".to_string());
        }
        _ => {}
    }
    let passing_verifiers = evidence
        .verifier_verdicts
        .iter()
        .filter(|verdict| verdict.verdict.eq_ignore_ascii_case("pass"))
        .map(|verdict| verdict.verifier_id.trim())
        .collect::<std::collections::BTreeSet<_>>()
        .len() as u32;
    if passing_verifiers < min_verifiers {
        missing.push(format!(
            "{min_verifiers} independent passing verifier verdict(s)"
        ));
    }

    let has_rejection = evidence
        .verifier_verdicts
        .iter()
        .any(|verdict| verdict.verdict.eq_ignore_ascii_case("fail"));
    // An explicit `fail` verdict must block completion even if the passing floor
    // is otherwise met — otherwise a single passing verifier would fail open
    // over a concurrent rejection.
    if has_rejection {
        missing.push("no rejecting verifier verdicts".to_string());
    }
    let decision = if missing.is_empty() {
        VerificationDecision::Complete
    } else if has_rejection {
        VerificationDecision::Escalate
    } else {
        VerificationDecision::Retry
    };
    let summary = match decision {
        VerificationDecision::Complete => {
            "DataEvol public/hidden checks and fractal-verify evidence floors passed".to_string()
        }
        VerificationDecision::Retry => format!("verification incomplete: {}", missing.join(", ")),
        VerificationDecision::Escalate => {
            format!("verifier rejected node evidence: {}", missing.join(", "))
        }
    };
    let encoded = serde_json::to_vec(&evidence)?;
    let evidence_hash = format!("sha256:{:x}", Sha256::digest(encoded));
    Ok(VerificationOutcome {
        decision,
        summary,
        evidence_hash,
        evidence,
        missing,
    })
}

fn validate_evidence_hash(value: &str) -> Result<()> {
    let digest = value
        .strip_prefix("sha256:")
        .context("evidence hash must use sha256:<hex> format")?;
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("evidence hash must contain a 64-character SHA-256 digest");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(character: char) -> String {
        format!("sha256:{}", character.to_string().repeat(64))
    }

    #[test]
    fn completes_only_when_public_hidden_and_verifier_floor_pass() {
        let outcome = evaluate_evidence(
            NodeEvidence {
                public_check: CheckEvidence {
                    passed: true,
                    evidence_hash: hash('a'),
                    detail: "public suite passed".to_string(),
                },
                hidden_regression: Some(CheckEvidence {
                    passed: true,
                    evidence_hash: hash('b'),
                    detail: "hidden suite passed".to_string(),
                }),
                verifier_verdicts: vec![VerifierVerdict {
                    verifier_id: "fractal-verify-1".to_string(),
                    verdict: "pass".to_string(),
                    confidence_bp: 9_500,
                    evidence_hash: hash('c'),
                }],
            },
            1,
            true,
        )
        .unwrap();

        assert_eq!(outcome.decision, VerificationDecision::Complete);
        assert!(outcome.evidence_hash.starts_with("sha256:"));
        assert!(outcome.missing.is_empty());
    }

    /// The acceptance verifier adapter (`fractalmaster/verifiers/acceptance_verifier.py`)
    /// emits camelCase `NodeEvidence`. Confirm both its pass and fail shapes are
    /// runtime-compatible: a pass evaluates to `Complete`, a fail (verdict
    /// "fail") to `Escalate`.
    #[test]
    fn acceptance_adapter_evidence_shape_is_runtime_compatible() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let pass = serde_json::json!({
            "publicCheck": {"passed": true, "evidenceHash": digest, "detail": "acceptance passed"},
            "hiddenRegression": {"passed": true, "evidenceHash": digest, "detail": "regression floor"},
            "verifierVerdicts": [
                {"verifierId": "acceptance", "verdict": "pass", "confidenceBp": 10000, "evidenceHash": digest}
            ]
        });
        let evidence: NodeEvidence =
            serde_json::from_value(pass).expect("adapter pass evidence deserializes");
        assert_eq!(
            evaluate_evidence(evidence, 1, true).unwrap().decision,
            VerificationDecision::Complete
        );

        let fail = serde_json::json!({
            "publicCheck": {"passed": false, "evidenceHash": digest, "detail": "acceptance failed"},
            "hiddenRegression": {"passed": false, "evidenceHash": digest, "detail": "regression floor"},
            "verifierVerdicts": [
                {"verifierId": "acceptance", "verdict": "fail", "confidenceBp": 0, "evidenceHash": digest}
            ]
        });
        let evidence: NodeEvidence =
            serde_json::from_value(fail).expect("adapter fail evidence deserializes");
        assert_eq!(
            evaluate_evidence(evidence, 1, true).unwrap().decision,
            VerificationDecision::Escalate
        );
    }

    #[test]
    fn retries_missing_hidden_evidence_and_escalates_explicit_rejection() {
        let retry = evaluate_evidence(
            NodeEvidence {
                public_check: CheckEvidence {
                    passed: true,
                    evidence_hash: hash('a'),
                    detail: String::new(),
                },
                hidden_regression: None,
                verifier_verdicts: vec![],
            },
            1,
            true,
        )
        .unwrap();
        assert_eq!(retry.decision, VerificationDecision::Retry);

        let escalated = evaluate_evidence(
            NodeEvidence {
                public_check: CheckEvidence {
                    passed: false,
                    evidence_hash: hash('a'),
                    detail: String::new(),
                },
                hidden_regression: Some(CheckEvidence {
                    passed: true,
                    evidence_hash: hash('b'),
                    detail: String::new(),
                }),
                verifier_verdicts: vec![VerifierVerdict {
                    verifier_id: "reviewer".to_string(),
                    verdict: "fail".to_string(),
                    confidence_bp: 9_000,
                    evidence_hash: hash('c'),
                }],
            },
            1,
            true,
        )
        .unwrap();
        assert_eq!(escalated.decision, VerificationDecision::Escalate);
    }

    #[test]
    fn explicit_rejection_blocks_completion_even_when_passing_floor_is_met() {
        // A single `fail` verdict must not be overridden by an otherwise-met
        // passing floor (fail-open regression).
        let outcome = evaluate_evidence(
            NodeEvidence {
                public_check: CheckEvidence {
                    passed: true,
                    evidence_hash: hash('a'),
                    detail: String::new(),
                },
                hidden_regression: Some(CheckEvidence {
                    passed: true,
                    evidence_hash: hash('b'),
                    detail: String::new(),
                }),
                verifier_verdicts: vec![
                    VerifierVerdict {
                        verifier_id: "approver".to_string(),
                        verdict: "pass".to_string(),
                        confidence_bp: 9_000,
                        evidence_hash: hash('c'),
                    },
                    VerifierVerdict {
                        verifier_id: "rejecter".to_string(),
                        verdict: "fail".to_string(),
                        confidence_bp: 0,
                        evidence_hash: hash('d'),
                    },
                ],
            },
            1,
            true,
        )
        .unwrap();
        assert_eq!(outcome.decision, VerificationDecision::Escalate);
        assert!(!outcome.missing.is_empty());
    }

    #[test]
    fn executes_configured_dataevol_verifier_with_json_stdin() {
        let evidence = serde_json::json!({
            "publicCheck": {
                "passed": true,
                "evidenceHash": hash('a'),
                "detail": "DataEvol public check passed"
            },
            "hiddenRegression": {
                "passed": true,
                "evidenceHash": hash('b'),
                "detail": "DataEvol hidden verifier passed"
            },
            "verifierVerdicts": [{
                "verifierId": "fractal-verify-1",
                "verdict": "pass",
                "confidenceBp": 9500,
                "evidenceHash": hash('c')
            }]
        })
        .to_string();
        let config = NodeVerifierConfig {
            program: "sh".to_string(),
            args: vec![
                "-c".to_string(),
                format!("cat >/dev/null; printf %s {}", shell_literal(&evidence)),
            ],
            timeout_secs: Some(2),
            min_verifiers: Some(1),
            require_hidden_regression: Some(true),
        };
        let outcome = run_node_verifier(
            &config,
            &NodeVerificationRequest {
                task_id: "task-1",
                node_id: "implement",
                acceptance_criteria: &["tests pass".to_string()],
                report: &serde_json::json!({"summary": "done"}),
            },
        )
        .unwrap();

        assert_eq!(outcome.decision, VerificationDecision::Complete);
    }

    fn shell_literal(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}
