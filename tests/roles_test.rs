use squad::roles::{default_role_prompt, load_role, BUILTIN_ROLES};
use std::fs;
use tempfile::TempDir;

#[test]
fn test_builtin_roles_exist() {
    assert!(BUILTIN_ROLES.contains(&"manager"));
    assert!(BUILTIN_ROLES.contains(&"worker"));
    assert!(BUILTIN_ROLES.contains(&"inspector"));
}

#[test]
fn test_load_builtin_role() {
    let prompt = default_role_prompt("manager").unwrap();
    assert!(prompt.contains("manager"));
}

#[test]
fn test_builtin_roles_recommend_receive() {
    for role in BUILTIN_ROLES {
        let prompt = default_role_prompt(role).unwrap();
        assert!(prompt.contains("squad receive <your-id>"));
        assert!(prompt.contains("--wait"));
    }
}

#[test]
fn test_manager_role_uses_actual_joined_id_pattern() {
    let prompt = default_role_prompt("manager").unwrap();
    assert!(prompt.contains("squad receive <your-id>"));
    assert!(!prompt.contains("squad receive manager"));
}

#[test]
fn test_load_role_prefers_refreshed_workspace_builtin_file() {
    let tmp = TempDir::new().unwrap();
    squad::init::init_workspace(tmp.path()).unwrap();

    let manager_path = tmp.path().join(".squad").join("roles").join("manager.md");
    fs::write(&manager_path, "outdated manager instructions").unwrap();

    squad::init::init_workspace_with_options(tmp.path(), true).unwrap();

    let prompt = load_role(tmp.path(), "manager").unwrap();
    assert_eq!(prompt, default_role_prompt("manager").unwrap());
    assert_eq!(fs::read_to_string(manager_path).unwrap(), prompt);
}

#[test]
fn test_load_custom_role_from_disk() {
    let tmp = TempDir::new().unwrap();
    let roles_dir = tmp.path().join(".squad").join("roles");
    fs::create_dir_all(&roles_dir).unwrap();
    fs::write(roles_dir.join("custom.md"), "You are a custom agent.").unwrap();
    let prompt = load_role(tmp.path(), "custom").unwrap();
    assert_eq!(prompt, "You are a custom agent.");
}

#[test]
fn test_custom_role_overrides_builtin() {
    let tmp = TempDir::new().unwrap();
    let roles_dir = tmp.path().join(".squad").join("roles");
    fs::create_dir_all(&roles_dir).unwrap();
    fs::write(roles_dir.join("manager.md"), "Custom manager.").unwrap();
    let prompt = load_role(tmp.path(), "manager").unwrap();
    assert_eq!(prompt, "Custom manager.");
}
