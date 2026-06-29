use anyhow::{Context, Result};
use std::path::Path;

pub const BUILTIN_ROLES: &[&str] = &["manager", "worker", "inspector"];

pub fn default_role_prompt(role: &str) -> Option<String> {
    match role {
        "manager" => Some(include_str!("roles/manager.md").to_string()),
        "worker" => Some(include_str!("roles/worker.md").to_string()),
        "inspector" => Some(include_str!("roles/inspector.md").to_string()),
        _ => None,
    }
}

pub fn load_role(workspace: &Path, role: &str) -> Result<String> {
    let custom_path = workspace
        .join(".squad")
        .join("roles")
        .join(format!("{role}.md"));
    if custom_path.exists() {
        return std::fs::read_to_string(&custom_path)
            .with_context(|| format!("failed to read role: {}", custom_path.display()));
    }
    default_role_prompt(role).with_context(|| {
        format!(
            "unknown role: {role}. Available: {}",
            BUILTIN_ROLES.join(", ")
        )
    })
}

pub fn list_roles(workspace: &Path) -> Vec<String> {
    let mut roles: Vec<String> = BUILTIN_ROLES.iter().map(|s| s.to_string()).collect();
    let custom_dir = workspace.join(".squad").join("roles");
    if let Ok(entries) = std::fs::read_dir(&custom_dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.path().file_stem() {
                let name = name.to_string_lossy().to_string();
                if !roles.contains(&name) {
                    roles.push(name);
                }
            }
        }
    }
    roles.sort();
    roles
}
