//! Tests for the Science Swarm routing, cost model, local helpers, reliability,
//! trace capture, and planner-template functions in `squad::autopilot`.
//!
//! Covers autopilot tasks: 6, 14, 25, 26, 27, 28, 35, 37, 38, 67, 74, 81,
//! 82, 83, 84, 91, 93, 101, 102, 124, 125, 126. (Files-changed capture,
//! task 99, is covered by the existing autopilot test-run tracking tests.)

use squad::autopilot::RiskLevel;
use squad::autopilot::{
    all_provider_adapter_overviews, available_providers, background_evidence_search_task_template,
    check_provider_availability, classify_task_difficulty_local, claude_task_eligibility,
    codex_backfill_plan, continuous_integrity_check, cost_latency_path, cost_latency_report_lines,
    curate_memory, dataset_discovery_task_template, detect_duplicate_work, enforce_caps,
    estimate_cost_and_rate_limit, estimate_task_difficulty, extract_acceptance_criteria,
    independent_verification_task_template, next_retry_delay_seconds, plan_failed_task_requeue,
    provider_adapter_overview, provider_adapters_report_path, provider_tier, read_cost_latency,
    read_verification_results, recommend_provider_tier, record_cost_latency,
    record_verification_result, retry_backoff_delays_seconds, tool_availability_scan_task_template,
    trace_summary_report_lines, verification_result_report_lines, verification_results_path,
    verify_controls, verify_statistics_plan, watchdog_plan, write_provider_adapters_report,
    AdaptiveSchedulingConfig, AutopilotConfig, CostLatencyRecord, CostRateLimitEstimate, CostTier,
    CuratedMemoryItem, DifficultyBand, DifficultyEstimate, DuplicateWorkHit, IntegrityFinding,
    IntegritySeverity, LocalDifficultyClassification, ModelProvider, ProviderTier, RequeueOutcome,
    RoundTimeCaps, TaskGraph, TaskGraphStatus, TaskGraphTask, VerificationResultRecord,
    VerificationVerdict, WatchdogAction, WatchdogActionKind, WorkerHeartbeat,
};
use std::collections::BTreeMap;
use tempfile::TempDir;

// ---- test fixtures ----

fn task(id: &str, title: &str, desc: &str) -> TaskGraphTask {
    TaskGraphTask {
        id: id.to_string(),
        title: title.to_string(),
        description: desc.to_string(),
        assigned_role: None,
        status: TaskGraphStatus::ReadyParallel,
        priority: 10,
        risk_level: RiskLevel::Low,
        acceptance_criteria: vec!["criterion recorded".to_string()],
        likely_files: Vec::new(),
        test_requirements: Vec::new(),
        depends_on: Vec::new(),
    }
}

fn graph_with(tasks: Vec<TaskGraphTask>) -> TaskGraph {
    TaskGraph {
        prd_path: "PRD.md".to_string(),
        tasks,
        ..TaskGraph::default()
    }
}

// ============================================================================
// Provider tiers & adapter map (tasks 6, 37)
// ============================================================================

#[test]
fn test_provider_tier_maps_each_provider() {
    assert_eq!(provider_tier(&ModelProvider::Local), ProviderTier::Local);
    assert_eq!(
        provider_tier(&ModelProvider::OpenRouterFree),
        ProviderTier::Free
    );
    assert_eq!(provider_tier(&ModelProvider::Codex), ProviderTier::Cheap);
    assert_eq!(
        provider_tier(&ModelProvider::OpenRouterCheap),
        ProviderTier::Cheap
    );
    assert_eq!(
        provider_tier(&ModelProvider::Claude),
        ProviderTier::Frontier
    );
    assert_eq!(
        provider_tier(&ModelProvider::Gemini),
        ProviderTier::Frontier
    );
}

#[test]
fn test_provider_adapter_overview_surfaces_existing_adapter() {
    let overview = provider_adapter_overview(&ModelProvider::Claude);
    assert_eq!(overview.provider, "claude");
    assert_eq!(overview.program, "claude");
    assert_eq!(overview.tier, ProviderTier::Frontier);
    assert!(overview.injection_syntax.contains("/squad"));
    // The Claude adapter passes a skip-permissions flag.
    assert!(overview.args.iter().any(|arg| arg.contains("permissions")));

    let local = provider_adapter_overview(&ModelProvider::Local);
    assert_eq!(local.program, "zsh");
    assert_eq!(local.tier, ProviderTier::Local);
}

