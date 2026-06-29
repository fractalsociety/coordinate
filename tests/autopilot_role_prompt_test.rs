use squad::autopilot::{
    apply_model_policy_to_role_specs, generate_role_prompt, generated_roles_dir,
    synthesize_role_specs_from_prd, write_generated_roles, AutopilotConfig, ModelMix,
    ModelProvider, PrdRoleContext, RolePromptSpec,
};
use std::collections::BTreeMap;
use tempfile::TempDir;

#[test]
fn test_generate_role_prompt_includes_prd_context_and_role_bounds() {
    let context = PrdRoleContext {
        prd_path: "./PRD.md".to_string(),
        product_goal: "Create an automated AI build manager.".to_string(),
        milestones: vec!["MVP 1: Config + Schema".to_string()],
        implementation_tasks: vec!["Add SQLite migrations".to_string()],
        acceptance_criteria: vec!["Final report is generated".to_string()],
        risky_areas: vec!["Terminal spawning".to_string()],
        likely_files: vec!["src/store.rs".to_string(), "src/main.rs".to_string()],
        test_requirements: vec!["cargo test".to_string()],
    };
    let spec = RolePromptSpec {
        role_id: "sqlite_engineer".to_string(),
        role_name: "SQLite/Data Engineer".to_string(),
        model_provider: ModelProvider::Codex,
        skills_prompt: "Own SQLite schema and persistence changes.".to_string(),
        allowed_files: vec![
            "src/store.rs".to_string(),
            "tests/store_test.rs".to_string(),
        ],
        forbidden_areas: vec!["Do not change tmux launcher behavior".to_string()],
        expected_output_format: vec![
            "Summary".to_string(),
            "Changed files".to_string(),
            "Tests run".to_string(),
        ],
        acceptance_criteria: vec!["Autopilot tables are created idempotently".to_string()],
        approval_triggers: vec!["Schema change affects existing tables".to_string()],
    };

    let prompt = generate_role_prompt(&context, &spec);

    assert!(prompt.contains("## Autopilot Role: SQLite/Data Engineer"));
    assert!(prompt.contains("- Role ID: sqlite_engineer"));
    assert!(prompt.contains("- Model Provider: codex"));
    assert!(prompt.contains("- Source PRD: ./PRD.md"));
    assert!(prompt.contains("Create an automated AI build manager."));
    assert!(prompt.contains("Add SQLite migrations"));
    assert!(prompt.contains("src/store.rs"));
    assert!(prompt.contains("Do not change tmux launcher behavior"));
    assert!(prompt.contains("Autopilot tables are created idempotently"));
    assert!(prompt.contains("Schema change affects existing tables"));
    assert!(prompt.contains("squad receive <your-id> --wait"));
}

#[test]
fn test_generate_role_prompt_marks_missing_sections() {
    let context = PrdRoleContext {
        prd_path: String::new(),
        product_goal: String::new(),
        ..PrdRoleContext::default()
    };
    let spec = RolePromptSpec {
        role_id: "docs".to_string(),
        role_name: "Docs Engineer".to_string(),
        model_provider: ModelProvider::Gemini,
        skills_prompt: String::new(),
        allowed_files: Vec::new(),
        forbidden_areas: Vec::new(),
        expected_output_format: Vec::new(),
        acceptance_criteria: Vec::new(),
        approval_triggers: Vec::new(),
    };

    let prompt = generate_role_prompt(&context, &spec);

    assert!(prompt.contains("- Source PRD: unknown"));
    assert!(prompt.contains("- Product Goal: Not specified"));
    assert!(prompt.contains("Execute assigned work for this PRD."));
    assert!(prompt.contains("- Milestones:\n  - Not specified"));
    assert!(prompt.contains("- Allowed Files and Tools:\n  - Not specified"));
}

#[test]
fn test_synthesize_role_specs_from_prd_uses_smallest_specialized_team() {
    let context = PrdRoleContext {
        prd_path: "./PRD.md".to_string(),
        product_goal: "Create an automated Rust CLI build manager.".to_string(),
        milestones: vec!["MVP 1: Config + SQLite schema".to_string()],
        implementation_tasks: vec![
            "Add squad autopilot run command".to_string(),
            "Persist tasks in SQLite tables".to_string(),
            "Spawn tmux panes for generated workers".to_string(),
            "Generate final report docs".to_string(),
        ],
        acceptance_criteria: vec!["Run `cargo test` before completion".to_string()],
        risky_areas: vec!["Terminal spawning may affect user sessions".to_string()],
        likely_files: vec!["src/main.rs".to_string(), "src/store.rs".to_string()],
        test_requirements: vec!["Add integration tests".to_string()],
    };

    let specs = synthesize_role_specs_from_prd(&context);
    let role_ids: Vec<&str> = specs.iter().map(|spec| spec.role_id.as_str()).collect();

    assert_eq!(
        role_ids,
        vec![
            "manager",
            "inspector",
            "rust_backend",
            "sqlite_engineer",
            "terminal_tmux",
            "test_engineer",
            "security_reviewer",
            "docs",
        ]
    );
    assert_eq!(specs[0].model_provider, ModelProvider::Claude);
    assert_eq!(specs[2].model_provider, ModelProvider::Codex);
    assert_eq!(specs[7].model_provider, ModelProvider::Gemini);
}

