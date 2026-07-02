use squad::autopilot::{
    failures_retries_path, failures_retries_report_lines, files_changed_path, final_report_path,
    init_autopilot_workspace, load_config, read_failures_retries, read_files_changed,
    record_failure_retry, record_files_changed, render_final_report, write_final_report,
    AdaptiveSchedulingConfig, AutopilotConfig, FailureRetryRecord, FinalReport, ModelMix,
    ModelProvider,
};
use tempfile::TempDir;

#[test]
fn test_default_autopilot_config_uses_codex_heavy_adaptive_mix() {
    let tmp = TempDir::new().unwrap();
    init_autopilot_workspace(tmp.path()).unwrap();
    let config = load_config(tmp.path()).unwrap();

    assert_eq!(
        config.model_mix,
        ModelMix {
            claude: 0.20,
            codex: 0.80,
            gemini: 0.00,
            openrouter_free: 0.00,
            openrouter_cheap: 0.00,
            local: 0.00,
        }
    );
    assert_eq!(
        config.adaptive_scheduling,
        AdaptiveSchedulingConfig::default()
    );

    assert_eq!(
        config.role_overrides.get("literature_worker"),
        Some(&ModelProvider::Codex)
    );
    assert_eq!(
        config.role_overrides.get("test_worker"),
        Some(&ModelProvider::Codex)
    );
    assert_eq!(
        config.role_overrides.get("claude_coding_worker"),
        Some(&ModelProvider::Claude)
    );
    assert_eq!(
        config.role_overrides.get("trace_collector"),
        Some(&ModelProvider::Codex)
    );
}

#[test]
fn test_missing_autopilot_config_uses_defaults() {
    let tmp = TempDir::new().unwrap();
    let config = load_config(tmp.path()).unwrap();

    assert_eq!(config, AutopilotConfig::default());
    assert_eq!(config.model_mix, ModelMix::default());
    assert!(config.role_overrides.is_empty());
}

#[test]
fn test_loads_model_mix_and_role_overrides_from_autopilot_toml() {
    let tmp = TempDir::new().unwrap();
    let squad_dir = tmp.path().join(".squad");
    std::fs::create_dir_all(&squad_dir).unwrap();
    std::fs::write(
        squad_dir.join("autopilot.toml"),
        r#"
[model_mix]
claude = 0.25
codex = 0.50
gemini = 0.15
openrouter_free = 0.05
openrouter_cheap = 0.05
local = 0.10

[role_overrides]
manager = "claude"
architect = "claude"
rust_backend = "codex"
sqlite_engineer = "codex"
test_engineer = "codex"
security_reviewer = "claude"
docs = "gemini"
validation = "local"
terminal = "opencode"
literature_worker = "openrouter_free"
test_worker = "openrouter_cheap"
"#,
    )
    .unwrap();

    let config = load_config(tmp.path()).unwrap();

    assert_eq!(
        config.model_mix,
        ModelMix {
            claude: 0.25,
            codex: 0.50,
            gemini: 0.15,
            openrouter_free: 0.05,
            openrouter_cheap: 0.05,
            local: 0.10,
        }
    );
    assert_eq!(
        config.role_overrides.get("manager"),
        Some(&ModelProvider::Claude)
    );
    assert_eq!(
        config.role_overrides.get("rust_backend"),
        Some(&ModelProvider::Codex)
    );
    assert_eq!(
        config.role_overrides.get("docs"),
        Some(&ModelProvider::Gemini)
    );
    assert_eq!(
        config.role_overrides.get("validation"),
        Some(&ModelProvider::Local)
    );
    assert_eq!(
        config.role_overrides.get("terminal"),
        Some(&ModelProvider::OpenCode)
    );
    assert_eq!(
        config.role_overrides.get("literature_worker"),
        Some(&ModelProvider::OpenRouterFree)
    );
    assert_eq!(
        config.role_overrides.get("test_worker"),
        Some(&ModelProvider::OpenRouterCheap)
    );
}

