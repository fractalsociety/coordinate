use anyhow::{bail, Context, Result};
use reqwest::blocking::Client;
use reqwest::blocking::ClientBuilder;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::node_verifier::{
    run_node_verifier, NodeVerificationRequest, NodeVerifierConfig, VerificationDecision,
    VerificationOutcome,
};

const REPORT_MARKER: &str = "COORDINATE_REPORT_JSON:";
const ACK_MARKER: &str = "COORDINATE_ACK:";
const FAILED_MARKER: &str = "COORDINATE_FAILED:";
const BLOCKED_MARKER: &str = "COORDINATE_BLOCKED:";

#[derive(Debug, Clone)]
pub struct HostBridgeOptions {
    pub service_url: String,
    pub execution_graph_url: Option<String>,
    pub node_verifier: Option<NodeVerifierConfig>,
    pub auth_token: Option<String>,
    pub worker_id: String,
    pub kind: String,
    pub role: String,
    pub tmux_target: String,
    pub command: String,
    pub create_session: bool,
    pub readiness_timeout_secs: u64,
    pub readiness_poll_secs: u64,
    pub interval_secs: u64,
    pub once: bool,
    pub auto_assign: bool,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct HostBridgeConfig {
    pub service_url: Option<String>,
    pub execution_graph_url: Option<String>,
    pub auth_token: Option<String>,
    pub tmux_session_prefix: Option<String>,
    pub startup_timeout_secs: Option<u64>,
    pub readiness_poll_secs: Option<u64>,
    pub interval_secs: Option<u64>,
    pub log_dir: Option<String>,
    pub node_verifier: Option<NodeVerifierConfig>,
    #[serde(default)]
    pub workers: Vec<HostBridgeWorkerConfig>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct HostBridgeWorkerConfig {
    pub id: String,
    pub kind: String,
    pub role: Option<String>,
    pub tmux_target: Option<String>,
    pub command: Option<String>,
    pub create_session: Option<bool>,
    pub readiness_timeout_secs: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkerRegisterRequest {
    id: String,
    kind: String,
    role: String,
    status: String,
    capacity: i64,
    metadata: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HeartbeatRequest {
    status: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClaimNextTaskRequest {
    worker_id: String,
    lease_secs: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TouchTaskRequest {
    worker_id: String,
    lease_secs: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReapQueueRequest {
    stale_after_secs: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FailTaskRequest {
    reason: String,
    blocked: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReleaseTaskClaimRequest {
    worker_id: String,
    reason: String,
}

#[derive(Debug, Serialize)]
struct GraphCheckoutRequest {
    agent_id: String,
    agent_label: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RetryTaskRequest {
    reason: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct WorkerRecord {
    current_task_id: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct BridgeTaskRecord {
    pub id: String,
    pub title: String,
    pub description: String,
    pub acceptance_criteria: Vec<String>,
    pub state: String,
    pub preferred_model: String,
    pub source_prd_path: String,
    pub source_task_number: Option<String>,
    pub scheduling_pool: String,
    pub estimated_size: String,
    pub claude_suitable: bool,
    pub assigned_worker_id: Option<String>,
    pub lease_owner: Option<String>,
    pub lease_expires_at: Option<String>,
    pub claimed_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BridgeTaskReport {
    #[serde(default)]
    pub task_id: Option<String>,
    pub summary: String,
    pub files_inspected: Vec<String>,
    pub changed_files: Vec<String>,
    pub tests_run: Vec<String>,
    pub verification: String,
    #[serde(deserialize_with = "deserialize_flexible_string")]
    pub risks: String,
    #[serde(deserialize_with = "deserialize_flexible_string")]
    pub raw_report: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PaneMarker {
    Ack(String),
    Report(BridgeTaskReport),
    Failed(String),
    Blocked(String),
}

fn deserialize_flexible_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::String(value) => Ok(value),
        serde_json::Value::Array(items) => Ok(items
            .into_iter()
            .map(|item| match item {
                serde_json::Value::String(value) => value,
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join("; ")),
        serde_json::Value::Null => Ok(String::new()),
        other => Ok(other.to_string()),
    }
}

#[derive(Debug, Default)]
struct BridgeRuntimeState {
    delivered_tasks: BTreeSet<String>,
    completed_markers: BTreeSet<String>,
    board_checked_out_tasks: BTreeSet<String>,
}

const HOST_BRIDGE_LEASE_SECS: i64 = 15 * 60;

pub fn run_host_bridge(options: HostBridgeOptions) -> Result<()> {
    validate_options(&options)?;
    let client = build_client(options.auth_token.as_deref())?;
    let base = options.service_url.trim_end_matches('/').to_string();
    prepare_tmux_worker(&options)?;
    register_worker(&client, &base, &options)?;
    wait_for_worker_ready(&options)?;
    post_empty(
        &client,
        &format!("{base}/workers/{}/ready", options.worker_id),
    )?;
    println!(
        "Coordinate host bridge registered worker {} for tmux target {}",
        options.worker_id, options.tmux_target
    );
    run_startup_queue_maintenance(&client, &base)?;

    let mut runtime = BridgeRuntimeState::default();
    loop {
        bridge_tick(&client, &base, &options, &mut runtime)?;
        if options.once {
            break;
        }
        std::thread::sleep(Duration::from_secs(options.interval_secs));
    }
    Ok(())
}

pub fn run_host_bridge_config(path: &Path, once: bool) -> Result<()> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read host bridge config: {}", path.display()))?;
    let config: HostBridgeConfig = toml::from_str(&raw)
        .with_context(|| format!("failed to parse host bridge config: {}", path.display()))?;
    if config.workers.is_empty() {
        bail!("host bridge config must define at least one [[workers]] entry");
    }
    let options = config.to_options(once)?;
    let base = config
        .service_url
        .as_deref()
        .unwrap_or("http://127.0.0.1:8787")
        .trim_end_matches('/')
        .to_string();
    let client = build_client(config.auth_token.as_deref())?;
    let mut runtimes = BTreeMap::new();
    for option in &options {
        validate_options(option)?;
        prepare_tmux_worker(option)?;
        register_worker(&client, &base, option)?;
        wait_for_worker_ready(option)?;
        post_empty(
            &client,
            &format!("{base}/workers/{}/ready", option.worker_id),
        )?;
        runtimes.insert(option.worker_id.clone(), BridgeRuntimeState::default());
        println!(
            "Coordinate host bridge registered worker {} for tmux target {}",
            option.worker_id, option.tmux_target
        );
    }
    run_startup_queue_maintenance(&client, &base)?;
    loop {
        for option in &options {
            let runtime = runtimes
                .get_mut(&option.worker_id)
                .context("host bridge runtime disappeared")?;
            bridge_tick(&client, &base, option, runtime)?;
        }
        if once {
            break;
        }
        std::thread::sleep(Duration::from_secs(
            options
                .iter()
                .map(|option| option.interval_secs)
                .min()
                .unwrap_or(5),
        ));
    }
    Ok(())
}

pub fn run_simulated_worker() -> Result<()> {
    let stdin = io::stdin();
    let mut current_task_id = None;
    let mut title = None;
    for line in stdin.lock().lines() {
        let line = line.context("failed to read simulated worker stdin")?;
        if let Some(task_id) = line.trim().strip_prefix("Task ID:") {
            current_task_id = Some(task_id.trim().to_string());
        }
        if let Some(task_title) = line.trim().strip_prefix("Title:") {
            title = Some(task_title.trim().to_string());
        }
        if line.trim().is_empty() {
            if let Some(task_id) = current_task_id.take() {
                let title = title.take().unwrap_or_else(|| "simulated task".to_string());
                println!("{ACK_MARKER} {task_id}");
                println!(
                    "{REPORT_MARKER} {}",
                    serde_json::to_string(&BridgeTaskReport {
                        task_id: Some(task_id.clone()),
                        summary: format!("Simulated worker completed {title}"),
                        files_inspected: vec!["simulated-worker".to_string()],
                        changed_files: Vec::new(),
                        tests_run: vec!["simulated host bridge smoke".to_string()],
                        verification: "simulated acceptance path passed".to_string(),
                        risks: "simulated worker does not execute real code".to_string(),
                        raw_report: "deterministic simulated worker report".to_string(),
                    })?
                );
            }
        }
    }
    Ok(())
}

impl HostBridgeConfig {
    pub fn to_options(&self, once: bool) -> Result<Vec<HostBridgeOptions>> {
        let prefix = self
            .tmux_session_prefix
            .clone()
            .unwrap_or_else(|| "coordinate-worker".to_string());
        let service_url = self
            .service_url
            .clone()
            .unwrap_or_else(|| "http://127.0.0.1:8787".to_string());
        let execution_graph_url = self
            .execution_graph_url
            .clone()
            .or_else(|| Some("http://127.0.0.1:8091".to_string()));
        self.workers
            .iter()
            .map(|worker| {
                let kind = worker.kind.trim().to_ascii_lowercase();
                let id = required_config_field("worker id", &worker.id)?;
                let tmux_target = worker
                    .tmux_target
                    .clone()
                    .unwrap_or_else(|| format!("{prefix}-{id}"));
                Ok(HostBridgeOptions {
                    service_url: service_url.clone(),
                    execution_graph_url: execution_graph_url.clone(),
                    node_verifier: self.node_verifier.clone(),
                    auth_token: self.auth_token.clone(),
                    worker_id: id,
                    kind: kind.clone(),
                    role: worker
                        .role
                        .clone()
                        .unwrap_or_else(|| "coding_worker".to_string()),
                    tmux_target,
                    command: worker
                        .command
                        .clone()
                        .unwrap_or_else(|| default_worker_command(&kind)),
                    create_session: worker.create_session.unwrap_or(true),
                    readiness_timeout_secs: worker
                        .readiness_timeout_secs
                        .or(self.startup_timeout_secs)
                        .unwrap_or_else(|| default_readiness_timeout_secs(&kind)),
                    readiness_poll_secs: self.readiness_poll_secs.unwrap_or(2).max(1),
                    interval_secs: self.interval_secs.unwrap_or(5).max(1),
                    once,
                    auto_assign: true,
                })
            })
            .collect()
    }
}

fn bridge_tick(
    client: &Client,
    base: &str,
    options: &HostBridgeOptions,
    runtime: &mut BridgeRuntimeState,
) -> Result<()> {
    let pane_alive = tmux_target_exists(&options.tmux_target)?;
    if !pane_alive {
        post_empty(
            client,
            &format!("{base}/workers/{}/offline", options.worker_id),
        )?;
        bail!("tmux target is not available: {}", options.tmux_target);
    }
    let worker = heartbeat_worker(client, base, &options.worker_id, None)?;

    let current_task_id = match worker.current_task_id {
        Some(task_id) => Some(task_id),
        None if options.auto_assign => {
            claim_next_task(client, base, &options.worker_id)?.map(|task| task.id)
        }
        None => None,
    };

    let Some(task_id) = current_task_id else {
        return Ok(());
    };
    let mut task = get_task(client, base, &task_id)?;
    if is_graph_node_task(&task) && task.lease_owner.as_deref() != Some(options.worker_id.as_str())
    {
        task = claim_next_task(client, base, &options.worker_id)?.with_context(|| {
            format!("graph task {task_id} could not acquire a Coordinate lease")
        })?;
        if task.id != task_id || task.lease_owner.as_deref() != Some(options.worker_id.as_str()) {
            bail!(
                "worker {} acquired unexpected graph task {} while leasing {}",
                options.worker_id,
                task.id,
                task_id
            );
        }
    }
    if is_graph_node_task(&task) && !runtime.board_checked_out_tasks.contains(&task_id) {
        let checkout_result = checkout_graph_node(
            client,
            options.execution_graph_url.as_deref(),
            &task,
            &options.worker_id,
        );
        if let Err(error) = checkout_result {
            release_task_claim(
                client,
                base,
                &options.worker_id,
                &task_id,
                "execution graph checkout failed before execution",
            )?;
            return Err(error.context(format!(
                "released Coordinate lease for graph task {task_id}"
            )));
        }
        runtime.board_checked_out_tasks.insert(task_id.clone());
    }
    if matches!(task.state.as_str(), "acked" | "working")
        && task.lease_owner.as_deref() == Some(options.worker_id.as_str())
    {
        let _ = touch_task(client, base, &options.worker_id, &task_id);
    }
    if matches!(task.state.as_str(), "assigned" | "acked")
        && !runtime.delivered_tasks.contains(&task_id)
    {
        if options.kind == "cursor"
            && task.lease_owner.as_deref() != Some(options.worker_id.as_str())
        {
            bail!(
                "cursor worker {} cannot execute task {} without owning its lease",
                options.worker_id,
                task.id
            );
        }
        inject_task_brief(&options.tmux_target, &task, &options.kind)?;
        runtime.delivered_tasks.insert(task_id.clone());
    }

    let pane = capture_tmux_pane(&options.tmux_target, 2000)?;
    if let Some(marker) = parse_latest_marker(&pane)? {
        let marker_key = marker.dedupe_key(&task_id);
        if runtime.completed_markers.contains(&marker_key) {
            return Ok(());
        }
        match marker {
            PaneMarker::Ack(acked_task_id) => {
                if acked_task_id == task_id && task.state == "assigned" {
                    post_empty(client, &format!("{base}/tasks/{task_id}/ack"))?;
                    post_empty(client, &format!("{base}/tasks/{task_id}/start"))?;
                } else if acked_task_id == task_id && task.state == "acked" {
                    post_empty(client, &format!("{base}/tasks/{task_id}/start"))?;
                }
            }
            PaneMarker::Report(report) => {
                if is_graph_node_task(&task) && report.task_id.as_deref() != Some(task_id.as_str())
                {
                    runtime.completed_markers.insert(marker_key);
                    return Ok(());
                }
                let latest = get_task(client, base, &task_id)?;
                if latest.state == "assigned" {
                    post_empty(client, &format!("{base}/tasks/{task_id}/ack"))?;
                    post_empty(client, &format!("{base}/tasks/{task_id}/start"))?;
                } else if latest.state == "acked" {
                    post_empty(client, &format!("{base}/tasks/{task_id}/start"))?;
                }
                http_json::<BridgeTaskRecord, _>(
                    client,
                    reqwest::Method::POST,
                    &format!("{base}/tasks/{task_id}/report"),
                    &report,
                )?;
                if is_graph_node_task(&latest) {
                    let outcome = verify_graph_node(options, &latest, &report)?;
                    apply_graph_verification(client, base, options, runtime, &latest, &outcome)?;
                } else {
                    http_json::<BridgeTaskRecord, _>(
                        client,
                        reqwest::Method::POST,
                        &format!("{base}/tasks/{task_id}/verify"),
                        &serde_json::json!({"summary": "host bridge report accepted"}),
                    )?;
                    post_empty(client, &format!("{base}/tasks/{task_id}/complete"))?;
                }
            }
            PaneMarker::Failed(reason) => fail_task(client, base, &task_id, false, &reason)?,
            PaneMarker::Blocked(reason) => fail_task(client, base, &task_id, true, &reason)?,
        }
        runtime.completed_markers.insert(marker_key);
    }
    Ok(())
}

impl PaneMarker {
    fn dedupe_key(&self, task_id: &str) -> String {
        match self {
            PaneMarker::Ack(acked_task_id) => format!("ack:{acked_task_id}"),
            PaneMarker::Report(report) => {
                let encoded = serde_json::to_vec(report).unwrap_or_default();
                format!("report:{task_id}:{:x}", Sha256::digest(encoded))
            }
            PaneMarker::Failed(reason) => format!("failed:{task_id}:{reason}"),
            PaneMarker::Blocked(reason) => format!("blocked:{task_id}:{reason}"),
        }
    }
}

pub fn render_task_injection(task: &BridgeTaskRecord) -> String {
    let criteria = task
        .acceptance_criteria
        .iter()
        .map(|item| format!("- {item}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        r#"Coordinate task assigned.

	Task ID: {task_id}
	Title: {title}
	Source PRD: {source_prd_path}
	Source task number: {source_task_number}
		Preferred model: {preferred_model}
		Scheduling pool: {scheduling_pool}
		Estimated size: {estimated_size}
		Claim ID: {claim_id}
		Lease expires at: {lease_expires_at}

Description:
{description}

Acceptance criteria:
{criteria}

		First, acknowledge acceptance by printing exactly:
		COORDINATE_ACK: {task_id}

For long work, keep making progress. The host bridge renews your lease while this pane is active; if the pane dies or stops responding, Coordinate will return the task to the shared queue.

When finished, print exactly one line beginning with COORDINATE_REPORT_JSON: followed by JSON with:
taskId (exactly "{task_id}"), summary, filesInspected, changedFiles, testsRun, verification, risks, rawReport.

	If blocked, print COORDINATE_BLOCKED: <reason>.
	If failed, print COORDINATE_FAILED: <reason>.

	End the prompt with a blank line so automated bridge tests and simulated workers
	can detect prompt completion.
	"#,
        task_id = task.id,
        title = task.title,
        source_prd_path = task.source_prd_path,
        source_task_number = task.source_task_number.as_deref().unwrap_or("unknown"),
        preferred_model = task.preferred_model,
        scheduling_pool = task.scheduling_pool,
        estimated_size = task.estimated_size,
        claim_id = task
            .lease_owner
            .as_deref()
            .map(|owner| format!("{owner}:{}", task.id))
            .unwrap_or_else(|| format!("unleased:{}", task.id)),
        lease_expires_at = task.lease_expires_at.as_deref().unwrap_or("unknown"),
        description = task.description,
        criteria = criteria
    )
}

pub fn parse_latest_marker(output: &str) -> Result<Option<PaneMarker>> {
    if let Some(marker) = parse_latest_text_marker(output)? {
        return Ok(Some(marker));
    }

    for line in output.lines().rev() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        let mut strings = Vec::new();
        collect_json_strings(&value, &mut strings);
        for text in strings.into_iter().rev() {
            if let Some(marker) = parse_latest_text_marker(text)? {
                return Ok(Some(marker));
            }
        }
    }
    Ok(None)
}

fn parse_latest_text_marker(output: &str) -> Result<Option<PaneMarker>> {
    let lines = output.lines().collect::<Vec<_>>();
    for index in (0..lines.len()).rev() {
        let line = lines[index].trim();
        if let Some(raw) = marker_suffix(line, REPORT_MARKER) {
            if !raw.trim_start().starts_with('{') {
                continue;
            }
            if let Ok(report) = parse_wrapped_report(raw, &lines[index + 1..]) {
                return Ok(Some(PaneMarker::Report(report)));
            }
        }
    }

    for line in lines.iter().rev() {
        let line = line.trim();
        if let Some(raw) = marker_suffix(line, ACK_MARKER) {
            return Ok(Some(PaneMarker::Ack(raw.trim().to_string())));
        }
        if let Some(reason) = marker_suffix(line, BLOCKED_MARKER) {
            return Ok(Some(PaneMarker::Blocked(reason.trim().to_string())));
        }
        if let Some(reason) = marker_suffix(line, FAILED_MARKER) {
            return Ok(Some(PaneMarker::Failed(reason.trim().to_string())));
        }
    }
    Ok(None)
}

fn collect_json_strings<'a>(value: &'a serde_json::Value, output: &mut Vec<&'a str>) {
    match value {
        serde_json::Value::String(value) => output.push(value),
        serde_json::Value::Array(values) => {
            for value in values {
                collect_json_strings(value, output);
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values() {
                collect_json_strings(value, output);
            }
        }
        _ => {}
    }
}

fn marker_suffix<'a>(line: &'a str, marker: &str) -> Option<&'a str> {
    let normalized = line
        .trim_start()
        .trim_start_matches(|c| matches!(c, '•' | '⏺' | '↳' | '>' | '›' | '-'))
        .trim_start();
    normalized.strip_prefix(marker)
}

fn parse_wrapped_report(raw: &str, following_lines: &[&str]) -> Result<BridgeTaskReport> {
    let mut compact = raw.trim().to_string();
    let mut spaced = compact.clone();
    if let Ok(report) = serde_json::from_str::<BridgeTaskReport>(&compact) {
        return Ok(report);
    }

    // Interactive UIs wrap long evidence reports across many physical pane
    // lines. Keep the scan bounded, but large enough for realistic reports.
    for line in following_lines.iter().take(200) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if marker_suffix(trimmed, ACK_MARKER).is_some()
            || marker_suffix(trimmed, REPORT_MARKER).is_some()
            || marker_suffix(trimmed, BLOCKED_MARKER).is_some()
            || marker_suffix(trimmed, FAILED_MARKER).is_some()
        {
            break;
        }
        compact.push_str(trimmed);
        spaced.push(' ');
        spaced.push_str(trimmed);

        if let Ok(report) = serde_json::from_str::<BridgeTaskReport>(&spaced) {
            return Ok(report);
        }
        if let Ok(report) = serde_json::from_str::<BridgeTaskReport>(&compact) {
            return Ok(report);
        }
    }

    serde_json::from_str(raw.trim()).context("failed to parse COORDINATE_REPORT_JSON marker")
}

fn register_worker(
    client: &Client,
    base: &str,
    options: &HostBridgeOptions,
) -> Result<WorkerRecord> {
    http_json(
        client,
        reqwest::Method::POST,
        &format!("{base}/workers/register"),
        &WorkerRegisterRequest {
            id: options.worker_id.clone(),
            kind: options.kind.clone(),
            role: options.role.clone(),
            status: "ready".to_string(),
            capacity: 1,
            metadata: serde_json::json!({
                "hostBridge": true,
                "tmuxTarget": options.tmux_target,
                "evidenceFormat": if options.kind == "cursor" { "stream-json" } else { "tmux-pane" },
                "sandbox": if options.kind == "cursor" { "enabled" } else { "provider-default" },
            })
            .to_string(),
        },
    )
}

fn heartbeat_worker(
    client: &Client,
    base: &str,
    worker_id: &str,
    status: Option<&str>,
) -> Result<WorkerRecord> {
    http_json(
        client,
        reqwest::Method::POST,
        &format!("{base}/workers/{worker_id}/heartbeat"),
        &HeartbeatRequest {
            status: status.map(str::to_string),
        },
    )
}

fn claim_next_task(
    client: &Client,
    base: &str,
    worker_id: &str,
) -> Result<Option<BridgeTaskRecord>> {
    http_json(
        client,
        reqwest::Method::POST,
        &format!("{base}/tasks/next"),
        &ClaimNextTaskRequest {
            worker_id: worker_id.to_string(),
            lease_secs: HOST_BRIDGE_LEASE_SECS,
        },
    )
}

fn touch_task(
    client: &Client,
    base: &str,
    worker_id: &str,
    task_id: &str,
) -> Result<BridgeTaskRecord> {
    http_json(
        client,
        reqwest::Method::POST,
        &format!("{base}/tasks/{task_id}/touch"),
        &TouchTaskRequest {
            worker_id: worker_id.to_string(),
            lease_secs: HOST_BRIDGE_LEASE_SECS,
        },
    )
}

fn run_startup_queue_maintenance(client: &Client, base: &str) -> Result<()> {
    let _: serde_json::Value = http_json(
        client,
        reqwest::Method::POST,
        &format!("{base}/tasks/reap"),
        &ReapQueueRequest {
            stale_after_secs: HOST_BRIDGE_LEASE_SECS,
        },
    )?;
    let _: serde_json::Value = http_get_json(client, &format!("{base}/tasks/stats"))?;
    Ok(())
}

fn get_task(client: &Client, base: &str, task_id: &str) -> Result<BridgeTaskRecord> {
    http_get_json(client, &format!("{base}/tasks/{task_id}"))
}

fn fail_task(
    client: &Client,
    base: &str,
    task_id: &str,
    blocked: bool,
    reason: &str,
) -> Result<()> {
    let _: BridgeTaskRecord = http_json(
        client,
        reqwest::Method::POST,
        &format!("{base}/tasks/{task_id}/fail"),
        &FailTaskRequest {
            reason: reason.to_string(),
            blocked,
        },
    )?;
    Ok(())
}

fn release_task_claim(
    client: &Client,
    base: &str,
    worker_id: &str,
    task_id: &str,
    reason: &str,
) -> Result<()> {
    let _: BridgeTaskRecord = http_json(
        client,
        reqwest::Method::POST,
        &format!("{base}/tasks/{task_id}/release-claim"),
        &ReleaseTaskClaimRequest {
            worker_id: worker_id.to_string(),
            reason: reason.to_string(),
        },
    )?;
    Ok(())
}

fn is_graph_node_task(task: &BridgeTaskRecord) -> bool {
    task.source_prd_path.starts_with("fractal-graph:")
}

fn checkout_graph_node(
    client: &Client,
    execution_graph_url: Option<&str>,
    task: &BridgeTaskRecord,
    worker_id: &str,
) -> Result<()> {
    if !is_graph_node_task(task) {
        return Ok(());
    }
    let node_id = task
        .source_task_number
        .as_deref()
        .context("compiled graph task is missing its node id")?;
    let base = execution_graph_url
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .context("compiled graph task requires executionGraphUrl")?;
    let mut url = reqwest::Url::parse(base)
        .with_context(|| format!("invalid execution graph URL: {base}"))?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|()| anyhow::anyhow!("execution graph URL cannot be a base"))?;
        segments.pop_if_empty();
        segments.extend(["api", "tasks", node_id, "checkout"]);
    }
    let response = client
        .post(url.clone())
        .json(&GraphCheckoutRequest {
            agent_id: worker_id.to_string(),
            agent_label: format!("Coordinate · {worker_id}"),
        })
        .send()
        .with_context(|| format!("failed to check out graph node {node_id}"))?;
    ensure_success(response, url.as_str())?;
    Ok(())
}

fn verify_graph_node(
    options: &HostBridgeOptions,
    task: &BridgeTaskRecord,
    report: &BridgeTaskReport,
) -> Result<VerificationOutcome> {
    let config = options
        .node_verifier
        .as_ref()
        .context("compiled graph task requires nodeVerifier configuration")?;
    let node_id = task
        .source_task_number
        .as_deref()
        .context("compiled graph task is missing its node id")?;
    run_node_verifier(
        config,
        &NodeVerificationRequest {
            task_id: &task.id,
            node_id,
            acceptance_criteria: &task.acceptance_criteria,
            report,
        },
    )
}

fn apply_graph_verification(
    client: &Client,
    base: &str,
    options: &HostBridgeOptions,
    runtime: &mut BridgeRuntimeState,
    task: &BridgeTaskRecord,
    outcome: &VerificationOutcome,
) -> Result<()> {
    match outcome.decision {
        VerificationDecision::Complete => {
            http_json::<BridgeTaskRecord, _>(
                client,
                reqwest::Method::POST,
                &format!("{base}/tasks/{}/verify", task.id),
                &serde_json::json!({
                    "summary": outcome.summary,
                    "decision": "complete",
                    "evidenceHash": outcome.evidence_hash,
                    "evidence": outcome.evidence,
                }),
            )?;
            update_graph_node(
                client,
                options.execution_graph_url.as_deref(),
                task,
                &options.worker_id,
                "complete",
            )?;
            post_empty(client, &format!("{base}/tasks/{}/complete", task.id))?;
        }
        VerificationDecision::Retry => {
            update_graph_node(
                client,
                options.execution_graph_url.as_deref(),
                task,
                &options.worker_id,
                "release",
            )?;
            runtime.board_checked_out_tasks.remove(&task.id);
            let rejected: BridgeTaskRecord = http_json(
                client,
                reqwest::Method::POST,
                &format!("{base}/tasks/{}/reject", task.id),
                &RetryTaskRequest {
                    reason: outcome.summary.clone(),
                },
            )?;
            if rejected.state == "queued" {
                runtime.delivered_tasks.remove(&task.id);
            }
        }
        VerificationDecision::Escalate => {
            update_graph_node(
                client,
                options.execution_graph_url.as_deref(),
                task,
                &options.worker_id,
                "release",
            )?;
            runtime.board_checked_out_tasks.remove(&task.id);
            fail_task(client, base, &task.id, true, &outcome.summary)?;
        }
    }
    Ok(())
}

fn update_graph_node(
    client: &Client,
    execution_graph_url: Option<&str>,
    task: &BridgeTaskRecord,
    worker_id: &str,
    action: &str,
) -> Result<()> {
    if !matches!(action, "complete" | "release") {
        bail!("unsupported execution graph action: {action}");
    }
    let node_id = task
        .source_task_number
        .as_deref()
        .context("compiled graph task is missing its node id")?;
    let base = execution_graph_url
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .context("compiled graph task requires executionGraphUrl")?;
    let mut url = reqwest::Url::parse(base)
        .with_context(|| format!("invalid execution graph URL: {base}"))?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|()| anyhow::anyhow!("execution graph URL cannot be a base"))?;
        segments.pop_if_empty();
        segments.extend(["api", "tasks", node_id, action]);
    }
    let response = client
        .post(url.clone())
        .json(&GraphCheckoutRequest {
            agent_id: worker_id.to_string(),
            agent_label: format!("Coordinate · {worker_id}"),
        })
        .send()
        .with_context(|| format!("failed to {action} graph node {node_id}"))?;
    ensure_success(response, url.as_str())?;
    Ok(())
}

fn post_empty(client: &Client, url: &str) -> Result<()> {
    let response = client
        .post(url)
        .json(&serde_json::json!({}))
        .send()
        .with_context(|| format!("failed to POST {url}"))?;
    ensure_success(response, url)?;
    Ok(())
}

fn http_get_json<T: for<'de> Deserialize<'de>>(client: &Client, url: &str) -> Result<T> {
    let response = client
        .get(url)
        .send()
        .with_context(|| format!("failed to GET {url}"))?;
    let response = ensure_success(response, url)?;
    response
        .json()
        .with_context(|| format!("failed to decode JSON response from {url}"))
}

fn http_json<T: for<'de> Deserialize<'de>, B: Serialize>(
    client: &Client,
    method: reqwest::Method,
    url: &str,
    body: &B,
) -> Result<T> {
    let response = client
        .request(method, url)
        .json(body)
        .send()
        .with_context(|| format!("failed to send request to {url}"))?;
    let response = ensure_success(response, url)?;
    response
        .json()
        .with_context(|| format!("failed to decode JSON response from {url}"))
}

fn ensure_success(
    response: reqwest::blocking::Response,
    url: &str,
) -> Result<reqwest::blocking::Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = response.text().unwrap_or_default();
    bail!("Coordinate service request failed {status} for {url}: {body}");
}

fn build_client(auth_token: Option<&str>) -> Result<Client> {
    let mut builder = ClientBuilder::new().timeout(Duration::from_secs(10));
    if let Some(token) = auth_token.map(str::trim).filter(|token| !token.is_empty()) {
        let mut headers = HeaderMap::new();
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .context("invalid host bridge auth token for Authorization header")?;
        headers.insert(AUTHORIZATION, value);
        builder = builder.default_headers(headers);
    }
    builder
        .build()
        .context("failed to build Coordinate host bridge HTTP client")
}

fn prepare_tmux_worker(options: &HostBridgeOptions) -> Result<()> {
    if tmux_target_exists(&options.tmux_target)? {
        return Ok(());
    }
    if !options.create_session {
        bail!("tmux target does not exist: {}", options.tmux_target);
    }
    spawn_tmux_session(&options.tmux_target, &options.command)
}

fn wait_for_worker_ready(options: &HostBridgeOptions) -> Result<()> {
    let deadline = std::time::Instant::now()
        .checked_add(Duration::from_secs(options.readiness_timeout_secs))
        .context("invalid readiness timeout")?;
    loop {
        if tmux_target_exists(&options.tmux_target)?
            && capture_tmux_pane(&options.tmux_target, 20).is_ok()
        {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            bail!(
                "tmux target {} did not become ready within {}s",
                options.tmux_target,
                options.readiness_timeout_secs
            );
        }
        std::thread::sleep(Duration::from_secs(options.readiness_poll_secs));
    }
}

fn spawn_tmux_session(target: &str, command: &str) -> Result<()> {
    let session = tmux_session_from_target(target)?;
    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, command])
        .status()
        .with_context(|| format!("failed to spawn tmux session {session}"))?;
    if !status.success() {
        bail!("tmux new-session failed for {session}");
    }
    Ok(())
}

fn tmux_session_from_target(target: &str) -> Result<String> {
    let session = target
        .split_once(':')
        .map(|(session, _)| session)
        .unwrap_or(target)
        .trim();
    if session.is_empty() {
        bail!("tmux target must include a non-empty session name");
    }
    Ok(session.to_string())
}

fn default_worker_command(kind: &str) -> String {
    match kind {
        "claude" => "claude --dangerously-skip-permissions".to_string(),
        "codex" => "codex --dangerously-bypass-approvals-and-sandbox --no-alt-screen".to_string(),
        "cursor" => "$SHELL".to_string(),
        "openrouter" => "squad openrouter-worker".to_string(),
        "local" => "squad local-worker".to_string(),
        "manual" => "$SHELL".to_string(),
        _ => kind.to_string(),
    }
}

fn default_readiness_timeout_secs(kind: &str) -> u64 {
    match kind {
        "codex" => 90,
        "claude" => 45,
        _ => 30,
    }
}

fn required_config_field(field: &str, value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        bail!("host bridge {field} cannot be empty");
    }
    Ok(value.to_string())
}

fn tmux_target_exists(target: &str) -> Result<bool> {
    let status = Command::new("tmux")
        .args(["display-message", "-p", "-t", target, "#{pane_id}"])
        .status()
        .with_context(|| "failed to run tmux; is tmux installed?")?;
    Ok(status.success())
}

fn capture_tmux_pane(target: &str, lines: usize) -> Result<String> {
    let output = Command::new("tmux")
        .args([
            "capture-pane",
            "-p",
            "-J",
            "-S",
            &format!("-{lines}"),
            "-t",
            target,
        ])
        .output()
        .with_context(|| "failed to run tmux capture-pane")?;
    if !output.status.success() {
        bail!("tmux capture-pane failed for target {target}");
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn inject_task_brief(target: &str, task: &BridgeTaskRecord, kind: &str) -> Result<()> {
    let prompt = render_task_injection(task);
    let text = if kind == "cursor" {
        cursor_worker_command(&prompt)
    } else {
        prompt
    };
    let mut load = Command::new("tmux")
        .args(["load-buffer", "-"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .context("failed to run tmux load-buffer")?;
    {
        use std::io::Write;
        let stdin = load
            .stdin
            .as_mut()
            .context("tmux load-buffer stdin unavailable")?;
        stdin.write_all(text.as_bytes())?;
    }
    let status = load.wait()?;
    if !status.success() {
        bail!("tmux load-buffer failed");
    }
    let paste = Command::new("tmux")
        .args(["paste-buffer", "-t", target])
        .status()
        .context("failed to run tmux paste-buffer")?;
    if !paste.success() {
        bail!("tmux paste-buffer failed for target {target}");
    }
    let enter = Command::new("tmux")
        .args(["send-keys", "-t", target, "Enter"])
        .status()
        .context("failed to run tmux send-keys")?;
    if !enter.success() {
        bail!("tmux send-keys failed for target {target}");
    }
    if kind == "codex" || kind == "claude" {
        std::thread::sleep(Duration::from_millis(500));
        let extra_enter = Command::new("tmux")
            .args(["send-keys", "-t", target, "Enter"])
            .status()
            .context("failed to run tmux send-keys")?;
        if !extra_enter.success() {
            bail!("tmux extra send-keys failed for target {target}");
        }
    }
    let _ = Command::new("tmux").arg("delete-buffer").status();
    Ok(())
}

/// Render the Cursor worker invocation for a leased node execution.
///
/// Cursor is an *interchangeable worker*: this renders the headless,
/// sandboxed execute-only invocation (`cursor-agent -p …`) used by the real
/// injection path, and deliberately carries no planning/intent mode — Cursor
/// executes the leased node and never drives the pipeline. Exposed so the P3.4
/// interchangeability proof can assert the execution-only contract offline.
pub fn cursor_worker_command(prompt: &str) -> String {
    // This command is pasted into a plain interactive shell. Keep it on one
    // physical line so embedded prompt newlines cannot enter the shell's quote
    // continuation mode before the closing quote arrives.
    let prompt = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    format!(
        "cursor-agent -p --output-format stream-json --sandbox enabled --trust {}",
        shell_quote(&prompt)
    )
}

fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn validate_options(options: &HostBridgeOptions) -> Result<()> {
    if options.service_url.trim().is_empty() {
        bail!("--service-url cannot be empty");
    }
    if options.worker_id.trim().is_empty() {
        bail!("--worker-id cannot be empty");
    }
    if options.kind.trim().is_empty() {
        bail!("--kind cannot be empty");
    }
    if options.role.trim().is_empty() {
        bail!("--role cannot be empty");
    }
    if options.tmux_target.trim().is_empty() {
        bail!("--tmux-target cannot be empty");
    }
    if options.command.trim().is_empty() {
        bail!("--command cannot be empty");
    }
    if options.readiness_timeout_secs == 0 {
        bail!("--readiness-timeout-secs must be positive");
    }
    if options.readiness_poll_secs == 0 {
        bail!("--readiness-poll-secs must be positive");
    }
    if options.interval_secs == 0 {
        bail!("--interval-secs must be positive");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn parses_latest_report_marker() {
        let output = r#"
older text
COORDINATE_REPORT_JSON: {"summary":"done","filesInspected":["src/main.rs"],"changedFiles":["src/main.rs"],"testsRun":["cargo test"],"verification":"passed","risks":"none","rawReport":"ok"}
"#;
        let marker = parse_latest_marker(output).unwrap().unwrap();
        assert_eq!(
            marker,
            PaneMarker::Report(BridgeTaskReport {
                task_id: None,
                summary: "done".to_string(),
                files_inspected: vec!["src/main.rs".to_string()],
                changed_files: vec!["src/main.rs".to_string()],
                tests_run: vec!["cargo test".to_string()],
                verification: "passed".to_string(),
                risks: "none".to_string(),
                raw_report: "ok".to_string(),
            })
        );
    }

    #[test]
    fn parses_task_bound_report_and_skips_newer_malformed_candidate() {
        let output = r#"
COORDINATE_REPORT_JSON: {"taskId":"graph-node:one","summary":"done","filesInspected":[],"changedFiles":[],"testsRun":["test"],"verification":"passed","risks":"none","rawReport":"ok"}
COORDINATE_REPORT_JSON: {"summary":"unterminated
"#;
        let marker = parse_latest_marker(output).unwrap().unwrap();
        let PaneMarker::Report(report) = marker else {
            panic!("expected report marker");
        };
        assert_eq!(report.task_id.as_deref(), Some("graph-node:one"));
    }

    #[test]
    fn parses_wrapped_report_marker_from_terminal_pane() {
        let output = r#"
• COORDINATE_ACK: task-1

  COORDINATE_REPORT_JSON: {"summary":"Acknowledged task as requested; no files
  inspected or changed.","filesInspected":[],"changedFiles":[],"testsRun":
  [],"verification":"Printed required markers.","risks":[],"rawReport":"ok"}
"#;
        let marker = parse_latest_marker(output).unwrap().unwrap();
        assert_eq!(
            marker,
            PaneMarker::Report(BridgeTaskReport {
                task_id: None,
                summary: "Acknowledged task as requested; no files inspected or changed."
                    .to_string(),
                files_inspected: vec![],
                changed_files: vec![],
                tests_run: vec![],
                verification: "Printed required markers.".to_string(),
                risks: String::new(),
                raw_report: "ok".to_string(),
            })
        );
    }

    #[test]
    fn parses_claude_prefixed_report_marker() {
        let output = r#"
⏺ COORDINATE_ACK: task-1
⏺ COORDINATE_REPORT_JSON: {"summary":"done","filesInspected":[],"changedFiles":[],"testsRun":["noop"],"verification":"passed","risks":[],"rawReport":"ok"}
"#;
        let marker = parse_latest_marker(output).unwrap().unwrap();
        assert_eq!(
            marker,
            PaneMarker::Report(BridgeTaskReport {
                task_id: None,
                summary: "done".to_string(),
                files_inspected: vec![],
                changed_files: vec![],
                tests_run: vec!["noop".to_string()],
                verification: "passed".to_string(),
                risks: String::new(),
                raw_report: "ok".to_string(),
            })
        );
    }

    #[test]
    fn parses_cursor_stream_json_report_marker() {
        let output = serde_json::json!({
            "type": "result",
            "result": "Work complete.\nCOORDINATE_REPORT_JSON: {\"summary\":\"done\",\"filesInspected\":[\"src/main.rs\"],\"changedFiles\":[\"src/main.rs\"],\"testsRun\":[\"cargo test\"],\"verification\":\"passed\",\"risks\":\"none\",\"rawReport\":\"cursor stream evidence\"}"
        })
        .to_string();

        let marker = parse_latest_marker(&output).unwrap().unwrap();

        assert_eq!(
            marker,
            PaneMarker::Report(BridgeTaskReport {
                task_id: None,
                summary: "done".to_string(),
                files_inspected: vec!["src/main.rs".to_string()],
                changed_files: vec!["src/main.rs".to_string()],
                tests_run: vec!["cargo test".to_string()],
                verification: "passed".to_string(),
                risks: "none".to_string(),
                raw_report: "cursor stream evidence".to_string(),
            })
        );
    }

    #[test]
    fn latest_marker_wins_and_supports_blocked_failed() {
        assert_eq!(
            parse_latest_marker("COORDINATE_ACK: task-1").unwrap(),
            Some(PaneMarker::Ack("task-1".to_string()))
        );
        assert_eq!(
            parse_latest_marker("COORDINATE_FAILED: bad\nCOORDINATE_BLOCKED: waiting").unwrap(),
            Some(PaneMarker::Blocked("waiting".to_string()))
        );
        assert_eq!(
            parse_latest_marker("COORDINATE_FAILED: tests failed").unwrap(),
            Some(PaneMarker::Failed("tests failed".to_string()))
        );
    }

    #[test]
    fn ignores_report_marker_in_instruction_text() {
        let output = r#"
When finished, print exactly one line beginning with COORDINATE_REPORT_JSON:
followed by JSON with summary and verification.
If failed, print COORDINATE_FAILED: <reason>.
"#;
        assert_eq!(parse_latest_marker(output).unwrap(), None);
    }

    #[test]
    fn renders_task_injection_with_completion_contract() {
        let rendered = render_task_injection(&BridgeTaskRecord {
            id: "task-1".to_string(),
            title: "Build bridge".to_string(),
            description: "Wire tmux worker".to_string(),
            acceptance_criteria: vec!["reports via marker".to_string()],
            state: "assigned".to_string(),
            preferred_model: "codex".to_string(),
            source_prd_path: "PRD.md".to_string(),
            source_task_number: Some("25".to_string()),
            scheduling_pool: "codex".to_string(),
            estimated_size: "small".to_string(),
            claude_suitable: false,
            assigned_worker_id: Some("codex-1".to_string()),
            lease_owner: Some("codex-1".to_string()),
            lease_expires_at: Some("2026-07-03T00:15:00Z".to_string()),
            claimed_at: Some("2026-07-03T00:00:00Z".to_string()),
        });
        assert!(rendered.contains("Task ID: task-1"));
        assert!(rendered.contains("Source PRD: PRD.md"));
        assert!(rendered.contains("Scheduling pool: codex"));
        assert!(rendered.contains("Claim ID: codex-1:task-1"));
        assert!(rendered.contains("Lease expires at: 2026-07-03T00:15:00Z"));
        assert!(rendered.contains("COORDINATE_REPORT_JSON"));
        assert!(rendered.contains("COORDINATE_BLOCKED"));
    }

    #[test]
    fn config_expands_provider_workers_with_defaults() {
        let config: HostBridgeConfig = toml::from_str(
            r#"
serviceUrl = "http://127.0.0.1:8787"
tmuxSessionPrefix = "bridge"
startupTimeoutSecs = 12

[nodeVerifier]
program = "dataevol-node-verifier"
minVerifiers = 2
requireHiddenRegression = true

[[workers]]
id = "codex-1"
kind = "codex"

[[workers]]
id = "claude-1"
kind = "claude"
role = "coding_worker"

[[workers]]
id = "cursor-1"
kind = "cursor"
"#,
        )
        .unwrap();
        let options = config.to_options(true).unwrap();
        assert_eq!(options.len(), 3);
        assert_eq!(options[0].tmux_target, "bridge-codex-1");
        assert_eq!(
            options[0].command,
            "codex --dangerously-bypass-approvals-and-sandbox --no-alt-screen"
        );
        assert_eq!(options[0].readiness_timeout_secs, 12);
        assert_eq!(options[1].command, "claude --dangerously-skip-permissions");
        assert_eq!(options[2].command, "$SHELL");
        assert_eq!(
            options[0].execution_graph_url.as_deref(),
            Some("http://127.0.0.1:8091")
        );
        let verifier = options[0].node_verifier.as_ref().unwrap();
        assert_eq!(verifier.program, "dataevol-node-verifier");
        assert_eq!(verifier.min_verifiers, Some(2));
    }

    #[test]
    fn graph_node_checkout_posts_worker_ownership_before_execution() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let count = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..count]);
            assert!(request.starts_with("POST /api/tasks/implement/checkout HTTP/1.1"));
            assert!(request.contains(r#""agent_id":"cursor-1""#));
            assert!(request.contains(r#""agent_label":"Coordinate · cursor-1""#));
            let body = r#"{"ok":true}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let client = build_client(None).unwrap();
        let task = BridgeTaskRecord {
            id: "graph-task-1".to_string(),
            title: "Implement".to_string(),
            description: "Execute node".to_string(),
            acceptance_criteria: vec!["done".to_string()],
            state: "acked".to_string(),
            preferred_model: "cursor".to_string(),
            source_prd_path: "fractal-graph:graph:test:sha256:abc".to_string(),
            source_task_number: Some("implement".to_string()),
            scheduling_pool: "cursor".to_string(),
            estimated_size: "small".to_string(),
            claude_suitable: false,
            assigned_worker_id: Some("cursor-1".to_string()),
            lease_owner: Some("cursor-1".to_string()),
            lease_expires_at: Some("2026-07-23T15:00:00Z".to_string()),
            claimed_at: Some("2026-07-23T14:45:00Z".to_string()),
        };

        checkout_graph_node(
            &client,
            Some(&format!("http://{address}")),
            &task,
            "cursor-1",
        )
        .unwrap();
        server.join().unwrap();
    }

    #[test]
    fn graph_node_checkout_fails_closed_without_board_url() {
        let client = build_client(None).unwrap();
        let task = BridgeTaskRecord {
            id: "graph-task-1".to_string(),
            title: "Implement".to_string(),
            description: "Execute node".to_string(),
            acceptance_criteria: Vec::new(),
            state: "acked".to_string(),
            preferred_model: "codex".to_string(),
            source_prd_path: "fractal-graph:graph:test:sha256:abc".to_string(),
            source_task_number: Some("implement".to_string()),
            scheduling_pool: "codex".to_string(),
            estimated_size: "small".to_string(),
            claude_suitable: false,
            assigned_worker_id: Some("codex-1".to_string()),
            lease_owner: Some("codex-1".to_string()),
            lease_expires_at: None,
            claimed_at: None,
        };

        assert!(checkout_graph_node(&client, None, &task, "codex-1")
            .unwrap_err()
            .to_string()
            .contains("executionGraphUrl"));
    }

    #[test]
    fn cursor_worker_command_is_sandboxed_stream_json_and_shell_quoted() {
        let command = cursor_worker_command("Task lease: cursor-1:task-1\nDon't escape");

        assert!(command
            .starts_with("cursor-agent -p --output-format stream-json --sandbox enabled --trust "));
        assert!(command.contains("Task lease: cursor-1:task-1"));
        assert!(command.contains(r#"Don'\''t escape"#));
        assert!(!command.contains('\n'));
    }

    #[test]
    fn tmux_session_is_derived_from_target() {
        assert_eq!(
            tmux_session_from_target("bridge-codex-1").unwrap(),
            "bridge-codex-1"
        );
        assert_eq!(tmux_session_from_target("bridge:0.1").unwrap(), "bridge");
        assert!(tmux_session_from_target(":0.1").is_err());
    }
}
