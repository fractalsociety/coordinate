use squad::autopilot::{
    init_autopilot_workspace, load_config, plan_manager_pane, plan_terminal_sessions,
    plan_worker_panes, provider_tool_command, render_macos_terminal_commands,
    render_tmux_spawn_commands, role_specific_injection_text, AutopilotConfig, GeneratedTeamRole,
    ModelProvider, TerminalKind, TerminalSessionRole, TerminalSessionStatus,
};
use std::collections::BTreeMap;
use tempfile::TempDir;

#[test]
fn test_plan_manager_pane_defaults_to_claude_tmux_pane() {
    let tmp = TempDir::new().unwrap();

    let pane = plan_manager_pane(tmp.path(), &AutopilotConfig::default());

    assert_eq!(pane.agent_id, "manager");
    assert_eq!(pane.role_id, "manager");
    assert_eq!(pane.session_role, TerminalSessionRole::Manager);
    assert_eq!(pane.model_provider, ModelProvider::Claude);
    assert_eq!(pane.terminal_kind, TerminalKind::Tmux);
    assert_eq!(pane.pane_label, "manager");
    assert_eq!(pane.command, "claude --dangerously-skip-permissions");
    assert_eq!(pane.provider_tool.program, "claude");
    assert_eq!(
        pane.provider_tool.args,
        vec!["--dangerously-skip-permissions".to_string()]
    );
    assert_eq!(pane.working_dir, tmp.path().display().to_string());
    assert_eq!(pane.inject_text, "/squad manager manager");
    assert_eq!(pane.status, TerminalSessionStatus::Planned);
}

#[test]
fn test_plan_manager_pane_honors_role_override() {
    let tmp = TempDir::new().unwrap();
    let config = AutopilotConfig {
        role_overrides: BTreeMap::from([("manager".to_string(), ModelProvider::OpenCode)]),
        ..AutopilotConfig::default()
    };

    let pane = plan_manager_pane(tmp.path(), &config);

    assert_eq!(pane.model_provider, ModelProvider::OpenCode);
    assert_eq!(pane.command, "opencode");
    assert_eq!(pane.provider_tool.program, "opencode");
    assert_eq!(pane.inject_text, "/squad manager manager");
}

#[test]
fn test_plan_worker_panes_uses_generated_worker_roles_only() {
    let tmp = TempDir::new().unwrap();
    let roles = vec![
        GeneratedTeamRole {
            role_id: "manager".to_string(),
            prompt_file: "generated/manager".to_string(),
        },
        GeneratedTeamRole {
            role_id: "rust_backend".to_string(),
            prompt_file: "generated/rust_backend".to_string(),
        },
        GeneratedTeamRole {
            role_id: "inspector".to_string(),
            prompt_file: "generated/inspector".to_string(),
        },
        GeneratedTeamRole {
            role_id: "docs".to_string(),
            prompt_file: "generated/docs".to_string(),
        },
    ];
    let config = AutopilotConfig {
        role_overrides: BTreeMap::from([("docs".to_string(), ModelProvider::Gemini)]),
        ..AutopilotConfig::default()
    };

    let panes = plan_worker_panes(tmp.path(), &roles, &config).unwrap();

    assert_eq!(panes.len(), 2);
    assert_eq!(panes[0].agent_id, "rust_backend");
    assert_eq!(panes[0].role_id, "rust_backend");
    assert_eq!(panes[0].session_role, TerminalSessionRole::Worker);
    assert_eq!(panes[0].model_provider, ModelProvider::Codex);
    assert_eq!(panes[0].terminal_kind, TerminalKind::Tmux);
    assert_eq!(panes[0].pane_label, "rust_backend");
    assert_eq!(panes[0].command, "codex --yolo");
    assert_eq!(panes[0].provider_tool.program, "codex");
    assert_eq!(panes[0].provider_tool.args, vec!["--yolo".to_string()]);
    assert_eq!(panes[0].inject_text, "$squad rust_backend rust_backend");
    assert_eq!(panes[0].working_dir, tmp.path().display().to_string());
    assert_eq!(panes[0].status, TerminalSessionStatus::Planned);

    assert_eq!(panes[1].agent_id, "docs");
    assert_eq!(panes[1].session_role, TerminalSessionRole::Worker);
    assert_eq!(panes[1].model_provider, ModelProvider::Gemini);
    assert_eq!(panes[1].command, "gemini");
    assert_eq!(panes[1].provider_tool.program, "gemini");
    assert_eq!(panes[1].inject_text, "/squad docs docs");
}