#[test]
fn test_rejects_invalid_model_mix_values() {
    let tmp = TempDir::new().unwrap();
    let squad_dir = tmp.path().join(".squad");
    std::fs::create_dir_all(&squad_dir).unwrap();
    std::fs::write(
        squad_dir.join("autopilot.toml"),
        r#"
[model_mix]
claude = 0.0
codex = -0.1
gemini = 0.0
openrouter_free = 0.0
openrouter_cheap = 0.0
local = 0.0
"#,
    )
    .unwrap();

    let error = load_config(tmp.path()).unwrap_err().to_string();

    assert!(error.contains("failed to validate autopilot config"));
}

#[test]
fn test_provider_for_role_falls_back_to_default() {
    let tmp = TempDir::new().unwrap();
    let squad_dir = tmp.path().join(".squad");
    std::fs::create_dir_all(&squad_dir).unwrap();
    std::fs::write(
        squad_dir.join("autopilot.toml"),
        r#"
[role_overrides]
docs = "gemini"
"#,
    )
    .unwrap();

    let config = load_config(tmp.path()).unwrap();
    let default_provider = ModelProvider::Codex;

    assert_eq!(
        config.provider_for_role("docs", &default_provider),
        &ModelProvider::Gemini
    );
    assert_eq!(
        config.provider_for_role("rust_backend", &default_provider),
        &ModelProvider::Codex
    );
}

#[test]
fn test_rejects_unknown_role_override_provider() {
    let tmp = TempDir::new().unwrap();
    let squad_dir = tmp.path().join(".squad");
    std::fs::create_dir_all(&squad_dir).unwrap();
    std::fs::write(
        squad_dir.join("autopilot.toml"),
        r#"
[role_overrides]
manager = "expensive-ai"
"#,
    )
    .unwrap();

    let error = load_config(tmp.path()).unwrap_err().to_string();
    assert!(error.contains("failed to parse autopilot config"));
}

#[test]
fn test_render_final_report_includes_required_prd_sections() {
    let report = FinalReport {
        product_goals: vec!["Preserve PRD context".to_string()],
        milestones: vec!["MVP 1: PRD ingestion".to_string()],
        acceptance_criteria: vec!["Final report includes acceptance criteria".to_string()],
        test_requirements: vec!["Run the autopilot plan command".to_string()],
        prd_tasks_completed: vec!["Implement receive update".to_string()],
        task_graph: vec!["1 -> 2 -> review".to_string()],
        agents_used: vec!["manager: claude".to_string(), "worker: codex".to_string()],
        model_mix_used: vec!["claude 40%".to_string(), "codex 40%".to_string()],
        files_changed: vec!["src/store.rs".to_string()],
        tests_run: vec!["cargo test".to_string()],
        failures_retries: vec!["worker retry after review feedback".to_string()],
        unresolved_risks: vec!["manual release not verified".to_string()],
        final_git_diff_summary: " src/store.rs | 2 +-\n 1 file changed".to_string(),
    };

    let markdown = render_final_report(&report);

    assert!(markdown.contains("# Squad Autopilot Final Report"));
    assert!(markdown.contains("## Product Goals"));
    assert!(markdown.contains("- Preserve PRD context"));
    assert!(markdown.contains("## Milestones"));
    assert!(markdown.contains("- MVP 1: PRD ingestion"));
    assert!(markdown.contains("## Acceptance Criteria"));
    assert!(markdown.contains("- Final report includes acceptance criteria"));
    assert!(markdown.contains("## Test Requirements"));
    assert!(markdown.contains("- Run the autopilot plan command"));
    assert!(markdown.contains("## PRD Tasks Completed"));
    assert!(markdown.contains("- Implement receive update"));
    assert!(markdown.contains("## Task Graph"));
    assert!(markdown.contains("## Agents Used"));
    assert!(markdown.contains("## Model Mix Used"));
    assert!(markdown.contains("## Files Changed"));
    assert!(markdown.contains("## Tests Run"));
    assert!(markdown.contains("## Failures / Retries"));
    assert!(markdown.contains("## Unresolved Risks"));
    assert!(markdown.contains("## Final Git Diff Summary"));
    assert!(markdown.contains("```text\nsrc/store.rs | 2 +-\n 1 file changed\n```"));
}

