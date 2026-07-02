use squad::autopilot::{autopilot_team_path, write_autopilot_team, GeneratedTeamRole};
use tempfile::TempDir;

#[test]
fn test_write_autopilot_team_creates_loadable_team_yaml() {
    let tmp = TempDir::new().unwrap();
    let roles = vec![
        GeneratedTeamRole {
            role_id: "manager".to_string(),
            prompt_file: "generated/manager".to_string(),
        },
        GeneratedTeamRole {
            role_id: "sqlite_engineer".to_string(),
            prompt_file: "generated/sqlite_engineer".to_string(),
        },
        GeneratedTeamRole {
            role_id: "docs".to_string(),
            prompt_file: "generated/docs".to_string(),
        },
    ];

    let path = write_autopilot_team(tmp.path(), &roles).unwrap();

    assert_eq!(path, autopilot_team_path(tmp.path()));
    assert_eq!(
        path,
        tmp.path()
            .join(".squad")
            .join("teams")
            .join("autopilot.yaml")
    );

    let team = squad::teams::load_team(tmp.path(), "autopilot").unwrap();
    assert_eq!(team.name, "autopilot");
    assert_eq!(
        team.roles.get("manager").unwrap().prompt_file,
        "generated/manager"
    );
    assert_eq!(
        team.roles.get("sqlite_engineer").unwrap().prompt_file,
        "generated/sqlite_engineer"
    );
    assert_eq!(
        team.roles.get("docs").unwrap().prompt_file,
        "generated/docs"
    );
}

#[test]
fn test_write_autopilot_team_rejects_empty_role_list() {
    let tmp = TempDir::new().unwrap();

    let error = write_autopilot_team(tmp.path(), &[])
        .unwrap_err()
        .to_string();

    assert!(error.contains("must include at least one role"));
}

#[test]
fn test_write_autopilot_team_rejects_duplicate_role_ids() {
    let tmp = TempDir::new().unwrap();
    let roles = vec![
        GeneratedTeamRole {
            role_id: "worker".to_string(),
            prompt_file: "generated/worker".to_string(),
        },
        GeneratedTeamRole {
            role_id: "worker".to_string(),
            prompt_file: "generated/worker_2".to_string(),
        },
    ];

    let error = write_autopilot_team(tmp.path(), &roles)
        .unwrap_err()
        .to_string();

    assert!(error.contains("duplicate autopilot team role id: worker"));
}