#[test]
fn test_all_provider_adapter_overviews_covers_every_provider() {
    let overviews = all_provider_adapter_overviews();
    assert_eq!(overviews.len(), 7);
    let providers: Vec<&str> = overviews.iter().map(|o| o.provider.as_str()).collect();
    assert!(providers.contains(&"claude"));
    assert!(providers.contains(&"codex"));
    assert!(providers.contains(&"openrouter_free"));
    assert!(providers.contains(&"local"));
}

#[test]
fn test_write_provider_adapters_report_produces_artifact() {
    let tmp = TempDir::new().unwrap();
    let overviews = all_provider_adapter_overviews();
    let path = write_provider_adapters_report(tmp.path(), &overviews).unwrap();
    assert_eq!(path, provider_adapters_report_path(tmp.path()));

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("# Provider Adapters"));
    assert!(content.contains("## claude"));
    assert!(content.contains("Program: `claude`"));
    assert!(content.contains("Tier: frontier"));
}

#[test]
fn test_check_provider_availability_matches_path_detection() {
    let availability = check_provider_availability(&ModelProvider::Claude);
    assert_eq!(availability.provider, "claude");
    assert_eq!(availability.program, "claude");
    // available must agree with the shared installer detector.
    assert_eq!(availability.available, squad::setup::is_installed("claude"));
    if availability.available {
        assert!(availability.reason.contains("found"));
    } else {
        assert!(availability.reason.contains("not found"));
    }
}

#[test]
fn test_available_providers_maps_a_list() {
    let results = available_providers(&[ModelProvider::Claude, ModelProvider::OpenRouterFree]);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].provider, "claude");
    assert_eq!(results[1].provider, "openrouter_free");
}

// ============================================================================
// Task difficulty estimator + provider recommendation (task 35)
// ============================================================================

#[test]
fn test_difficulty_band_boundaries() {
    assert_eq!(DifficultyBand::from_score(0), DifficultyBand::Low);
    assert_eq!(DifficultyBand::from_score(32), DifficultyBand::Low);
    assert_eq!(DifficultyBand::from_score(33), DifficultyBand::Medium);
    assert_eq!(DifficultyBand::from_score(65), DifficultyBand::Medium);
    assert_eq!(DifficultyBand::from_score(66), DifficultyBand::High);
    assert_eq!(DifficultyBand::from_score(100), DifficultyBand::High);
}

#[test]
fn test_difficulty_estimator_flags_hard_security_work() {
    let mut hard = task(
        "task-1",
        "Security migration and verification",
        "refactor the auth protocol; add integrity checks",
    );
    hard.risk_level = RiskLevel::High;
    hard.depends_on = vec!["task-0".to_string()];

    let estimate: DifficultyEstimate = estimate_task_difficulty(&hard);
    assert_eq!(estimate.task_id, "task-1");
    assert!(
        estimate.score >= 66,
        "expected high band, got {}",
        estimate.score
    );
    assert_eq!(estimate.band, DifficultyBand::High);
    assert!(!estimate.signals.is_empty());
}

#[test]
fn test_difficulty_estimator_flags_trivial_docs_work() {
    let easy = task(
        "task-2",
        "Fix typo in README docs",
        "rename a preset tag in the summary",
    );
    let estimate = estimate_task_difficulty(&easy);
    assert_eq!(
        estimate.band,
        DifficultyBand::Low,
        "expected low band, got {} ({:?})",
        estimate.score,
        estimate.signals
    );
}

#[test]
fn test_recommend_provider_tier_escalates_with_difficulty() {
    assert_eq!(
        recommend_provider_tier(DifficultyBand::Low),
        ProviderTier::Free
    );
    assert_eq!(
        recommend_provider_tier(DifficultyBand::Medium),
        ProviderTier::Cheap
    );
    assert_eq!(
        recommend_provider_tier(DifficultyBand::High),
        ProviderTier::Frontier
    );
}