#[test]
fn test_write_final_report_creates_autopilot_report_path() {
    let tmp = TempDir::new().unwrap();
    let report = FinalReport {
        prd_tasks_completed: vec!["README update".to_string()],
        ..FinalReport::default()
    };

    let path = write_final_report(tmp.path(), &report).unwrap();

    assert_eq!(path, final_report_path(tmp.path()));
    assert_eq!(
        path,
        tmp.path()
            .join(".squad")
            .join("autopilot")
            .join("final-report.md")
    );
    let markdown = std::fs::read_to_string(path).unwrap();
    assert!(markdown.contains("- README update"));
    assert!(markdown.contains("## Final Git Diff Summary"));
    assert!(markdown.contains("_None recorded._"));
}

#[test]
fn test_read_files_changed_missing_artifact_returns_empty_list() {
    let tmp = TempDir::new().unwrap();

    let files = read_files_changed(tmp.path()).unwrap();

    assert!(files.is_empty());
}

#[test]
fn test_record_files_changed_tracks_unique_files_in_first_seen_order() {
    let tmp = TempDir::new().unwrap();

    let first = record_files_changed(
        tmp.path(),
        &[
            "src/store.rs".to_string(),
            " README.md ".to_string(),
            "src/store.rs".to_string(),
            String::new(),
        ],
    )
    .unwrap();
    let second = record_files_changed(
        tmp.path(),
        &[
            "tests/store_test.rs".to_string(),
            "README.md".to_string(),
            "src/main.rs".to_string(),
        ],
    )
    .unwrap();

    assert_eq!(
        first,
        vec!["src/store.rs".to_string(), "README.md".to_string()]
    );
    assert_eq!(
        second,
        vec![
            "src/store.rs".to_string(),
            "README.md".to_string(),
            "tests/store_test.rs".to_string(),
            "src/main.rs".to_string()
        ]
    );
    assert_eq!(read_files_changed(tmp.path()).unwrap(), second);
    assert_eq!(
        std::fs::read_to_string(files_changed_path(tmp.path())).unwrap(),
        "src/store.rs\nREADME.md\ntests/store_test.rs\nsrc/main.rs\n"
    );
}

#[test]
fn test_record_failure_retry_tracks_structured_failure_history() {
    let tmp = TempDir::new().unwrap();

    let records = record_failure_retry(
        tmp.path(),
        FailureRetryRecord {
            task_id: Some(" task-1 ".to_string()),
            agent_id: Some(" worker ".to_string()),
            attempt: 2,
            action: " requeue ".to_string(),
            notes: Some(" needs tests ".to_string()),
        },
    )
    .unwrap();

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].task_id.as_deref(), Some("task-1"));
    assert_eq!(records[0].agent_id.as_deref(), Some("worker"));
    assert_eq!(records[0].action, "requeue");
    assert_eq!(records[0].notes.as_deref(), Some("needs tests"));
    assert_eq!(read_failures_retries(tmp.path()).unwrap(), records);
    assert_eq!(
        failures_retries_path(tmp.path()),
        tmp.path()
            .join(".squad")
            .join("autopilot")
            .join("failures-retries.json")
    );
    assert_eq!(
        failures_retries_report_lines(&records),
        vec!["requeue [attempt: 2] [task: task-1] [agent: worker]: needs tests".to_string()]
    );

    let error = record_failure_retry(
        tmp.path(),
        FailureRetryRecord {
            action: " ".to_string(),
            attempt: 1,
            task_id: None,
            agent_id: None,
            notes: None,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("failure/retry action cannot be empty"));
}
