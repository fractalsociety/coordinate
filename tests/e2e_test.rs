use assert_cmd::Command;
use predicates::prelude::*;
use squad::store::Store;
use std::process::{Command as StdCommand, Stdio};
use std::thread::sleep;
use std::time::Duration;
use tempfile::TempDir;

fn squad(workspace: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("squad").unwrap();
    cmd.current_dir(workspace);
    cmd
}

fn setup_workspace() -> TempDir {
    let tmp = TempDir::new().unwrap();
    squad(tmp.path()).arg("init").assert().success();
    tmp
}

/// Full collaboration flow: manager -> worker -> inspector -> FAIL -> rework -> PASS
#[test]
fn test_full_collaboration_flow() {
    let tmp = setup_workspace();

    // 1. Three agents join
    squad(tmp.path())
        .args(["join", "manager", "--role", "manager"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "worker", "--role", "worker"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "inspector", "--role", "inspector"])
        .assert()
        .success();

    // 2. Manager sends task to worker
    squad(tmp.path())
        .args([
            "send",
            "manager",
            "worker",
            "implement auth module with JWT",
        ])
        .assert()
        .success();

    // 3. Worker receives and replies
    squad(tmp.path())
        .args(["receive", "worker"])
        .assert()
        .success()
        .stdout(predicate::str::contains("implement auth module with JWT"));

    squad(tmp.path())
        .args([
            "send",
            "worker",
            "manager",
            "done: added JWT auth in src/auth.rs",
        ])
        .assert()
        .success();

    // 4. Manager forwards to inspector
    squad(tmp.path())
        .args(["receive", "manager"])
        .assert()
        .success()
        .stdout(predicate::str::contains("done: added JWT auth"));

    squad(tmp.path())
        .args([
            "send",
            "manager",
            "inspector",
            "review worker's auth implementation in src/auth.rs",
        ])
        .assert()
        .success();

    // 5. Inspector sends FAIL
    squad(tmp.path())
        .args(["receive", "inspector"])
        .assert()
        .success();

    squad(tmp.path())
        .args([
            "send",
            "inspector",
            "worker",
            "missing token expiration check",
        ])
        .assert()
        .success();
    squad(tmp.path())
        .args([
            "send",
            "inspector",
            "manager",
            "FAIL: missing token expiration check",
        ])
        .assert()
        .success();

    // 6. Manager gets FAIL, forwards to worker
    squad(tmp.path())
        .args(["receive", "manager"])
        .assert()
        .success()
        .stdout(predicate::str::contains("FAIL"));

    squad(tmp.path())
        .args([
            "send",
            "manager",
            "worker",
            "inspector says: add token expiration check",
        ])
        .assert()
        .success();

    // 7. Worker fixes and reports back
    squad(tmp.path())
        .args(["receive", "worker"])
        .assert()
        .success()
        .stdout(predicate::str::contains("token expiration"));

    squad(tmp.path())
        .args([
            "send",
            "worker",
            "manager",
            "done: added token expiration validation",
        ])
        .assert()
        .success();

    // 8. Manager forwards to inspector again
    squad(tmp.path())
        .args(["receive", "manager"])
        .assert()
        .success();
    squad(tmp.path())
        .args([
            "send",
            "manager",
            "inspector",
            "review updated auth with expiration check",
        ])
        .assert()
        .success();

    // 9. Inspector sends PASS
    squad(tmp.path())
        .args(["receive", "inspector"])
        .assert()
        .success();
    squad(tmp.path())
        .args([
            "send",
            "inspector",
            "manager",
            "PASS: auth module looks good with expiration",
        ])
        .assert()
        .success();

    // 10. Manager receives PASS
    squad(tmp.path())
        .args(["receive", "manager"])
        .assert()
        .success()
        .stdout(predicate::str::contains("PASS"));

    // 11. History shows full conversation
    squad(tmp.path())
        .arg("history")
        .assert()
        .success()
        .stdout(predicate::str::contains("implement auth module with JWT"))
        .stdout(predicate::str::contains("FAIL"))
        .stdout(predicate::str::contains("PASS"));
}