#[test]
fn test_synthesize_role_specs_from_prd_keeps_minimal_team_for_generic_prd() {
    let context = PrdRoleContext {
        product_goal: "Improve the product workflow.".to_string(),
        implementation_tasks: vec!["Make the requested change".to_string()],
        ..PrdRoleContext::default()
    };

    let specs = synthesize_role_specs_from_prd(&context);
    let role_ids: Vec<&str> = specs.iter().map(|spec| spec.role_id.as_str()).collect();

    assert_eq!(role_ids, vec!["manager", "inspector", "rust_backend"]);
}

#[test]
fn test_apply_model_policy_to_role_specs_uses_mix_and_role_overrides() {
    let specs = vec![
        RolePromptSpec {
            role_id: "manager".to_string(),
            role_name: "Autopilot Manager".to_string(),
            model_provider: ModelProvider::Claude,
            skills_prompt: "Coordinate work.".to_string(),
            allowed_files: Vec::new(),
            forbidden_areas: Vec::new(),
            expected_output_format: Vec::new(),
            acceptance_criteria: Vec::new(),
            approval_triggers: Vec::new(),
        },
        RolePromptSpec {
            role_id: "rust_backend".to_string(),
            role_name: "Rust Backend Engineer".to_string(),
            model_provider: ModelProvider::Codex,
            skills_prompt: "Implement Rust changes.".to_string(),
            allowed_files: Vec::new(),
            forbidden_areas: Vec::new(),
            expected_output_format: Vec::new(),
            acceptance_criteria: Vec::new(),
            approval_triggers: Vec::new(),
        },
        RolePromptSpec {
            role_id: "docs".to_string(),
            role_name: "Docs Engineer".to_string(),
            model_provider: ModelProvider::Gemini,
            skills_prompt: "Update docs.".to_string(),
            allowed_files: Vec::new(),
            forbidden_areas: Vec::new(),
            expected_output_format: Vec::new(),
            acceptance_criteria: Vec::new(),
            approval_triggers: Vec::new(),
        },
    ];
    let config = AutopilotConfig {
        model_mix: ModelMix {
            claude: 0.0,
            codex: 0.0,
            gemini: 0.0,
            openrouter_free: 0.0,
            openrouter_cheap: 0.0,
            local: 1.0,
        },
        role_overrides: BTreeMap::from([("docs".to_string(), ModelProvider::Gemini)]),
    };

    let planned = apply_model_policy_to_role_specs(&specs, &config);

    assert_eq!(planned[0].model_provider, ModelProvider::Claude);
    assert_eq!(planned[1].model_provider, ModelProvider::Local);
    assert_eq!(planned[2].model_provider, ModelProvider::Gemini);
}

#[test]
fn test_apply_model_policy_splits_generated_roles_evenly_for_claude_codex_mix() {
    let context = PrdRoleContext {
        product_goal: "Design and implement a Rust CLI autopilot workflow.".to_string(),
        implementation_tasks: vec![
            "Architect the generated team model".to_string(),
            "Implement Rust backend commands".to_string(),
            "Persist run state in SQLite".to_string(),
            "Launch terminal tmux sessions".to_string(),
            "Add tests and security review".to_string(),
            "Update docs and release packaging".to_string(),
        ],
        acceptance_criteria: vec!["Run cargo test before completion".to_string()],
        risky_areas: vec!["Security-sensitive terminal spawning".to_string()],
        likely_files: vec!["src/main.rs".to_string(), "src/store.rs".to_string()],
        test_requirements: vec!["Add regression tests".to_string()],
        ..PrdRoleContext::default()
    };
    let specs = synthesize_role_specs_from_prd(&context);
    let config = AutopilotConfig {
        model_mix: ModelMix {
            claude: 0.50,
            codex: 0.50,
            gemini: 0.0,
            openrouter_free: 0.0,
            openrouter_cheap: 0.0,
            local: 0.0,
        },
        role_overrides: BTreeMap::from([
            ("manager".to_string(), ModelProvider::Claude),
            ("inspector".to_string(), ModelProvider::Codex),
        ]),
    };

    let planned = apply_model_policy_to_role_specs(&specs, &config);

    assert_eq!(planned.len(), 10);
    assert_eq!(
        planned
            .iter()
            .filter(|spec| spec.model_provider == ModelProvider::Claude)
            .count(),
        5
    );
    assert_eq!(
        planned
            .iter()
            .filter(|spec| spec.model_provider == ModelProvider::Codex)
            .count(),
        5
    );
    assert!(planned.iter().all(|spec| {
        spec.model_provider == ModelProvider::Claude || spec.model_provider == ModelProvider::Codex
    }));
}