#[test]
fn test_claude_is_eligible_only_for_small_coding_tasks() {
    let policy = AdaptiveSchedulingConfig::default();
    let mut small = task(
        "task-1",
        "Implement parser guard",
        "change one Rust function and add a focused unit test",
    );
    small.assigned_role = Some("claude_coding_worker".to_string());
    small.likely_files = vec![
        "src/autopilot.rs".to_string(),
        "tests/autopilot.rs".to_string(),
    ];
    small.acceptance_criteria = vec!["invalid input is rejected".to_string()];
    small.risk_level = RiskLevel::Low;

    let eligibility = claude_task_eligibility(&small, &policy);
    assert!(eligibility.eligible, "{:?}", eligibility.reasons);

    let mut broad = task(
        "task-2",
        "Review architecture and summarize PRD",
        "read the whole PRD, review the architecture, and produce a synthesis report",
    );
    broad.assigned_role = Some("architect".to_string());
    broad.likely_files = vec![
        "src/a.rs".to_string(),
        "src/b.rs".to_string(),
        "src/c.rs".to_string(),
        "src/d.rs".to_string(),
    ];
    broad.acceptance_criteria = vec![
        "criterion 1".to_string(),
        "criterion 2".to_string(),
        "criterion 3".to_string(),
        "criterion 4".to_string(),
    ];

    let eligibility = claude_task_eligibility(&broad, &policy);
    assert!(!eligibility.eligible);
    assert!(eligibility
        .reasons
        .iter()
        .any(|reason| reason.contains("not a coding task")));
}

#[test]
fn test_codex_backfills_ready_work_when_claude_is_stalled() {
    let config = AutopilotConfig::default();
    let tasks = vec![
        task("task-1", "Implement API route", "coding work"),
        task("task-2", "Implement store migration", "coding work"),
        task("task-3", "Write docs summary", "documentation work"),
    ];
    let heartbeats = vec![
        WorkerHeartbeat {
            agent_id: "claude_coding_worker".to_string(),
            last_seen_seconds_ago: config.adaptive_scheduling.claude_stall_seconds,
            assigned_task_id: Some("task-99".to_string()),
        },
        WorkerHeartbeat {
            agent_id: "codex_worker".to_string(),
            last_seen_seconds_ago: 2,
            assigned_task_id: None,
        },
    ];

    let assignments = codex_backfill_plan(&tasks, &heartbeats, &config);

    assert_eq!(
        assignments.len(),
        config.adaptive_scheduling.codex_backfill_batch_size
    );
    assert_eq!(assignments[0].provider, ModelProvider::Codex);
    assert_eq!(assignments[0].task_id, "task-1");
    assert!(assignments[0].reason.contains("Claude worker exceeded"));
}

// ============================================================================
// Cost / rate-limit estimator (task 38)
// ============================================================================

#[test]
fn test_cost_estimator_classifies_each_provider() {
    let local = estimate_cost_and_rate_limit(&ModelProvider::Local);
    assert_eq!(local.cost_tier, CostTier::Free);
    assert_eq!(local.relative_cost, 1);

    let free = estimate_cost_and_rate_limit(&ModelProvider::OpenRouterFree);
    assert_eq!(free.cost_tier, CostTier::Free);
    assert_eq!(free.rate_limit_per_minute, Some(20));

    let claude = estimate_cost_and_rate_limit(&ModelProvider::Claude);
    assert_eq!(claude.cost_tier, CostTier::Premium);
    assert!(claude.relative_cost >= 7);
    assert!(claude.notes.to_lowercase().contains("frontier"));
}

#[test]
fn test_cost_estimator_is_serializable() {
    let estimate: CostRateLimitEstimate = estimate_cost_and_rate_limit(&ModelProvider::Codex);
    let json = serde_json::to_string(&estimate).unwrap();
    assert!(json.contains("\"cost_tier\":\"cheap\""));
}

// ============================================================================
// Retry / backoff (task 74)
// ============================================================================

#[test]
fn test_next_retry_delay_is_exponential_and_capped() {
    assert_eq!(next_retry_delay_seconds(0, 2, 100), 0);
    assert_eq!(next_retry_delay_seconds(1, 2, 100), 2);
    assert_eq!(next_retry_delay_seconds(2, 2, 100), 4);
    assert_eq!(next_retry_delay_seconds(3, 2, 100), 8);
    // Capped at the configured ceiling.
    assert_eq!(next_retry_delay_seconds(10, 2, 10), 10);
}

#[test]
fn test_retry_backoff_schedule() {
    assert_eq!(retry_backoff_delays_seconds(3, 1, 1024), vec![1, 2, 4]);
    assert_eq!(retry_backoff_delays_seconds(0, 1, 100), Vec::<u64>::new());
}

// ============================================================================
// Local duplicate detector (task 82)
// ============================================================================