/// Broadcast to multiple workers
#[test]
fn test_broadcast_to_workers() {
    let tmp = setup_workspace();

    squad(tmp.path())
        .args(["join", "manager"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "worker-1", "--role", "worker"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "worker-2", "--role", "worker"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "worker-3", "--role", "worker"])
        .assert()
        .success();

    squad(tmp.path())
        .args([
            "send",
            "manager",
            "@all",
            "API contract updated, rebase your work",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Broadcast to 3 agents"));

    // Each worker gets the message
    for worker in &["worker-1", "worker-2", "worker-3"] {
        squad(tmp.path())
            .args(["receive", worker])
            .assert()
            .success()
            .stdout(predicate::str::contains("API contract updated"));
    }
}

/// Send to archived agent fails
#[test]
fn test_send_to_left_agent_fails() {
    let tmp = setup_workspace();

    squad(tmp.path())
        .args(["join", "manager"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["leave", "worker"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["send", "manager", "worker", "hello"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("worker is archived"));
}

#[test]
fn test_archived_agent_cannot_receive_until_rejoin() {
    let tmp = setup_workspace();

    squad(tmp.path())
        .args(["join", "manager"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["send", "manager", "worker", "task-before-leave"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["leave", "worker"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["receive", "worker"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("worker is archived"));
}

#[test]
fn test_receive_wait_fails_if_agent_is_archived_mid_poll() {
    let tmp = setup_workspace();

    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success();

    let child = StdCommand::new(assert_cmd::cargo::cargo_bin("squad"))
        .current_dir(tmp.path())
        .args(["receive", "worker", "--wait", "--timeout", "5"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    sleep(Duration::from_millis(700));

    squad(tmp.path())
        .args(["leave", "worker"])
        .assert()
        .success();

    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("worker is archived"));
}

#[test]
fn test_old_receive_wait_does_not_consume_rejoined_session_messages() {
    let tmp = setup_workspace();

    squad(tmp.path())
        .args(["join", "manager"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success();

    let mut child = StdCommand::new(assert_cmd::cargo::cargo_bin("squad"))
        .current_dir(tmp.path())
        .args(["receive", "worker", "--wait", "--timeout", "5"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    sleep(Duration::from_millis(700));

    squad(tmp.path())
        .args(["leave", "worker"])
        .assert()
        .success();

    let workspace = tmp.path().join(".squad");
    let store = Store::open(&workspace.join("messages.db")).unwrap();
    let (_id, token) = store.register_agent_unique("worker", "worker").unwrap();

    sleep(Duration::from_millis(700));

    let early_status = child.try_wait().unwrap();
    assert!(
        early_status.is_some(),
        "old waiter should exit once DB token changes"
    );

    squad::session::write_token(&workspace.join("sessions"), "worker", &token).unwrap();
    squad(tmp.path())
        .args(["send", "manager", "worker", "new-session-task"])
        .assert()
        .success();

    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Session replaced"));

    squad(tmp.path())
        .args(["receive", "worker"])
        .assert()
        .success()
        .stdout(predicate::str::contains("new-session-task"));
}

#[test]
fn test_rejoin_same_id_receives_old_unread_messages() {
    let tmp = setup_workspace();

    squad(tmp.path())
        .args(["join", "manager"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["send", "manager", "worker", "task-before-leave"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["leave", "worker"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["receive", "worker"])
        .assert()
        .success()
        .stdout(predicate::str::contains("task-before-leave"));
}

/// Clean command removes state
#[test]
fn test_clean_command() {
    let tmp = setup_workspace();

    squad(tmp.path())
        .args(["join", "manager"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["send", "manager", "manager", "test"])
        .assert()
        .success();

    squad(tmp.path()).arg("clean").assert().success();

    // After clean, agents list is empty
    squad(tmp.path())
        .arg("agents")
        .assert()
        .success()
        .stdout(predicate::str::contains("No agents"));
}

/// Multiple agents with same role work independently
#[test]
fn test_multiple_workers_same_role() {
    let tmp = setup_workspace();

    squad(tmp.path())
        .args(["join", "manager"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "worker-1", "--role", "worker"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "worker-2", "--role", "worker"])
        .assert()
        .success();

    // Send different tasks
    squad(tmp.path())
        .args(["send", "manager", "worker-1", "implement login"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["send", "manager", "worker-2", "implement signup"])
        .assert()
        .success();

    // Each gets their own task
    squad(tmp.path())
        .args(["receive", "worker-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("implement login"))
        .stdout(predicate::str::contains("implement signup").not());

    squad(tmp.path())
        .args(["receive", "worker-2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("implement signup"));
}

/// Second join with same ID gets auto-suffixed
#[test]
fn test_second_join_gets_auto_suffix() {
    let tmp = setup_workspace();

    squad(tmp.path())
        .args(["join", "worker", "--role", "worker"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Joined as worker"));

    // Second join gets worker-2, not overwrite
    squad(tmp.path())
        .args(["join", "worker", "--role", "worker"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Joined as worker-2"));

    // Both exist independently
    squad(tmp.path())
        .arg("agents")
        .assert()
        .success()
        .stdout(predicate::str::contains("worker"))
        .stdout(predicate::str::contains("worker-2"));

    // Original worker is not displaced — can still send
    squad(tmp.path())
        .args(["send", "worker", "worker-2", "hello from original"])
        .assert()
        .success();
}

/// Three agents with same base ID all get unique IDs
#[test]
fn test_three_agents_same_base_id() {
    let tmp = setup_workspace();

    squad(tmp.path())
        .args(["join", "member"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Joined as member"));
    squad(tmp.path())
        .args(["join", "member"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Joined as member-2"));
    squad(tmp.path())
        .args(["join", "member"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Joined as member-3"));

    let output = squad(tmp.path()).arg("agents").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("member-2"));
    assert!(stdout.contains("member-3"));
}

/// Pending shows unread messages overview
#[test]
fn test_pending_overview() {
    let tmp = setup_workspace();

    squad(tmp.path())
        .args(["join", "manager"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["join", "worker"])
        .assert()
        .success();

    squad(tmp.path())
        .args(["send", "manager", "worker", "task alpha"])
        .assert()
        .success();
    squad(tmp.path())
        .args(["send", "manager", "worker", "task beta"])
        .assert()
        .success();

    squad(tmp.path())
        .arg("pending")
        .assert()
        .success()
        .stdout(predicate::str::contains("manager -> worker"))
        .stdout(predicate::str::contains("task alpha"));
}
