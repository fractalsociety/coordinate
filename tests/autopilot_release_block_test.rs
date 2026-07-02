//! End-to-end guard for the autopilot release BLOCK.
//!
//! Runs the full autopilot pipeline against `amc-test-prd.md` in a FRESH
//! temporary workspace using the SHIPPED default config (no hand editing), and
//! asserts the three release criteria:
//!   1. `autopilot plan` reports EXACTLY 10 tasks (parser must not absorb
//!      `## Completion Notes` or any other non-task section as tasks).
//!   2. The shipped default model mix is Codex-heavy (Claude 20% / Codex 80%)
//!      with adaptive scheduling enabled and everything else at 0%.
//!   3. `autopilot launch` does not assign broad manager/review/planning
//!      sessions to Claude; Claude is only allowed for a small coding worker.
//!
//! The test MUST FAIL if the parser over-counts tasks or the adaptive
//! Codex-managed contract drifts.

use assert_cmd::Command;
use tempfile::TempDir;

fn squad(workspace: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("squad").unwrap();
    cmd.current_dir(workspace);
    cmd
}

fn amc_prd_path() -> String {
    format!("{}/amc-test-prd.md", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn fresh_init_autopilot_pipeline_meets_release_block_criteria() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path();

    // Fresh init writes the shipped default autopilot.toml.
    squad(workspace).arg("init").assert().success();
    squad(workspace)
        .args(["autopilot", "init"])
        .assert()
        .success();

    let config = std::fs::read_to_string(workspace.join(".squad").join("autopilot.toml")).unwrap();
    assert!(
        config.contains("claude = 0.20") && config.contains("codex = 0.80"),
        "shipped default mix must be Claude 20% / Codex 80%"
    );
    assert!(
        config.contains("enabled = true")
            && config.contains("claude_coding_only = true")
            && config.contains("codex_backfill_when_waiting_on_claude = true"),
        "adaptive scheduling must be enabled by default: {config}"
    );
    assert!(
        config.contains("gemini = 0.00")
            && config.contains("openrouter_free = 0.00")
            && config.contains("openrouter_cheap = 0.00")
            && config.contains("local = 0.00"),
        "non-Claude/Codex providers must be disabled by default: {config}"
    );

    // STEP 1 guard: parser emits exactly 10 tasks (not 15).
    let plan = squad(workspace)
        .args(["autopilot", "plan", &amc_prd_path()])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let plan = String::from_utf8_lossy(&plan);
    assert!(
        plan.contains("Tasks: 10"),
        "plan must report exactly 10 tasks; got: {plan}"
    );

    // STEP 2 guard: run synthesizes a Codex-managed team. The generated team
    // may include an extra small Claude coding worker, but broad roles stay on
    // Codex.
    let run = squad(workspace)
        .args(["autopilot", "run", &amc_prd_path()])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let run = String::from_utf8_lossy(&run);
    assert!(
        run.contains("Agents: 11") && run.contains("Sessions planned: 11"),
        "run must synthesize 11 agents and 11 sessions with the small Claude coding worker; got: {run}"
    );

    // STEP 2 guard: launch plans one Claude session and the rest Codex.
    let launch = squad(workspace)
        .args(["autopilot", "launch", "--run-id", "1"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let launch = String::from_utf8_lossy(&launch);
    assert!(
        launch.contains("Sessions: 11"),
        "launch must plan 11 sessions; got: {launch}"
    );
    let claude_sessions = launch.matches("claude ->").count();
    let codex_sessions = launch.matches("codex ->").count();
    assert_eq!(
        claude_sessions, 1,
        "expected 1 Claude small-coding session; got {claude_sessions}\n{launch}"
    );
    assert_eq!(
        codex_sessions, 10,
        "expected 10 Codex sessions; got {codex_sessions}\n{launch}"
    );
    assert!(launch.contains("claude_coding_worker"));
    assert!(!launch.contains("manager [manager] claude"));
}
