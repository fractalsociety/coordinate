//! End-to-end guard for the autopilot release BLOCK.
//!
//! Runs the full autopilot pipeline against `amc-test-prd.md` in a FRESH
//! temporary workspace using the SHIPPED default config (no hand editing), and
//! asserts the three release criteria:
//!   1. `autopilot plan` reports EXACTLY 10 tasks (parser must not absorb
//!      `## Completion Notes` or any other non-task section as tasks).
//!   2. The shipped default model mix is Claude 50% / Codex 50% with everything
//!      else at 0%.
//!   3. `autopilot launch` plans EXACTLY 10 sessions: 5 Claude and 5 Codex.
//!
//! The test MUST FAIL if the parser over-counts tasks or the default mix drifts
//! away from the 50/50 Claude/Codex contract.

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
        config.contains("claude = 0.50") && config.contains("codex = 0.50"),
        "shipped default mix must be Claude 50% / Codex 50%"
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

    // STEP 2 guard: run synthesizes exactly 10 agents / 10 sessions.
    let run = squad(workspace)
        .args(["autopilot", "run", &amc_prd_path()])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let run = String::from_utf8_lossy(&run);
    assert!(
        run.contains("Agents: 10") && run.contains("Sessions planned: 10"),
        "run must synthesize 10 agents and 10 sessions; got: {run}"
    );

    // STEP 2 guard: launch plans exactly 10 sessions split 5 Claude / 5 Codex.
    let launch = squad(workspace)
        .args(["autopilot", "launch", "--run-id", "1"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let launch = String::from_utf8_lossy(&launch);
    assert!(
        launch.contains("Sessions: 10"),
        "launch must plan 10 sessions; got: {launch}"
    );
    let claude_sessions = launch.matches("claude ->").count();
    let codex_sessions = launch.matches("codex ->").count();
    assert_eq!(
        claude_sessions, 5,
        "expected 5 Claude sessions; got {claude_sessions}\n{launch}"
    );
    assert_eq!(
        codex_sessions, 5,
        "expected 5 Codex sessions; got {codex_sessions}\n{launch}"
    );
}