#[test]
fn test_detect_duplicate_work_flags_near_identical_tasks() {
    let tasks = vec![
        task(
            "task-1",
            "Add retry backoff logic",
            "exponential backoff for retries",
        ),
        task(
            "task-2",
            "Add retry backoff logic",
            "exponential backoff for retries",
        ),
        task(
            "task-3",
            "Write provider availability check",
            "detect installed binaries",
        ),
    ];

    let hits: Vec<DuplicateWorkHit> = detect_duplicate_work(&tasks, 0.5);
    assert_eq!(hits.len(), 1);
    let hit = &hits[0];
    assert_eq!(hit.task_a, "task-1");
    assert_eq!(hit.task_b, "task-2");
    assert!(
        (hit.overlap - 1.0).abs() < f64::EPSILON,
        "expected full overlap, got {}",
        hit.overlap
    );
}

#[test]
fn test_detect_duplicate_work_ignores_unrelated_tasks() {
    let tasks = vec![
        task(
            "task-1",
            "Add retry backoff logic",
            "exponential backoff for retries",
        ),
        task(
            "task-2",
            "Write provider availability check",
            "detect installed binaries on PATH",
        ),
    ];
    let hits = detect_duplicate_work(&tasks, 0.5);
    assert!(hits.is_empty());
}

// ============================================================================
// Local difficulty classifier (task 83)
// ============================================================================

#[test]
fn test_classify_task_difficulty_local_returns_rationale() {
    let mut hard = task(
        "task-1",
        "Security verification refactor",
        "add integrity checks",
    );
    hard.risk_level = RiskLevel::High;
    let class: LocalDifficultyClassification = classify_task_difficulty_local(&hard);
    assert_eq!(class.task_id, "task-1");
    assert_eq!(class.band, DifficultyBand::High);
    assert!(!class.rationale.is_empty());
}

// ============================================================================
// Memory curator (task 84)
// ============================================================================

#[test]
fn test_curate_memory_deduplicates_and_caps() {
    let claims = vec![
        "Rate limit hit free tier".to_string(),
        "free tier hit rate limit".to_string(), // same words, different order -> deduped
        "Cost overrun on frontier model".to_string(),
        "Security review pending".to_string(),
    ];

    let curated: Vec<CuratedMemoryItem> = curate_memory(&claims, 2);
    assert_eq!(curated.len(), 2, "should be capped at 2");
    // First claim wins; the reordered duplicate is dropped.
    assert_eq!(curated[0].summary, "Rate limit hit free tier");
    assert!(curated[0].tags.contains(&"rate-limit".to_string()));
    assert_eq!(curated[1].summary, "Cost overrun on frontier model");
    assert!(curated[1].tags.contains(&"cost".to_string()));
}

#[test]
fn test_curate_memory_tags_and_skips_empty() {
    let curated = curate_memory(
        &vec![
            "   ".to_string(),
            "verification failed and a test regressed".to_string(),
        ],
        5,
    );
    assert_eq!(curated.len(), 1);
    let tags = &curated[0].tags;
    assert!(tags.contains(&"verification".to_string()));
    assert!(tags.contains(&"test".to_string()));
    assert!(tags.contains(&"failure".to_string()));
}

#[test]
fn test_trace_summary_report_lines_use_curated_memory_items() {
    let curated = curate_memory(
        &[
            "retry failed verification".to_string(),
            "duplicate duplicate trace".to_string(),
        ],
        5,
    );

    let lines = trace_summary_report_lines(&curated);

    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("retry failed verification"));
    assert!(lines[0].contains("retry"));
    assert!(lines[0].contains("failure"));
    assert!(lines[0].contains("verification"));
}

// ============================================================================
// Cost / latency capture (task 102)
// ============================================================================

#[test]
fn test_record_cost_latency_appends_and_normalizes() {
    let tmp = TempDir::new().unwrap();
    let records = record_cost_latency(
        tmp.path(),
        CostLatencyRecord {
            task_id: Some(" task-74 ".to_string()),
            agent_id: Some(" router ".to_string()),
            provider: Some(" claude ".to_string()),
            latency_ms: Some(1230),
            prompt_tokens: Some(120),
            completion_tokens: Some(80),
            estimated_cost_usd: Some(0.0123),
            notes: Some(" first attempt ".to_string()),
        },
    )
    .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].task_id.as_deref(), Some("task-74"));
    assert_eq!(records[0].provider.as_deref(), Some("claude"));
    assert_eq!(records[0].notes.as_deref(), Some("first attempt"));

    // Round-trips through the artifact.
    assert_eq!(read_cost_latency(tmp.path()).unwrap(), records);
    assert_eq!(
        cost_latency_path(tmp.path()),
        tmp.path()
            .join(".squad")
            .join("autopilot")
            .join("cost-latency.json")
    );
}