#[test]
fn test_plan_worker_panes_rejects_team_without_generated_workers() {
    let tmp = TempDir::new().unwrap();
    let roles = vec![
        GeneratedTeamRole {
            role_id: "manager".to_string(),
            prompt_file: "generated/manager".to_string(),
        },
        GeneratedTeamRole {
            role_id: "inspector".to_string(),
            prompt_file: "generated/inspector".to_string(),
        },
    ];

    let error = plan_worker_panes(tmp.path(), &roles, &AutopilotConfig::default())
        .unwrap_err()
        .to_string();

    assert!(error.contains("must include at least one generated worker role"));
}

#[test]
fn test_plan_terminal_sessions_uses_role_overrides_and_provider_commands() {
    let tmp = TempDir::new().unwrap();
    let roles = vec![
        GeneratedTeamRole {
            role_id: "manager".to_string(),
            prompt_file: "generated/manager".to_string(),
        },
        GeneratedTeamRole {
            role_id: "rust_backend".to_string(),
            prompt_file: "generated/rust_backend".to_string(),
        },
        GeneratedTeamRole {
            role_id: "inspector".to_string(),
            prompt_file: "generated/inspector".to_string(),
        },
        GeneratedTeamRole {
            role_id: "docs".to_string(),
            prompt_file: "generated/docs".to_string(),
        },
        GeneratedTeamRole {
            role_id: "terminal".to_string(),
            prompt_file: "generated/terminal".to_string(),
        },
    ];
    let config = AutopilotConfig {
        role_overrides: BTreeMap::from([
            ("manager".to_string(), ModelProvider::Claude),
            ("inspector".to_string(), ModelProvider::Claude),
            ("docs".to_string(), ModelProvider::Gemini),
            ("terminal".to_string(), ModelProvider::OpenCode),
        ]),
        ..AutopilotConfig::default()
    };

    let sessions = plan_terminal_sessions(tmp.path(), &roles, &config).unwrap();

    assert_eq!(sessions.len(), 5);
    assert_eq!(sessions[0].agent_id, "manager");
    assert_eq!(sessions[0].role_id, "manager");
    assert_eq!(sessions[0].session_role, TerminalSessionRole::Manager);
    assert_eq!(sessions[0].model_provider, ModelProvider::Claude);
    assert_eq!(sessions[0].terminal_kind, TerminalKind::Tmux);
    assert_eq!(sessions[0].pane_label, "manager");
    assert_eq!(sessions[0].command, "claude --dangerously-skip-permissions");
    assert_eq!(sessions[0].provider_tool.program, "claude");
    assert_eq!(sessions[0].inject_text, "/squad manager manager");
    assert_eq!(sessions[0].status, TerminalSessionStatus::Planned);
    assert_eq!(sessions[0].working_dir, tmp.path().display().to_string());

    assert_eq!(sessions[1].model_provider, ModelProvider::Codex);
    assert_eq!(sessions[1].session_role, TerminalSessionRole::Worker);
    assert_eq!(sessions[1].command, "codex --yolo");
    assert_eq!(sessions[1].provider_tool.program, "codex");
    assert_eq!(sessions[1].inject_text, "$squad rust_backend rust_backend");

    assert_eq!(sessions[2].agent_id, "inspector");
    assert_eq!(sessions[2].role_id, "inspector");
    assert_eq!(sessions[2].session_role, TerminalSessionRole::Inspector);
    assert_eq!(sessions[2].model_provider, ModelProvider::Claude);
    assert_eq!(sessions[2].command, "claude --dangerously-skip-permissions");
    assert_eq!(sessions[2].provider_tool.program, "claude");
    assert_eq!(sessions[2].inject_text, "/squad inspector inspector");

    assert_eq!(sessions[3].model_provider, ModelProvider::Gemini);
    assert_eq!(sessions[3].session_role, TerminalSessionRole::Worker);
    assert_eq!(sessions[3].command, "gemini");
    assert_eq!(sessions[3].provider_tool.program, "gemini");
    assert_eq!(sessions[3].inject_text, "/squad docs docs");

    assert_eq!(sessions[4].model_provider, ModelProvider::OpenCode);
    assert_eq!(sessions[4].session_role, TerminalSessionRole::Worker);
    assert_eq!(sessions[4].command, "opencode");
    assert_eq!(sessions[4].provider_tool.program, "opencode");
    assert_eq!(sessions[4].inject_text, "/squad terminal terminal");
}