#[test]
fn test_synthesize_role_specs_from_prd_covers_full_delivery_team_categories() {
    let context = PrdRoleContext {
        product_goal: "Plan architecture and implement a Rust CLI autopilot workflow.".to_string(),
        implementation_tasks: vec![
            "Design generated team architecture".to_string(),
            "Implement Rust backend commands".to_string(),
            "Persist task and run state in SQLite data tables".to_string(),
            "Launch terminal tmux sessions".to_string(),
            "Add regression tests and quality coverage".to_string(),
            "Update docs and release packaging".to_string(),
        ],
        acceptance_criteria: vec!["Run cargo test before completion".to_string()],
        risky_areas: vec!["Review security-sensitive terminal spawning".to_string()],
        likely_files: vec!["src/main.rs".to_string(), "src/store.rs".to_string()],
        test_requirements: vec!["Add coverage for generated roles".to_string()],
        ..PrdRoleContext::default()
    };

    let specs = synthesize_role_specs_from_prd(&context);
    let role_ids: Vec<&str> = specs.iter().map(|spec| spec.role_id.as_str()).collect();

    assert!(role_ids.contains(&"manager"));
    assert!(role_ids.contains(&"inspector"));
    assert!(role_ids.contains(&"architect"));
    assert!(role_ids.contains(&"rust_backend"));
    assert!(role_ids.contains(&"sqlite_engineer"));
    assert!(role_ids.contains(&"terminal_tmux"));
    assert!(role_ids.contains(&"test_engineer"));
    assert!(role_ids.contains(&"docs"));
    assert!(role_ids.contains(&"release_engineer"));
    assert!(role_ids.contains(&"security_reviewer"));
}

#[test]
fn test_write_generated_roles_saves_prompts_under_generated_roles_dir() {
    let tmp = TempDir::new().unwrap();
    let context = PrdRoleContext {
        prd_path: "./PRD.md".to_string(),
        product_goal: "Create an automated AI build manager.".to_string(),
        implementation_tasks: vec!["Save generated role prompts".to_string()],
        ..PrdRoleContext::default()
    };
    let specs = vec![
        RolePromptSpec {
            role_id: "manager".to_string(),
            role_name: "Autopilot Manager".to_string(),
            model_provider: ModelProvider::Claude,
            skills_prompt: "Coordinate the build.".to_string(),
            allowed_files: vec![".".to_string()],
            forbidden_areas: Vec::new(),
            expected_output_format: vec!["Summary".to_string()],
            acceptance_criteria: vec!["Tasks are assigned".to_string()],
            approval_triggers: Vec::new(),
        },
        RolePromptSpec {
            role_id: "docs_engineer".to_string(),
            role_name: "Docs Engineer".to_string(),
            model_provider: ModelProvider::Gemini,
            skills_prompt: "Update docs.".to_string(),
            allowed_files: vec!["README.md".to_string()],
            forbidden_areas: Vec::new(),
            expected_output_format: vec!["Changed files".to_string()],
            acceptance_criteria: vec!["Docs mention autopilot".to_string()],
            approval_triggers: Vec::new(),
        },
    ];

    let generated_roles = write_generated_roles(tmp.path(), &context, &specs).unwrap();

    assert_eq!(generated_roles.len(), 2);
    assert_eq!(generated_roles[0].role_id, "manager");
    assert_eq!(generated_roles[0].prompt_file, "generated/manager");
    assert_eq!(generated_roles[1].role_id, "docs_engineer");
    assert_eq!(generated_roles[1].prompt_file, "generated/docs_engineer");

    let roles_dir = generated_roles_dir(tmp.path());
    assert_eq!(
        roles_dir,
        tmp.path().join(".squad").join("roles").join("generated")
    );
    let manager_prompt = std::fs::read_to_string(roles_dir.join("manager.md")).unwrap();
    assert!(manager_prompt.contains("## Autopilot Role: Autopilot Manager"));
    assert!(manager_prompt.contains("Save generated role prompts"));
    let docs_prompt = std::fs::read_to_string(roles_dir.join("docs_engineer.md")).unwrap();
    assert!(docs_prompt.contains("## Autopilot Role: Docs Engineer"));
    assert!(docs_prompt.contains("README.md"));
}

#[test]
fn test_write_generated_roles_rejects_empty_and_path_like_role_ids() {
    let tmp = TempDir::new().unwrap();
    let context = PrdRoleContext::default();

    let empty_error = write_generated_roles(tmp.path(), &context, &[])
        .unwrap_err()
        .to_string();
    assert!(empty_error.contains("generated role list cannot be empty"));

    let spec = RolePromptSpec {
        role_id: "../manager".to_string(),
        role_name: "Manager".to_string(),
        model_provider: ModelProvider::Claude,
        skills_prompt: String::new(),
        allowed_files: Vec::new(),
        forbidden_areas: Vec::new(),
        expected_output_format: Vec::new(),
        acceptance_criteria: Vec::new(),
        approval_triggers: Vec::new(),
    };
    let path_error = write_generated_roles(tmp.path(), &context, &[spec])
        .unwrap_err()
        .to_string();
    assert!(path_error.contains("generated role id cannot contain path separators"));
}