#[test]
fn test_cost_latency_report_lines_format_entries() {
    let lines = cost_latency_report_lines(&[CostLatencyRecord {
        task_id: Some("task-74".to_string()),
        agent_id: Some("router".to_string()),
        provider: Some("claude".to_string()),
        latency_ms: Some(1230),
        prompt_tokens: Some(120),
        completion_tokens: Some(80),
        estimated_cost_usd: Some(0.0123),
        notes: Some("first attempt".to_string()),
    }]);
    assert_eq!(lines.len(), 1);
    let line = &lines[0];
    assert!(line.contains("claude"));
    assert!(line.contains("1230ms"));
    assert!(line.contains("120+80 tok"));
    assert!(line.contains("[task: task-74]"));
    assert!(line.contains("[agent: router]"));
}

#[test]
fn test_record_verification_result_appends_structured_verdicts() {
    let tmp = TempDir::new().unwrap();

    let records = record_verification_result(
        tmp.path(),
        VerificationResultRecord {
            task_id: Some(" T101 ".to_string()),
            verifier_id: Some(" verifier ".to_string()),
            verdict: VerificationVerdict::Pass,
            summary: " verified from test ".to_string(),
            evidence: vec![" cargo test ".to_string(), " ".to_string()],
        },
    )
    .unwrap();

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].task_id.as_deref(), Some("T101"));
    assert_eq!(records[0].verifier_id.as_deref(), Some("verifier"));
    assert_eq!(records[0].summary, "verified from test");
    assert_eq!(records[0].evidence, vec!["cargo test"]);
    assert!(verification_results_path(tmp.path()).exists());
    assert_eq!(read_verification_results(tmp.path()).unwrap(), records);
}

#[test]
fn test_verification_result_report_lines_format_verdict_and_evidence() {
    let lines = verification_result_report_lines(&[VerificationResultRecord {
        task_id: Some("T101".to_string()),
        verifier_id: Some("verification_worker".to_string()),
        verdict: VerificationVerdict::Blocked,
        summary: "missing control".to_string(),
        evidence: vec!["control report absent".to_string()],
    }]);

    assert_eq!(
        lines,
        vec![
            "blocked: missing control [task: T101] [verifier: verification_worker]: control report absent"
                .to_string()
        ]
    );
}

#[test]
fn test_verify_controls_reports_missing_required_controls() {
    let passing = verify_controls(&["negative", "baseline"], &["baseline", "negative"]);
    assert!(passing.passed);

    let failing = verify_controls(&["negative", "positive"], &["negative"]);
    assert!(!failing.passed);
    assert_eq!(failing.findings, vec!["missing control: positive"]);
}

#[test]
fn test_verify_statistics_plan_requires_metric_threshold_and_sample_size() {
    let passing = verify_statistics_plan(&["accuracy"], &["p < 0.05"], Some(12));
    assert!(passing.passed);

    let failing = verify_statistics_plan(&[], &[""], Some(0));
    assert!(!failing.passed);
    assert!(failing
        .findings
        .contains(&"missing statistical metric".to_string()));
    assert!(failing
        .findings
        .contains(&"missing decision threshold".to_string()));
    assert!(failing
        .findings
        .contains(&"missing positive sample size".to_string()));
}

// ============================================================================
// Automatic requeue of failed tasks (task 67)
// ============================================================================

#[test]
fn test_plan_failed_task_requeue_retries_then_exhausts() {
    let mut graph = graph_with(vec![task("task-1", "Do work", "desc")]);
    graph.tasks[0].status = TaskGraphStatus::Failed;
    let mut attempts = BTreeMap::new();

    // First failure -> requeued (no deps -> ReadyParallel), attempt 1.
    let actions = plan_failed_task_requeue(&mut graph, &mut attempts, 2);
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].outcome, RequeueOutcome::Requeued);
    assert_eq!(actions[0].attempt, 1);
    assert_eq!(graph.tasks[0].status, TaskGraphStatus::ReadyParallel);

    // Second failure -> attempt 2.
    graph.tasks[0].status = TaskGraphStatus::Failed;
    let actions = plan_failed_task_requeue(&mut graph, &mut attempts, 2);
    assert_eq!(actions[0].attempt, 2);
    assert_eq!(actions[0].outcome, RequeueOutcome::Requeued);

    // Third failure -> cap exceeded, stays Failed.
    graph.tasks[0].status = TaskGraphStatus::Failed;
    let actions = plan_failed_task_requeue(&mut graph, &mut attempts, 2);
    assert_eq!(actions[0].outcome, RequeueOutcome::MaxRetriesExhausted);
    assert_eq!(graph.tasks[0].status, TaskGraphStatus::Failed);
}