#[test]
fn test_render_tmux_spawn_commands_covers_manager_and_generated_workers() {
    let tmp = TempDir::new().unwrap();
    let roles = vec![
        GeneratedTeamRole {
            role_id: "manager".to_string(),
            prompt_file: "generated/manager".to_string(),
        },
        GeneratedTeamRole {
            role_id: "rust_backend".to_string(),
            prompt_file: "generated/rust_backend".to_string(),
        },
    ];
    let sessions = plan_terminal_sessions(tmp.path(), &roles, &AutopilotConfig::default()).unwrap();

    let commands = render_tmux_spawn_commands("autopilot", &sessions).unwrap();

    assert_eq!(commands.len(), 4);
    assert!(commands[0].contains("tmux new-window"));
    assert!(commands[0].contains("-n 'manager'"));
    assert!(commands[0].contains("'claude --dangerously-skip-permissions'"));
    assert!(commands[1].contains("tmux send-keys"));
    assert!(commands[1].contains("'/squad manager manager'"));
    assert!(commands[2].contains("-n 'rust_backend'"));
    assert!(commands[2].contains("'codex --yolo'"));
    assert!(commands[3].contains("'$squad rust_backend rust_backend'"));

    let error = render_tmux_spawn_commands(" ", &sessions)
        .unwrap_err()
        .to_string();
    assert!(error.contains("tmux session name cannot be empty"));
}

#[test]
fn test_render_macos_terminal_commands_opens_physical_terminal_windows() {
    let tmp = TempDir::new().unwrap();
    let roles = vec![
        GeneratedTeamRole {
            role_id: "manager".to_string(),
            prompt_file: "generated/manager".to_string(),
        },
        GeneratedTeamRole {
            role_id: "rust_backend".to_string(),
            prompt_file: "generated/rust_backend".to_string(),
        },
    ];
    let sessions = plan_terminal_sessions(tmp.path(), &roles, &AutopilotConfig::default()).unwrap();

    let commands = render_macos_terminal_commands("autopilot", &sessions).unwrap();

    assert_eq!(commands.len(), 2);
    assert!(commands[0].contains("osascript -e"));
    assert!(commands[0].contains("tell application"));
    assert!(commands[0].contains("Terminal"));
    assert!(commands[0].contains("autopilot - manager"));
    assert!(commands[0].contains("claude --dangerously-skip-permissions"));
    assert!(commands[0].contains("delay 8"));
    assert!(commands[0].contains("/squad manager manager"));
    assert!(commands[0].contains("do script \"\" in autopilotTab"));
    assert!(commands[0].contains("key code 36"));
    assert!(commands[1].contains("autopilot - rust_backend"));
    assert!(commands[1].contains("codex --yolo"));
    assert!(commands[1].contains("delay 45"));
    assert!(commands[1].contains("$squad rust_backend rust_backend"));
    assert!(commands[1].contains("key code 36"));

    let error = render_macos_terminal_commands(" ", &sessions)
        .unwrap_err()
        .to_string();
    assert!(error.contains("macOS Terminal title cannot be empty"));
}