#[test]
fn test_plan_failed_task_requeue_blocks_when_deps_incomplete() {
    let mut graph = graph_with(vec![task("task-1", "Upstream", "desc"), {
        let mut t = task("task-2", "Downstream", "desc");
        t.depends_on = vec!["task-1".to_string()];
        t.status = TaskGraphStatus::Failed;
        t
    }]);
    let mut attempts = BTreeMap::new();
    let actions = plan_failed_task_requeue(&mut graph, &mut attempts, 3);
    // task-1 is not failed -> skipped; task-2 failed with an incomplete dep -> Blocked.
    assert_eq!(actions.len(), 2);
    let downstream = actions.iter().find(|a| a.task_id == "task-2").unwrap();
    assert_eq!(downstream.outcome, RequeueOutcome::Requeued);
    assert_eq!(
        graph
            .tasks
            .iter()
            .find(|t| t.id == "task-2")
            .unwrap()
            .status,
        TaskGraphStatus::Blocked
    );
}

// ============================================================================
// Watchdog restart planner (task 124)
// ============================================================================

#[test]
fn test_watchdog_plan_keeps_restart_and_escalates() {
    let heartbeats = vec![
        WorkerHeartbeat {
            agent_id: "fresh".to_string(),
            last_seen_seconds_ago: 5,
            assigned_task_id: Some("task-1".to_string()),
        },
        WorkerHeartbeat {
            agent_id: "stalled".to_string(),
            last_seen_seconds_ago: 120,
            assigned_task_id: Some("task-2".to_string()),
        },
        WorkerHeartbeat {
            agent_id: "long-gone".to_string(),
            last_seen_seconds_ago: 900,
            assigned_task_id: Some("task-3".to_string()),
        },
        WorkerHeartbeat {
            agent_id: "idle-no-work".to_string(),
            last_seen_seconds_ago: 900,
            assigned_task_id: None,
        },
    ];

    let actions: Vec<WatchdogAction> = watchdog_plan(&heartbeats, 60, 600);
    let by_id = |id: &str| actions.iter().find(|a| a.agent_id == id).unwrap();
    assert_eq!(by_id("fresh").action, WatchdogActionKind::Keep);
    assert_eq!(by_id("stalled").action, WatchdogActionKind::Restart);
    assert_eq!(by_id("long-gone").action, WatchdogActionKind::Escalate);
    assert_eq!(by_id("idle-no-work").action, WatchdogActionKind::Keep);
}

// ============================================================================
// Continuous integrity checks (task 125)
// ============================================================================

#[test]
fn test_continuous_integrity_clean_graph_has_no_findings() {
    let graph = graph_with(vec![
        task("task-1", "First", "desc"),
        task("task-2", "Second", "desc"),
    ]);
    let findings: Vec<IntegrityFinding> = continuous_integrity_check(&graph);
    assert!(
        findings.is_empty(),
        "expected no findings, got {:?}",
        findings
    );
}

#[test]
fn test_continuous_integrity_detects_cycle_and_missing_criteria() {
    // task-1 depends on task-2 and task-2 depends on task-1: a cycle.
    let mut t1 = task("task-1", "First", "desc");
    t1.depends_on = vec!["task-2".to_string()];
    t1.acceptance_criteria = Vec::new();
    let mut t2 = task("task-2", "Second", "desc");
    t2.depends_on = vec!["task-1".to_string()];
    let graph = graph_with(vec![t1, t2]);

    let findings = continuous_integrity_check(&graph);
    let codes: Vec<&str> = findings.iter().map(|f| f.code.as_str()).collect();
    assert!(codes.contains(&"dependency_cycle"), "codes: {:?}", codes);
    assert!(
        codes.contains(&"missing_acceptance_criteria"),
        "codes: {:?}",
        codes
    );
    assert!(findings
        .iter()
        .any(|f| f.severity == IntegritySeverity::Error));
}