#[test]
fn test_terminal_session_plan_serializes_status_and_terminal_kind() {
    let tmp = TempDir::new().unwrap();
    let roles = vec![GeneratedTeamRole {
        role_id: "security_reviewer".to_string(),
        prompt_file: "generated/security_reviewer".to_string(),
    }];
    let config = AutopilotConfig {
        role_overrides: BTreeMap::from([("security_reviewer".to_string(), ModelProvider::Claude)]),
        ..AutopilotConfig::default()
    };

    let sessions = plan_terminal_sessions(tmp.path(), &roles, &config).unwrap();
    let json = serde_json::to_string_pretty(&sessions[0]).unwrap();

    assert!(json.contains(r#""terminal_kind": "tmux""#));
    assert!(json.contains(r#""session_role": "worker""#));
    assert!(json.contains(r#""provider_tool""#));
    assert!(json.contains(r#""program": "claude""#));
    assert!(json.contains(r#""--dangerously-skip-permissions""#));
    assert!(json.contains(r#""status": "planned""#));
    assert!(json.contains(r#""model_provider": "claude""#));
}

#[test]
fn test_provider_tool_command_maps_supported_provider_tools() {
    assert_eq!(
        provider_tool_command(&ModelProvider::Claude).shell_command(),
        "claude --dangerously-skip-permissions"
    );
    assert_eq!(
        provider_tool_command(&ModelProvider::Codex).shell_command(),
        "codex --yolo"
    );
    assert_eq!(
        provider_tool_command(&ModelProvider::Gemini).shell_command(),
        "gemini"
    );
    assert_eq!(
        provider_tool_command(&ModelProvider::OpenCode).shell_command(),
        "opencode"
    );
    assert_eq!(
        provider_tool_command(&ModelProvider::OpenRouterFree).shell_command(),
        "opencode --model openrouter/free"
    );
    assert_eq!(
        provider_tool_command(&ModelProvider::OpenRouterCheap).shell_command(),
        "opencode --model openrouter/cheap"
    );
}

#[test]
fn test_role_specific_injection_text_uses_provider_prompt_style() {
    assert_eq!(
        role_specific_injection_text(&ModelProvider::Codex, "rust_backend", "rust_backend"),
        "$squad rust_backend rust_backend"
    );
    assert_eq!(
        role_specific_injection_text(&ModelProvider::Claude, "manager", "manager"),
        "/squad manager manager"
    );
    assert_eq!(
        role_specific_injection_text(&ModelProvider::Gemini, "docs", "docs"),
        "/squad docs docs"
    );
    assert_eq!(
        role_specific_injection_text(&ModelProvider::OpenCode, "inspector", "inspector"),
        "/squad inspector inspector"
    );
}

#[test]
fn test_plan_terminal_sessions_rejects_empty_role_list() {
    let tmp = TempDir::new().unwrap();

    let error = plan_terminal_sessions(tmp.path(), &[], &AutopilotConfig::default())
        .unwrap_err()
        .to_string();

    assert!(error.contains("must include at least one role"));
}

#[test]
fn test_fresh_default_config_yields_claude_codex_only_mix() {
    // The shipped default mix is Claude/Codex only (50/50); OpenRouter, Gemini,
    // and local providers are all disabled, so a fresh config never schedules
    // them for the standard science-swarm roster.
    let tmp = TempDir::new().unwrap();
    init_autopilot_workspace(tmp.path()).unwrap();
    let config = load_config(tmp.path()).unwrap();

    let roles = [
        "manager",
        "inspector",
        "scientific_planner",
        "protocol_designer",
        "literature_worker",
        "hypothesis_worker",
        "tool_mapper",
        "coding_worker",
        "verification_worker",
        "adversarial_critic",
        "safety_gatekeeper",
        "trace_collector",
    ]
    .into_iter()
    .map(|role_id| GeneratedTeamRole {
        role_id: role_id.to_string(),
        prompt_file: format!("generated/{role_id}"),
    })
    .collect::<Vec<_>>();

    let sessions = plan_terminal_sessions(tmp.path(), &roles, &config).unwrap();

    assert_eq!(
        sessions.len(),
        12,
        "standard science swarm team has 12 sessions"
    );

    let count = |provider: ModelProvider| {
        sessions
            .iter()
            .filter(|s| s.model_provider == provider)
            .count()
    };

    assert_eq!(
        count(ModelProvider::Gemini),
        0,
        "Gemini must be disabled on a fresh config"
    );
    assert_eq!(count(ModelProvider::OpenRouterFree), 0);
    assert_eq!(count(ModelProvider::OpenRouterCheap), 0);
    assert_eq!(count(ModelProvider::Local), 0);
    assert_eq!(count(ModelProvider::Claude), 8);
    assert_eq!(count(ModelProvider::Codex), 4);
}

#[test]
fn test_role_overrides_plan_openrouter_and_codex_worker_sessions() {
    let tmp = TempDir::new().unwrap();
    let config = AutopilotConfig {
        role_overrides: BTreeMap::from([
            ("low_risk_worker".to_string(), ModelProvider::OpenRouterFree),
            ("medium_worker".to_string(), ModelProvider::OpenRouterCheap),
            ("coding_worker".to_string(), ModelProvider::Codex),
        ]),
        ..AutopilotConfig::default()
    };
    let roles = ["low_risk_worker", "medium_worker", "coding_worker"]
        .into_iter()
        .map(|role_id| GeneratedTeamRole {
            role_id: role_id.to_string(),
            prompt_file: format!("generated/{role_id}.md"),
        })
        .collect::<Vec<_>>();

    let sessions = plan_terminal_sessions(tmp.path(), &roles, &config).unwrap();

    let low = sessions
        .iter()
        .find(|session| session.role_id == "low_risk_worker")
        .unwrap();
    assert_eq!(low.model_provider, ModelProvider::OpenRouterFree);
    assert_eq!(low.command, "opencode --model openrouter/free");
    assert_eq!(low.session_role, TerminalSessionRole::Worker);

    let medium = sessions
        .iter()
        .find(|session| session.role_id == "medium_worker")
        .unwrap();
    assert_eq!(medium.model_provider, ModelProvider::OpenRouterCheap);
    assert_eq!(medium.command, "opencode --model openrouter/cheap");

    let coding = sessions
        .iter()
        .find(|session| session.role_id == "coding_worker")
        .unwrap();
    assert_eq!(coding.model_provider, ModelProvider::Codex);
    assert_eq!(coding.command, "codex --yolo");
    assert_eq!(
        coding.inject_text,
        "$squad coding_worker coding_worker".to_string()
    );
}