// ============================================================================
// Round / time caps (task 126)
// ============================================================================

#[test]
fn test_enforce_caps_within_budget() {
    let caps = RoundTimeCaps::default();
    let status = enforce_caps(3, 600, &caps);
    assert!(!status.should_stop);
    assert!(!status.hit_round_cap);
    assert!(!status.hit_time_cap);
    assert!(status.rounds_remaining > 0);
}

#[test]
fn test_enforce_caps_round_and_time_limits_stop_the_run() {
    let caps = RoundTimeCaps {
        max_rounds: 5,
        max_runtime_seconds: 3600,
        max_requeues_per_task: 2,
    };
    let over_rounds = enforce_caps(5, 100, &caps);
    assert!(over_rounds.hit_round_cap);
    assert!(over_rounds.should_stop);

    let over_time = enforce_caps(1, 3600, &caps);
    assert!(over_time.hit_time_cap);
    assert!(over_time.should_stop);
}

// ============================================================================
// Acceptance criteria extraction (task 14)
// ============================================================================

#[test]
fn test_extract_acceptance_criteria_from_prd_section() {
    let prd = "# Goals\nBuild it.\n\n## Acceptance Criteria\n- Plan reports ten tasks.\n- Run creates one run.\n\n## Risks\n- Spawning stays dry-run.\n";
    let criteria = extract_acceptance_criteria(prd);
    assert_eq!(
        criteria,
        vec![
            "Plan reports ten tasks.".to_string(),
            "Run creates one run.".to_string(),
        ]
    );
}

#[test]
fn test_extract_acceptance_criteria_handles_singular_heading() {
    let prd = "## Acceptance Criterion\n- Single criterion line.\n";
    let criteria = extract_acceptance_criteria(prd);
    assert_eq!(criteria, vec!["Single criterion line.".to_string()]);
}

// ============================================================================
// Dataset discovery task template (task 26)
// ============================================================================

#[test]
fn test_dataset_discovery_task_template_is_ready_and_verifiable() {
    let template = dataset_discovery_task_template("task-26");
    assert_eq!(template.id, "task-26");
    assert_eq!(template.status, TaskGraphStatus::ReadyParallel);
    assert_eq!(template.risk_level, RiskLevel::Low);
    assert_eq!(template.assigned_role.as_deref(), Some("literature_worker"));
    assert!(!template.acceptance_criteria.is_empty());
    // Independent verification requirement is present.
    assert!(template
        .test_requirements
        .iter()
        .any(|req| req.contains("independent")));
}

#[test]
fn test_background_evidence_search_task_template_is_context_only() {
    let template = background_evidence_search_task_template("task-25");
    assert_eq!(template.id, "task-25");
    assert_eq!(template.status, TaskGraphStatus::ReadyParallel);
    assert_eq!(template.risk_level, RiskLevel::Low);
    assert_eq!(template.assigned_role.as_deref(), Some("literature_worker"));
    assert!(template
        .acceptance_criteria
        .iter()
        .any(|criterion| criterion.contains("not a final scientific claim")));
    assert!(template
        .test_requirements
        .iter()
        .any(|requirement| requirement.contains("independent verifier")));
}

#[test]
fn test_tool_availability_scan_template_fails_loudly_without_fallbacks() {
    let template = tool_availability_scan_task_template("task-27");
    assert_eq!(template.status, TaskGraphStatus::ReadyParallel);
    assert_eq!(template.risk_level, RiskLevel::Low);
    assert_eq!(template.assigned_role.as_deref(), Some("tool_mapper"));
    assert!(template
        .description
        .contains("missing tools must be reported"));
    assert!(template
        .acceptance_criteria
        .iter()
        .any(|criterion| criterion.contains("no fabricated fallback")));
}

#[test]
fn test_independent_verification_template_requires_separate_verdict() {
    let template = independent_verification_task_template("task-28");
    assert_eq!(template.status, TaskGraphStatus::ReadyParallel);
    assert_eq!(template.risk_level, RiskLevel::Low);
    assert_eq!(
        template.assigned_role.as_deref(),
        Some("verification_worker")
    );
    assert!(template
        .acceptance_criteria
        .iter()
        .any(|criterion| criterion.contains("pass, fail, or blocked")));
    assert!(template
        .acceptance_criteria
        .iter()
        .any(|criterion| criterion.contains("does not review its own")));
}
