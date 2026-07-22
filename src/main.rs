use anyhow::{bail, Context, Result};
use chrono::TimeZone;
use fs2::FileExt;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

const DEFAULT_WAIT_TIMEOUT_SECS: u64 = 3_600;
const DEFAULT_AUTOPILOT_ASSIGN_WAIT_SECS: u64 = 180;
const DEFAULT_AUTOPILOT_TASKS_PER_ROLE: usize = 1;

#[derive(Default)]
struct JoinOptions {
    role: Option<String>,
    client_type: Option<String>,
    protocol_version: Option<i64>,
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "help".to_string());

    match command.as_str() {
        "init" => cmd_init(args.collect()),
        "join" => {
            let id = args.next().unwrap_or_default();
            if id.is_empty() {
                bail!("Usage: squad join <id> [--role <role>] [--client <claude|gemini|codex|opencode>] [--protocol-version <n>]");
            }
            let options = parse_join_args(&id, args.collect())?;
            cmd_join(&id, &options)
        }
        "leave" => {
            let id = args.next().unwrap_or_default();
            if id.is_empty() {
                bail!("Usage: squad leave <id>");
            }
            cmd_leave(&id)
        }
        "agents" => {
            let (show_all, json) = parse_agents_args(args.collect())?;
            cmd_agents(show_all, json)
        }
        "send" => {
            let options = parse_send_args(args.collect())?;
            cmd_send(&options)
        }
        "receive" => {
            let id = args.next().unwrap_or_default();
            if id.is_empty() {
                bail!("Usage: squad receive <id> [--wait [--timeout <secs>]] [--json]");
            }
            let mut wait = false;
            let mut json = false;
            let mut timeout_secs: u64 = DEFAULT_WAIT_TIMEOUT_SECS;
            let mut timeout_provided = false;
            let extra: Vec<String> = args.collect();
            let mut i = 0;
            while i < extra.len() {
                match extra[i].as_str() {
                    "--wait" => {
                        wait = true;
                        i += 1;
                    }
                    "--timeout" => {
                        if let Some(val) = extra.get(i + 1) {
                            timeout_secs = val
                                .parse()
                                .with_context(|| format!("invalid --timeout value: {val}"))?;
                            timeout_provided = true;
                        } else {
                            bail!("--timeout requires a value");
                        }
                        i += 2;
                    }
                    "--json" => {
                        json = true;
                        i += 1;
                    }
                    flag => bail!("unknown receive flag: {flag}"),
                }
            }
            if timeout_provided && !wait {
                bail!("--timeout requires --wait");
            }
            cmd_receive(&id, wait, timeout_secs, json)
        }
        "task" => cmd_task(args.collect()),
        "autopilot" | "swarm" => cmd_autopilot(args.collect()),
        "pending" => cmd_pending(),
        "history" => {
            let options = parse_history_args(args.collect())?;
            cmd_history(&options)
        }
        "roles" => cmd_roles(),
        "teams" => cmd_teams(),
        "team" => {
            let name = args.next().unwrap_or_default();
            if name.is_empty() {
                bail!("Usage: squad team <name>");
            }
            cmd_team(&name)
        }
        "doctor" => cmd_doctor(),
        "setup" => {
            let target = args.next();
            cmd_setup(target.as_deref())
        }
        "clean" => cmd_clean(),
        "cleanup" => cmd_cleanup(),
        "help" | "--help" | "-h" => {
            print_usage();
            Ok(())
        }
        "--version" | "-V" => {
            println!("squad {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        // Treat unknown commands as role-based join: `squad cto` = `squad join cto --role cto`
        other => cmd_join(
            other,
            &JoinOptions {
                role: Some(other.to_string()),
                ..JoinOptions::default()
            },
        ),
    }
}

// --- Helpers ---

struct SendOptions {
    from: String,
    to: String,
    message: String,
    task_id: Option<String>,
    reply_to: Option<i64>,
}

fn parse_join_args(id: &str, args: Vec<String>) -> Result<JoinOptions> {
    let mut options = JoinOptions {
        role: Some(id.to_string()),
        ..JoinOptions::default()
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--role" => {
                let value = args.get(i + 1).context("--role requires a value")?;
                if value.starts_with("--") {
                    bail!("--role requires a value");
                }
                options.role = Some(value.clone());
                i += 2;
            }
            "--client" => {
                let value = args.get(i + 1).context("--client requires a value")?;
                if value.starts_with("--") {
                    bail!("--client requires a value");
                }
                match value.as_str() {
                    "claude" | "gemini" | "codex" | "opencode" => {
                        options.client_type = Some(value.clone())
                    }
                    _ => bail!("invalid --client value: {value}"),
                }
                i += 2;
            }
            "--protocol-version" => {
                let value = args
                    .get(i + 1)
                    .context("--protocol-version requires a value")?;
                if value.starts_with("--") {
                    bail!("--protocol-version requires a value");
                }
                options.protocol_version = Some(
                    value
                        .parse()
                        .with_context(|| format!("invalid --protocol-version value: {value}"))?,
                );
                i += 2;
            }
            flag => bail!("unknown join flag: {flag}"),
        }
    }
    Ok(options)
}

fn parse_agents_args(args: Vec<String>) -> Result<(bool, bool)> {
    let mut show_all = false;
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--all" => show_all = true,
            "--json" => json = true,
            _ => bail!("Usage: squad agents [--all] [--json]"),
        }
    }
    Ok((show_all, json))
}

fn parse_send_args(args: Vec<String>) -> Result<SendOptions> {
    let mut task_id = None;
    let mut reply_to = None;
    let mut file_path = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--task-id" => {
                let value = args.get(i + 1).context("--task-id requires a value")?;
                task_id = Some(value.clone());
                i += 2;
            }
            "--reply-to" => {
                let value = args
                    .get(i + 1)
                    .context("--reply-to requires a message id")?;
                reply_to = Some(
                    value
                        .parse()
                        .with_context(|| format!("invalid --reply-to value: {value}"))?,
                );
                i += 2;
            }
            "--file" => {
                let value = args.get(i + 1).context("--file requires a path or -")?;
                file_path = Some(value.clone());
                i += 2;
            }
            _ => break,
        }
    }

    let remaining = &args[i..];
    let usage = "Usage: squad send [--task-id <id>] [--reply-to <message-id>] <from> <to> <message>\n   or: squad send [--task-id <id>] [--reply-to <message-id>] --file <path-or-> <from> <to>";

    if let Some(path) = file_path {
        if remaining.len() != 2 {
            bail!("{usage}");
        }
        let message = read_send_content(&path)?;
        return Ok(SendOptions {
            from: remaining[0].clone(),
            to: remaining[1].clone(),
            message,
            task_id,
            reply_to,
        });
    }

    if remaining.len() < 3 {
        bail!("{usage}");
    }

    let message = remaining[2..].join(" ");
    if message.is_empty() {
        bail!("{usage}");
    }

    Ok(SendOptions {
        from: remaining[0].clone(),
        to: remaining[1].clone(),
        message,
        task_id,
        reply_to,
    })
}

#[derive(Default)]
struct TaskListOptions {
    assigned_to: Option<String>,
    status: Option<String>,
}

#[derive(Serialize)]
struct ReceiveEnvelope {
    id: i64,
    from: String,
    to: String,
    content: String,
    created_at: i64,
    read: bool,
    kind: String,
    task_id: Option<String>,
    reply_to: Option<i64>,
    task: Option<squad::tasks::TaskRecord>,
}

#[derive(Serialize)]
struct AgentEnvelope {
    id: String,
    role: String,
    joined_at: i64,
    last_seen: Option<i64>,
    status: String,
    archived_at: Option<i64>,
    client_type_raw: Option<String>,
    protocol_version_raw: Option<i64>,
    effective_client_type: String,
    effective_protocol_version: i64,
    supports_task_commands: bool,
    supports_json_receive: bool,
}

#[derive(Default)]
struct HistoryOptions {
    agent: Option<String>,
    from: Option<String>,
    to: Option<String>,
    since: Option<i64>,
}

fn parse_history_args(args: Vec<String>) -> Result<HistoryOptions> {
    let mut options = HistoryOptions::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--from" => {
                let value = args.get(i + 1).context("--from requires an agent ID")?;
                options.from = Some(value.clone());
                i += 2;
            }
            "--to" => {
                let value = args.get(i + 1).context("--to requires an agent ID")?;
                options.to = Some(value.clone());
                i += 2;
            }
            "--since" => {
                let value = args
                    .get(i + 1)
                    .context("--since requires an RFC3339 timestamp or unix seconds")?;
                options.since = Some(parse_since(value)?);
                i += 2;
            }
            value if value.starts_with("--") => {
                bail!("unknown history flag: {value}");
            }
            value => {
                if options.agent.is_some() {
                    bail!("Usage: squad history [agent] [--from <id>] [--to <id>] [--since <RFC3339|unix-seconds>]");
                }
                options.agent = Some(value.to_string());
                i += 1;
            }
        }
    }
    Ok(options)
}

fn parse_since(value: &str) -> Result<i64> {
    if let Ok(ts) = value.parse::<i64>() {
        return Ok(ts);
    }
    let dt = chrono::DateTime::parse_from_rfc3339(value)
        .with_context(|| format!("invalid --since value: {value}"))?;
    Ok(dt.timestamp())
}

fn format_history_timestamp(timestamp: i64) -> String {
    chrono::Utc
        .timestamp_opt(timestamp, 0)
        .single()
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_else(|| timestamp.to_string())
}

fn format_history_entry(msg: &squad::store::MessageRecord) -> String {
    let marker = if msg.read { "  " } else { "* " };
    let prefix = format!(
        "{marker}[{}] {} -> {}: ",
        format_history_timestamp(msg.created_at),
        msg.from_agent,
        msg.to_agent
    );
    let mut lines = msg.content.lines();
    let first = lines.next().unwrap_or_default();
    let mut rendered = format!("{prefix}{first}");
    for line in lines {
        rendered.push('\n');
        rendered.push_str("  | ");
        rendered.push_str(line);
    }
    rendered
}

fn effective_client_type(agent: &squad::store::AgentRecord) -> &str {
    agent.client_type_raw.as_deref().unwrap_or("unknown")
}

fn effective_protocol_version(agent: &squad::store::AgentRecord) -> i64 {
    agent
        .protocol_version_raw
        .unwrap_or(squad::setup::DEFAULT_PROTOCOL_VERSION)
}

fn supports_capability(agent: &squad::store::AgentRecord) -> bool {
    effective_protocol_version(agent) >= 2
}

fn agent_envelope(agent: &squad::store::AgentRecord) -> AgentEnvelope {
    AgentEnvelope {
        id: agent.id.clone(),
        role: agent.role.clone(),
        joined_at: agent.joined_at,
        last_seen: agent.last_seen,
        status: agent.status.clone(),
        archived_at: agent.archived_at,
        client_type_raw: agent.client_type_raw.clone(),
        protocol_version_raw: agent.protocol_version_raw,
        effective_client_type: effective_client_type(agent).to_string(),
        effective_protocol_version: effective_protocol_version(agent),
        supports_task_commands: supports_capability(agent),
        supports_json_receive: supports_capability(agent),
    }
}

fn read_send_content(path: &str) -> Result<String> {
    let content = if path == "-" {
        let mut stdin = std::io::stdin();
        let mut content = String::new();
        use std::io::Read;
        stdin.read_to_string(&mut content)?;
        content
    } else {
        std::fs::read_to_string(path)
            .with_context(|| format!("failed to read message file: {path}"))?
    };
    if content.is_empty() {
        bail!("message content is empty");
    }
    Ok(content)
}

fn find_workspace() -> Result<PathBuf> {
    let mut dir = std::env::current_dir()?;
    loop {
        if dir.join(".squad").exists() {
            return Ok(dir);
        }
        if !dir.pop() {
            bail!("Not a squad workspace. Run 'squad init' first.");
        }
    }
}

fn open_store(workspace: &Path) -> Result<squad::store::Store> {
    let db_path = workspace.join(".squad").join("messages.db");
    squad::store::Store::open(&db_path)
}

fn ensure_agent_exists(store: &squad::store::Store, id: &str) -> Result<()> {
    store.require_active_agent(id)
}

fn sessions_dir(workspace: &Path) -> PathBuf {
    workspace.join(".squad").join("sessions")
}

/// Check if this agent's session is still valid. Returns Ok(()) if valid or if
/// no session tracking exists (backward compat). Errors with "Session replaced" if displaced.
fn check_session(workspace: &Path, store: &squad::store::Store, agent_id: &str) -> Result<()> {
    let token = store.get_session_token(agent_id)?;
    check_session_token(workspace, store, agent_id, token.as_deref())?;
    Ok(())
}

fn check_session_token(
    workspace: &Path,
    store: &squad::store::Store,
    agent_id: &str,
    expected_token: Option<&str>,
) -> Result<()> {
    store.require_active_agent(agent_id)?;
    let current_token = store.get_session_token(agent_id)?;
    if let Some(expected) = expected_token {
        match current_token.as_deref() {
            Some(current) if current == expected => {}
            Some(_) | None => bail!(
                "Session replaced. Another terminal joined as {agent_id}. \
                 Re-join with a different ID (e.g. squad join {agent_id}-2 --role <your-role>)."
            ),
        }
    }
    let sessions = sessions_dir(workspace);
    if let Some(token) = expected_token {
        squad::session::validate(&sessions, agent_id, token)?;
    }
    Ok(())
}

fn print_messages(
    store: &squad::store::Store,
    messages: &[squad::store::MessageRecord],
    receiver: Option<&str>,
) -> Result<()> {
    for msg in messages {
        if msg.kind == "task_assigned" {
            let task = msg
                .task_id
                .as_deref()
                .and_then(|task_id| store.get_task(task_id).transpose())
                .transpose()?;
            println!(
                "[task {}] queued from {}",
                msg.task_id.as_deref().unwrap_or("unknown"),
                msg.from_agent
            );
            println!("  Title: {}", msg.content);
            if let Some(task) = task {
                println!("  Body: {}", task.body);
                println!(
                    "  Assigned to: {}",
                    task.assigned_to.unwrap_or_else(|| "unassigned".to_string())
                );
                println!("  Status: {}", task.status);
            }
            if let Some(id) = receiver {
                if let Some(task_id) = &msg.task_id {
                    println!(
                        "  → Reply: squad send --task-id {task_id} {id} {} \"<your response>\"",
                        msg.from_agent
                    );
                }
            }
        } else {
            println!("[from {}] {}", msg.from_agent, msg.content);
            if let Some(id) = receiver {
                println!(
                    "  → Reply: squad send {id} {} \"<your response>\"",
                    msg.from_agent
                );
            }
        }
    }
    if let Some(id) = receiver {
        if !messages.is_empty() {
            println!(
                "  → After processing, run `squad receive {id} --wait` to continue listening."
            );
        }
    }
    Ok(())
}

fn json_messages(
    store: &squad::store::Store,
    messages: Vec<squad::store::MessageRecord>,
) -> Result<Vec<String>> {
    let mut envelopes = Vec::with_capacity(messages.len());
    for msg in messages {
        let task = if msg.kind == "task_assigned" {
            msg.task_id
                .as_deref()
                .and_then(|task_id| store.get_task(task_id).transpose())
                .transpose()?
        } else {
            None
        };
        envelopes.push(ReceiveEnvelope {
            id: msg.id,
            from: msg.from_agent,
            to: msg.to_agent,
            content: msg.content,
            created_at: msg.created_at,
            read: msg.read,
            kind: msg.kind,
            task_id: msg.task_id,
            reply_to: msg.reply_to,
            task,
        });
    }
    envelopes
        .into_iter()
        .map(|envelope| serde_json::to_string(&envelope).map_err(Into::into))
        .collect()
}

fn print_json_messages(
    store: &squad::store::Store,
    messages: Vec<squad::store::MessageRecord>,
) -> Result<()> {
    for line in json_messages(store, messages)? {
        println!("{line}");
    }
    Ok(())
}

// --- Commands ---

fn cmd_init(args: Vec<String>) -> Result<()> {
    let mut refresh_roles = false;
    for arg in args {
        match arg.as_str() {
            "--refresh-roles" => refresh_roles = true,
            _ => bail!("Usage: squad init [--refresh-roles]"),
        }
    }

    let workspace = std::env::current_dir()?;
    squad::init::init_workspace_with_options(&workspace, refresh_roles)?;
    println!("Initialized squad workspace.");

    // Auto-update outdated slash commands
    let updated = squad::setup::check_and_update_commands();
    if !updated.is_empty() {
        println!("Updated slash commands:");
        for (name, path) in &updated {
            println!("  {} → {}", name, path.display());
        }
    }
    Ok(())
}

fn cmd_autopilot(args: Vec<String>) -> Result<()> {
    let subcommand = args.first().map(String::as_str).unwrap_or_default();
    match subcommand {
        "init" => {
            if args.len() != 1 {
                bail!("Usage: squad autopilot init");
            }
            cmd_autopilot_init()
        }
        "plan" => {
            if args.len() != 2 {
                bail!("Usage: squad autopilot plan <PRD.md>");
            }
            cmd_autopilot_plan(&args[1])
        }
        "launch" => {
            let options = parse_autopilot_launch_args(&args[1..])?;
            cmd_autopilot_launch(&options)
        }
        "run" => {
            if args.len() != 2 {
                bail!("Usage: squad autopilot run <PRD.md>");
            }
            cmd_autopilot_run(&args[1])
        }
        _ => bail!("Usage: squad autopilot <init|plan|launch|run> ..."),
    }
}

fn cmd_autopilot_init() -> Result<()> {
    let workspace = std::env::current_dir()?;
    let result = squad::autopilot::init_autopilot_workspace(&workspace)?;
    println!("Initialized squad autopilot workspace.");
    if result.config_created {
        println!("Created config: {}", result.config_path.display());
    } else {
        println!("Config already exists: {}", result.config_path.display());
    }
    println!("Autopilot artifacts: {}", result.autopilot_dir.display());
    println!("Generated roles: {}", result.generated_roles_dir.display());
    println!("Teams directory: {}", result.teams_dir.display());
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AutopilotLaunchOptions {
    run_id: i64,
    execute: bool,
    terminal_backend: AutopilotLaunchBackend,
    tmux_session: Option<String>,
    terminal_title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AutopilotLaunchBackend {
    Tmux,
    MacosTerminal,
}

fn parse_autopilot_launch_args(args: &[String]) -> Result<AutopilotLaunchOptions> {
    let mut run_id = None;
    let mut execute = false;
    let mut terminal_backend = default_autopilot_launch_backend();
    let mut terminal_backend_provided = false;
    let mut tmux_session = None;
    let mut terminal_title = None;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--run-id" => {
                let value = args.get(i + 1).context("--run-id requires a value")?;
                let parsed: i64 = value
                    .parse()
                    .with_context(|| format!("invalid autopilot run id: {value}"))?;
                if parsed <= 0 {
                    bail!("autopilot run id must be positive");
                }
                run_id = Some(parsed);
                i += 2;
            }
            "--execute" => {
                execute = true;
                i += 1;
            }
            "--terminal-backend" => {
                let value = args
                    .get(i + 1)
                    .context("--terminal-backend requires a value")?;
                terminal_backend = parse_autopilot_launch_backend(value)?;
                terminal_backend_provided = true;
                i += 2;
            }
            "--tmux-session" => {
                let value = args.get(i + 1).context("--tmux-session requires a value")?;
                if value.trim().is_empty() {
                    bail!("tmux session name cannot be empty");
                }
                if !terminal_backend_provided {
                    terminal_backend = AutopilotLaunchBackend::Tmux;
                }
                tmux_session = Some(value.trim().to_string());
                i += 2;
            }
            "--terminal-title" => {
                let value = args
                    .get(i + 1)
                    .context("--terminal-title requires a value")?;
                if value.trim().is_empty() {
                    bail!("terminal title cannot be empty");
                }
                if !terminal_backend_provided {
                    terminal_backend = AutopilotLaunchBackend::MacosTerminal;
                }
                terminal_title = Some(value.trim().to_string());
                i += 2;
            }
            _ => bail!("{}", autopilot_launch_usage()),
        }
    }
    let run_id = run_id.context(autopilot_launch_usage())?;
    Ok(AutopilotLaunchOptions {
        run_id,
        execute,
        terminal_backend,
        tmux_session,
        terminal_title,
    })
}

fn default_autopilot_launch_backend() -> AutopilotLaunchBackend {
    if cfg!(target_os = "macos") {
        AutopilotLaunchBackend::MacosTerminal
    } else {
        AutopilotLaunchBackend::Tmux
    }
}

fn parse_autopilot_launch_backend(value: &str) -> Result<AutopilotLaunchBackend> {
    match value.trim() {
        "tmux" => Ok(AutopilotLaunchBackend::Tmux),
        "macos-terminal" | "macos" | "terminal" | "terminal.app" => {
            Ok(AutopilotLaunchBackend::MacosTerminal)
        }
        other => bail!("unsupported terminal backend: {other}; expected tmux or macos-terminal"),
    }
}

fn autopilot_launch_usage() -> &'static str {
    "Usage: squad autopilot launch --run-id <id> [--execute] [--terminal-backend <tmux|macos-terminal>] [--tmux-session <name>] [--terminal-title <name>]"
}

fn cmd_autopilot_launch(options: &AutopilotLaunchOptions) -> Result<()> {
    let workspace = find_workspace()?;
    let store = open_store(&workspace)?;
    let launch_started_at = chrono::Utc::now().timestamp();
    let run_id = options.run_id;
    let run = store
        .get_autopilot_run(run_id)?
        .with_context(|| format!("autopilot run does not exist: {run_id}"))?;
    let agents = store.list_autopilot_agents(run_id)?;
    if agents.is_empty() {
        bail!("autopilot run {run_id} has no generated agents");
    }

    let mut roles = Vec::with_capacity(agents.len());
    let mut role_overrides = BTreeMap::new();
    for agent in &agents {
        let role_id = agent.role.trim();
        if role_id.is_empty() {
            bail!("autopilot agent {} has empty role", agent.id);
        }
        let provider: squad::autopilot::ModelProvider = agent.model_provider.parse()?;
        roles.push(squad::autopilot::GeneratedTeamRole {
            role_id: role_id.to_string(),
            prompt_file: format!("generated/{role_id}"),
        });
        role_overrides.insert(role_id.to_string(), provider);
    }
    let config = squad::autopilot::AutopilotConfig {
        role_overrides,
        ..squad::autopilot::AutopilotConfig::default()
    };
    let sessions = scope_autopilot_terminal_sessions(
        run_id,
        squad::autopilot::plan_terminal_sessions(&workspace, &roles, &config)?,
    );
    let persisted_sessions = store.create_autopilot_terminal_sessions(run_id, &sessions)?;

    println!("Autopilot launch plan for run {}", run.id);
    println!("PRD: {}", run.prd_path);
    println!("Sessions: {}", sessions.len());
    println!("Persisted sessions: {}", persisted_sessions.len());
    println!(
        "Terminal backend: {}",
        match options.terminal_backend {
            AutopilotLaunchBackend::Tmux => "tmux",
            AutopilotLaunchBackend::MacosTerminal => "macos-terminal",
        }
    );
    for session in &sessions {
        println!(
            "- {} [{}] {} -> {}",
            session.agent_id,
            autopilot_session_role_label(&session.session_role),
            session.model_provider,
            session.command
        );
        println!("  inject: {}", session.inject_text);
    }
    if options.execute {
        match options.terminal_backend {
            AutopilotLaunchBackend::Tmux => {
                let tmux_session = options
                    .tmux_session
                    .clone()
                    .unwrap_or_else(|| format!("squad-autopilot-{run_id}"));
                let delivery = execute_tmux_spawn_with_sequential_delivery(
                    &store,
                    run_id,
                    &tmux_session,
                    &sessions,
                    launch_started_at.saturating_sub(1),
                    autopilot_assignment_wait_secs(),
                    autopilot_tasks_per_role(),
                )?;
                println!("Executed tmux launch: {tmux_session}");
                println!("Attach with: tmux attach -t {tmux_session}");
                println!(
                    "Autopilot task delivery: {} created, {} already existed.",
                    delivery.created, delivery.already_existed
                );
                if !delivery.missing_roles.is_empty() {
                    println!(
                        "Autopilot task delivery pending; missing active roles: {}",
                        delivery.missing_roles.join(", ")
                    );
                }
            }
            AutopilotLaunchBackend::MacosTerminal => {
                let terminal_title = options
                    .terminal_title
                    .clone()
                    .unwrap_or_else(|| format!("squad-autopilot-{run_id}"));
                let delivery = execute_macos_terminal_spawn_with_sequential_delivery(
                    &store,
                    run_id,
                    &terminal_title,
                    &sessions,
                    launch_started_at.saturating_sub(1),
                    autopilot_assignment_wait_secs(),
                    autopilot_tasks_per_role(),
                )?;
                println!("Executed macOS Terminal launch: {terminal_title}");
                println!("Opened physical Terminal.app windows.");
                println!(
                    "Terminal.app prompt injection waits for each provider and submits the squad command."
                );
                println!(
                    "Autopilot task delivery: {} created, {} already existed.",
                    delivery.created, delivery.already_existed
                );
                if !delivery.missing_roles.is_empty() {
                    println!(
                        "Autopilot task delivery pending; missing active roles: {}",
                        delivery.missing_roles.join(", ")
                    );
                }
            }
        }
    } else {
        println!("Dry run only. Add --execute to create terminal windows.");
    }
    Ok(())
}

fn scope_autopilot_terminal_sessions(
    run_id: i64,
    sessions: Vec<squad::autopilot::TerminalSessionPlan>,
) -> Vec<squad::autopilot::TerminalSessionPlan> {
    sessions
        .into_iter()
        .map(|mut session| {
            let agent_id = format!("{}-r{run_id}", session.role_id);
            session.agent_id = agent_id.clone();
            session.pane_label = agent_id.clone();
            session.inject_text = squad::autopilot::role_specific_injection_text(
                &session.model_provider,
                &session.role_id,
                &agent_id,
            );
            session
        })
        .collect()
}

#[derive(Debug, Default, PartialEq, Eq)]
struct AutopilotTaskDelivery {
    created: usize,
    already_existed: usize,
    missing_roles: Vec<String>,
    created_task_ids: Vec<String>,
}

impl AutopilotTaskDelivery {
    fn add(&mut self, other: AutopilotTaskDelivery) {
        self.created += other.created;
        self.already_existed += other.already_existed;
        self.created_task_ids.extend(other.created_task_ids);
        for role in other.missing_roles {
            if !self.missing_roles.contains(&role) {
                self.missing_roles.push(role);
            }
        }
    }
}

fn autopilot_assignment_wait_secs() -> u64 {
    std::env::var("SQUAD_AUTOPILOT_ASSIGN_WAIT_SECS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_AUTOPILOT_ASSIGN_WAIT_SECS)
}

fn autopilot_tasks_per_role() -> usize {
    std::env::var("SQUAD_AUTOPILOT_TASKS_PER_ROLE")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_AUTOPILOT_TASKS_PER_ROLE)
}

fn execute_tmux_spawn_with_sequential_delivery(
    store: &squad::store::Store,
    run_id: i64,
    tmux_session: &str,
    sessions: &[squad::autopilot::TerminalSessionPlan],
    joined_at_or_after: i64,
    wait_secs: u64,
    tasks_per_role: usize,
) -> Result<AutopilotTaskDelivery> {
    execute_terminal_spawn_with_sequential_delivery(
        store,
        run_id,
        sessions,
        joined_at_or_after,
        wait_secs,
        tasks_per_role,
        "tmux",
        |session| squad::autopilot::execute_tmux_spawn_one(tmux_session, session),
    )
}

fn execute_macos_terminal_spawn_with_sequential_delivery(
    store: &squad::store::Store,
    run_id: i64,
    terminal_title: &str,
    sessions: &[squad::autopilot::TerminalSessionPlan],
    joined_at_or_after: i64,
    wait_secs: u64,
    tasks_per_role: usize,
) -> Result<AutopilotTaskDelivery> {
    execute_terminal_spawn_with_sequential_delivery(
        store,
        run_id,
        sessions,
        joined_at_or_after,
        wait_secs,
        tasks_per_role,
        "macOS Terminal",
        |session| squad::autopilot::execute_macos_terminal_spawn_one(terminal_title, session),
    )
}

fn execute_terminal_spawn_with_sequential_delivery<F>(
    store: &squad::store::Store,
    run_id: i64,
    sessions: &[squad::autopilot::TerminalSessionPlan],
    joined_at_or_after: i64,
    wait_secs: u64,
    tasks_per_role: usize,
    backend_label: &str,
    mut launch_session: F,
) -> Result<AutopilotTaskDelivery>
where
    F: FnMut(&squad::autopilot::TerminalSessionPlan) -> Result<()>,
{
    if sessions.is_empty() {
        bail!("{backend_label} spawn command list cannot be empty");
    }
    let manager_session = sessions
        .iter()
        .find(|session| {
            session.role_id == "manager"
                || session.session_role == squad::autopilot::TerminalSessionRole::Manager
        })
        .with_context(|| "autopilot launch requires a manager terminal session")?;

    let mut ordered_sessions = vec![manager_session];
    for session in sessions {
        if !std::ptr::eq(session, manager_session) {
            ordered_sessions.push(session);
        }
    }

    let mut delivery = AutopilotTaskDelivery::default();
    for session in ordered_sessions {
        println!(
            "Sequential {backend_label} launch: launching {} ({})",
            session.role_id, session.model_provider
        );
        launch_session(session)?;

        let needed_roles = if session.role_id == "manager" {
            vec!["manager".to_string()]
        } else {
            vec!["manager".to_string(), session.role_id.clone()]
        };
        let active_agents =
            wait_for_ready_autopilot_roles(store, &needed_roles, joined_at_or_after, wait_secs)?;
        let missing_roles: Vec<String> = needed_roles
            .iter()
            .filter(|role| !active_agents.contains_key(role.as_str()))
            .cloned()
            .collect();
        if !missing_roles.is_empty() {
            println!(
                "Sequential {backend_label} launch stopped; missing ready roles: {}",
                missing_roles.join(", ")
            );
            delivery.add(AutopilotTaskDelivery {
                created: 0,
                already_existed: 0,
                missing_roles,
                created_task_ids: Vec::new(),
            });
            return Ok(delivery);
        }

        let Some(manager_id) = active_agents.get("manager") else {
            continue;
        };
        let Some(assigned_to) = active_agents.get(session.role_id.as_str()) else {
            continue;
        };
        let role_delivery = deliver_ready_autopilot_tasks_for_role(
            store,
            run_id,
            manager_id,
            &session.role_id,
            assigned_to,
            tasks_per_role,
        )?;
        println!(
            "Sequential task delivery for {}: {} created, {} already existed.",
            session.role_id, role_delivery.created, role_delivery.already_existed
        );
        if !wait_for_squad_tasks_to_start(store, &role_delivery.created_task_ids, wait_secs)? {
            println!(
                "Sequential {backend_label} launch stopped; {} did not ack or complete its assigned task.",
                session.role_id
            );
            delivery.add(AutopilotTaskDelivery {
                created: role_delivery.created,
                already_existed: role_delivery.already_existed,
                missing_roles: vec![session.role_id.clone()],
                created_task_ids: role_delivery.created_task_ids,
            });
            return Ok(delivery);
        }
        delivery.add(role_delivery);
    }
    Ok(delivery)
}

fn deliver_ready_autopilot_tasks_for_role(
    store: &squad::store::Store,
    run_id: i64,
    manager_id: &str,
    role: &str,
    assigned_to: &str,
    task_limit: usize,
) -> Result<AutopilotTaskDelivery> {
    let autopilot_agents = store.list_autopilot_agents(run_id)?;
    let roles_by_autopilot_agent_id: BTreeMap<i64, String> = autopilot_agents
        .into_iter()
        .map(|agent| (agent.id, agent.role))
        .collect();
    let mut delivery = AutopilotTaskDelivery::default();
    for task in store.list_autopilot_tasks(run_id)? {
        if task.status != "READY_PARALLEL" {
            continue;
        }
        let Some(autopilot_agent_id) = task.assigned_agent_id else {
            continue;
        };
        if roles_by_autopilot_agent_id
            .get(&autopilot_agent_id)
            .map(|assigned_role| assigned_role != role)
            .unwrap_or(true)
        {
            continue;
        }
        if autopilot_task_has_squad_delivery(store, task.id)? {
            delivery.already_existed += 1;
            continue;
        }
        let squad_task_id = store.create_task(
            manager_id,
            assigned_to,
            &format!("Autopilot task {}: {}", task.id, task.title),
            &autopilot_squad_task_body(run_id, &task),
        )?;
        delivery.created += 1;
        delivery.created_task_ids.push(squad_task_id);
        if delivery.created >= task_limit {
            break;
        }
    }
    Ok(delivery)
}

fn wait_for_squad_tasks_to_start(
    store: &squad::store::Store,
    task_ids: &[String],
    wait_secs: u64,
) -> Result<bool> {
    if task_ids.is_empty() || wait_secs == 0 {
        return Ok(true);
    }
    let deadline = chrono::Utc::now().timestamp() + wait_secs as i64;
    loop {
        let mut all_started = true;
        for task_id in task_ids {
            let Some(task) = store.get_task(task_id)? else {
                all_started = false;
                break;
            };
            if task.status == "queued" && !squad_task_assignment_was_read(store, task_id)? {
                all_started = false;
                break;
            }
        }
        if all_started {
            return Ok(true);
        }
        if chrono::Utc::now().timestamp() >= deadline {
            return Ok(false);
        }
        thread::sleep(Duration::from_secs(1));
    }
}

fn squad_task_assignment_was_read(store: &squad::store::Store, task_id: &str) -> Result<bool> {
    Ok(store.all_messages(None)?.iter().any(|message| {
        message.kind == "task_assigned"
            && message.read
            && message.task_id.as_deref() == Some(task_id)
    }))
}

fn wait_for_ready_autopilot_roles(
    store: &squad::store::Store,
    roles: &[String],
    joined_at_or_after: i64,
    wait_secs: u64,
) -> Result<BTreeMap<String, String>> {
    let deadline = chrono::Utc::now().timestamp() + wait_secs as i64;
    loop {
        let active_agents = newest_ready_agents_by_role(store, joined_at_or_after)?;
        let all_present = roles
            .iter()
            .all(|role| active_agents.contains_key(role.as_str()));
        if all_present || wait_secs == 0 || chrono::Utc::now().timestamp() >= deadline {
            return Ok(active_agents);
        }
        thread::sleep(Duration::from_secs(1));
    }
}

fn newest_ready_agents_by_role(
    store: &squad::store::Store,
    joined_at_or_after: i64,
) -> Result<BTreeMap<String, String>> {
    let mut newest: BTreeMap<String, (String, i64)> = BTreeMap::new();
    for agent in store.list_agents(false)? {
        if agent.joined_at < joined_at_or_after {
            continue;
        }
        if !supports_capability(&agent) {
            continue;
        }
        let replace = newest
            .get(&agent.role)
            .map(|(_, joined_at)| agent.joined_at >= *joined_at)
            .unwrap_or(true);
        if replace {
            newest.insert(agent.role, (agent.id, agent.joined_at));
        }
    }
    Ok(newest
        .into_iter()
        .map(|(role, (id, _))| (role, id))
        .collect())
}

fn autopilot_task_has_squad_delivery(
    store: &squad::store::Store,
    autopilot_task_id: i64,
) -> Result<bool> {
    let marker = autopilot_squad_task_marker(autopilot_task_id);
    Ok(store
        .list_tasks(None, None)?
        .iter()
        .any(|task| task.body.contains(&marker)))
}

fn autopilot_squad_task_marker(autopilot_task_id: i64) -> String {
    format!("Autopilot-Task-ID: {autopilot_task_id}")
}

fn autopilot_squad_task_body(run_id: i64, task: &squad::store::AutopilotTaskRecord) -> String {
    let acceptance = if task.acceptance_criteria.is_empty() {
        "- Not specified".to_string()
    } else {
        task.acceptance_criteria
            .iter()
            .map(|criterion| format!("- {criterion}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "{}\nAutopilot-Run-ID: {run_id}\n\n{}\n\nAcceptance criteria:\n{}\n\nAfter completing the work, run `squad task complete <your-id> <task-id> --summary \"<summary>\"` and include changed files, tests run, and unresolved risks.",
        autopilot_squad_task_marker(task.id),
        task.description,
        acceptance
    )
}

fn cmd_autopilot_plan(prd_path: &str) -> Result<()> {
    let prd_path = Path::new(prd_path);
    let document = squad::autopilot::ingest_prd_file(prd_path)?;
    let mut graph =
        squad::autopilot::extract_prd_task_graph_basics(&document.display_path, &document.content);
    squad::autopilot::classify_task_graph_statuses(&mut graph)?;

    println!("Autopilot plan for {}", document.display_path);
    println!("Tasks: {}", graph.tasks.len());
    for status in [
        squad::autopilot::TaskGraphStatus::ReadyParallel,
        squad::autopilot::TaskGraphStatus::Blocked,
        squad::autopilot::TaskGraphStatus::Sequential,
        squad::autopilot::TaskGraphStatus::ReviewRequired,
        squad::autopilot::TaskGraphStatus::Done,
        squad::autopilot::TaskGraphStatus::Failed,
    ] {
        let count = graph
            .tasks
            .iter()
            .filter(|task| task.status == status)
            .count();
        println!("{}: {}", autopilot_status_label(&status), count);
    }
    println!();
    print_autopilot_plan_section(
        "Ready Parallel",
        &graph,
        &squad::autopilot::TaskGraphStatus::ReadyParallel,
    );
    print_autopilot_plan_section(
        "Blocked",
        &graph,
        &squad::autopilot::TaskGraphStatus::Blocked,
    );
    print_autopilot_plan_section(
        "Sequential",
        &graph,
        &squad::autopilot::TaskGraphStatus::Sequential,
    );
    print_autopilot_plan_section(
        "Review Required",
        &graph,
        &squad::autopilot::TaskGraphStatus::ReviewRequired,
    );
    Ok(())
}

fn cmd_autopilot_run(prd_path: &str) -> Result<()> {
    let workspace = find_workspace()?;
    let _init = squad::autopilot::init_autopilot_workspace(&workspace)?;
    let store = open_store(&workspace)?;
    let config = squad::autopilot::load_config(&workspace)?;
    let document = squad::autopilot::ingest_prd_file(Path::new(prd_path))?;
    let mut graph =
        squad::autopilot::extract_prd_task_graph_basics(&document.display_path, &document.content);
    squad::autopilot::classify_task_graph_statuses(&mut graph)?;

    let context = autopilot_role_context_from_graph(&graph);
    let specs = squad::autopilot::synthesize_role_specs_from_prd(&context);
    let specs = squad::autopilot::apply_model_policy_to_role_specs(&specs, &config);
    let generated_roles = squad::autopilot::write_generated_roles(&workspace, &context, &specs)?;
    let team_path = squad::autopilot::write_autopilot_team(&workspace, &generated_roles)?;
    let science_artifacts =
        squad::autopilot::write_science_swarm_artifacts(&workspace, &graph, &specs)?;

    let run = store.create_autopilot_run(&document.display_path)?;
    let agent_inputs: Vec<squad::store::AutopilotAgentInput> = specs
        .iter()
        .map(|spec| squad::store::AutopilotAgentInput {
            name: spec.role_name.clone(),
            role: spec.role_id.clone(),
            model_provider: spec.model_provider.as_str().to_string(),
            skills_prompt: spec.skills_prompt.clone(),
        })
        .collect();
    let agents = store.create_autopilot_agents(run.id, &agent_inputs)?;
    let tasks = store.create_autopilot_tasks(run.id, &graph.tasks)?;
    let session_config = squad::autopilot::AutopilotConfig {
        model_mix: config.model_mix.clone(),
        adaptive_scheduling: config.adaptive_scheduling.clone(),
        role_overrides: specs
            .iter()
            .map(|spec| (spec.role_id.clone(), spec.model_provider.clone()))
            .collect(),
    };
    let sessions =
        squad::autopilot::plan_terminal_sessions(&workspace, &generated_roles, &session_config)?;
    let persisted_sessions = store.create_autopilot_terminal_sessions(run.id, &sessions)?;
    let assigned_tasks = store.assign_ready_autopilot_tasks(run.id)?;

    let test_records = squad::autopilot::record_test_run(
        &workspace,
        squad::autopilot::TestRunRecord {
            command: "cargo test".to_string(),
            status: squad::autopilot::TestRunStatus::Skipped,
            exit_code: None,
            task_id: None,
            agent_id: Some("manager".to_string()),
            notes: Some(
                "autopilot run initialized test checkpoint; execute after worker changes"
                    .to_string(),
            ),
        },
    )?;
    let accepted = store.autopilot_run_acceptance_satisfied(run.id)?;
    let detected_files = squad::autopilot::detect_git_files_changed(&workspace)?;
    let files_changed = squad::autopilot::record_files_changed(&workspace, &detected_files)?;
    let failures_retries = squad::autopilot::read_failures_retries(&workspace)?;
    let persisted_tasks = store.list_autopilot_tasks(run.id)?;
    let final_report = squad::autopilot::FinalReport {
        product_goals: graph.product_goals.clone(),
        milestones: graph.milestones.clone(),
        acceptance_criteria: graph.acceptance_criteria.clone(),
        test_requirements: graph.test_requirements.clone(),
        prd_tasks_completed: autopilot_completed_task_lines(&persisted_tasks),
        task_graph: autopilot_task_graph_report_lines(&graph),
        agents_used: autopilot_agents_report_lines(&agents),
        model_mix_used: autopilot_model_mix_report_lines(&config),
        files_changed,
        tests_run: squad::autopilot::tests_run_report_lines(&test_records),
        failures_retries: squad::autopilot::failures_retries_report_lines(&failures_retries),
        unresolved_risks: autopilot_unresolved_risk_lines(&graph, accepted),
        final_git_diff_summary: squad::autopilot::git_diff_stat_summary(&workspace)?,
    };
    let final_report_path = squad::autopilot::write_final_report(&workspace, &final_report)?;
    if accepted {
        let _ = store.complete_autopilot_run_if_accepted(run.id)?;
    }

    println!("Autopilot run initialized: {}", run.id);
    println!("PRD: {}", document.display_path);
    println!("Team: {}", team_path.display());
    println!("Science swarm artifacts: {}", science_artifacts.len());
    println!("Agents: {}", agents.len());
    println!("Tasks: {}", tasks.len());
    println!("Sessions planned: {}", persisted_sessions.len());
    println!("Ready tasks assigned: {}", assigned_tasks.len());
    println!("Tests recorded: {}", test_records.len());
    println!("Final report: {}", final_report_path.display());
    println!(
        "Acceptance criteria: {}",
        if accepted { "passed" } else { "pending" }
    );
    println!("Next: squad autopilot launch --run-id {}", run.id);
    Ok(())
}

fn autopilot_role_context_from_graph(
    graph: &squad::autopilot::TaskGraph,
) -> squad::autopilot::PrdRoleContext {
    squad::autopilot::PrdRoleContext {
        prd_path: graph.prd_path.clone(),
        product_goal: graph.product_goals.join("\n"),
        milestones: graph.milestones.clone(),
        implementation_tasks: graph.tasks.iter().map(|task| task.title.clone()).collect(),
        acceptance_criteria: graph.acceptance_criteria.clone(),
        risky_areas: graph.risky_areas.clone(),
        likely_files: graph
            .tasks
            .iter()
            .flat_map(|task| task.likely_files.clone())
            .collect(),
        test_requirements: graph.test_requirements.clone(),
    }
}

fn autopilot_completed_task_lines(tasks: &[squad::store::AutopilotTaskRecord]) -> Vec<String> {
    tasks
        .iter()
        .filter(|task| task.status == "DONE")
        .map(|task| format!("{}: {}", task.id, task.title))
        .collect()
}

fn autopilot_task_graph_report_lines(graph: &squad::autopilot::TaskGraph) -> Vec<String> {
    graph
        .tasks
        .iter()
        .map(|task| {
            let dependencies = if task.depends_on.is_empty() {
                "none".to_string()
            } else {
                task.depends_on.join(", ")
            };
            format!(
                "{} [{}] {} (depends on: {})",
                task.id,
                autopilot_status_label(&task.status),
                task.title,
                dependencies
            )
        })
        .collect()
}

fn autopilot_agents_report_lines(agents: &[squad::store::AutopilotAgentRecord]) -> Vec<String> {
    agents
        .iter()
        .map(|agent| format!("{}: {} ({})", agent.role, agent.name, agent.model_provider))
        .collect()
}

fn autopilot_model_mix_report_lines(config: &squad::autopilot::AutopilotConfig) -> Vec<String> {
    let mut lines = vec![
        format!("claude {:.0}%", config.model_mix.claude * 100.0),
        format!("codex {:.0}%", config.model_mix.codex * 100.0),
        format!("gemini {:.0}%", config.model_mix.gemini * 100.0),
        format!(
            "openrouter_free {:.0}%",
            config.model_mix.openrouter_free * 100.0
        ),
        format!(
            "openrouter_cheap {:.0}%",
            config.model_mix.openrouter_cheap * 100.0
        ),
        format!("local {:.0}%", config.model_mix.local * 100.0),
    ];
    lines.extend(
        config
            .role_overrides
            .iter()
            .map(|(role, provider)| format!("{role} override: {provider}")),
    );
    lines
}

fn autopilot_unresolved_risk_lines(
    graph: &squad::autopilot::TaskGraph,
    accepted: bool,
) -> Vec<String> {
    let mut risks = graph.risky_areas.clone();
    if !accepted {
        risks.push("Autopilot run initialized; acceptance criteria are still pending.".to_string());
    }
    risks
}

fn print_autopilot_plan_section(
    title: &str,
    graph: &squad::autopilot::TaskGraph,
    status: &squad::autopilot::TaskGraphStatus,
) {
    println!("{title}:");
    let tasks: Vec<&squad::autopilot::TaskGraphTask> = graph
        .tasks
        .iter()
        .filter(|task| task.status == *status)
        .collect();
    if tasks.is_empty() {
        println!("  (none)");
        return;
    }
    for task in tasks {
        println!("  - {} {}", task.id, task.title);
        if !task.depends_on.is_empty() {
            println!("    depends on: {}", task.depends_on.join(", "));
        }
        if let Some(role) = task.assigned_role.as_deref() {
            println!("    role: {role}");
        }
    }
}

fn autopilot_status_label(status: &squad::autopilot::TaskGraphStatus) -> &'static str {
    match status {
        squad::autopilot::TaskGraphStatus::ReadyParallel => "READY_PARALLEL",
        squad::autopilot::TaskGraphStatus::Blocked => "BLOCKED",
        squad::autopilot::TaskGraphStatus::Sequential => "SEQUENTIAL",
        squad::autopilot::TaskGraphStatus::ReviewRequired => "REVIEW_REQUIRED",
        squad::autopilot::TaskGraphStatus::Done => "DONE",
        squad::autopilot::TaskGraphStatus::Failed => "FAILED",
    }
}

fn autopilot_session_role_label(role: &squad::autopilot::TerminalSessionRole) -> &'static str {
    match role {
        squad::autopilot::TerminalSessionRole::Manager => "manager",
        squad::autopilot::TerminalSessionRole::Inspector => "inspector",
        squad::autopilot::TerminalSessionRole::Worker => "worker",
    }
}

fn cmd_join(id: &str, options: &JoinOptions) -> Result<()> {
    let workspace = find_workspace()?;
    let store = open_store(&workspace)?;
    let role = options.role.as_deref().unwrap_or(id);
    let (actual_id, token) = store.register_agent_unique_with_metadata(
        id,
        role,
        options.client_type.as_deref(),
        options.protocol_version,
    )?;
    store.touch_agent(&actual_id)?;
    squad::session::write_token(&sessions_dir(&workspace), &actual_id, &token)?;
    if actual_id != id {
        println!("ID '{id}' was taken. Joined as {actual_id} (role: {role}).");
    } else {
        println!("Joined as {actual_id} (role: {role}).");
    }

    match squad::roles::load_role(&workspace, role) {
        Ok(prompt) => {
            println!("\n=== Role Instructions ===\n{prompt}");
        }
        Err(_) => {
            println!("\nNo predefined template for \"{role}\". Interpret this role autonomously.");
            println!("Communicate using: squad send, squad receive, squad agents");
            println!("Tip: create .squad/roles/{role}.md to customize behavior.");
            let roles = squad::roles::list_roles(&workspace);
            println!("Predefined roles: {}", roles.join(", "));
        }
    }
    Ok(())
}

fn cmd_leave(id: &str) -> Result<()> {
    let workspace = find_workspace()?;
    let store = open_store(&workspace)?;
    store.unregister_agent(id)?;
    squad::session::delete_token(&sessions_dir(&workspace), id)?;
    println!("{id} archived from the squad. Unread work was preserved.");
    Ok(())
}

fn cmd_agents(show_all: bool, json: bool) -> Result<()> {
    let workspace = find_workspace()?;
    let store = open_store(&workspace)?;
    let agents = store.list_agents(show_all)?;
    if json {
        for agent in &agents {
            println!("{}", serde_json::to_string(&agent_envelope(agent))?);
        }
        return Ok(());
    }
    if agents.is_empty() {
        if show_all {
            println!("No agents found.");
        } else {
            println!("No agents online.");
        }
    } else {
        let now = chrono::Utc::now().timestamp();
        for agent in &agents {
            let status = if agent.status == "archived" {
                let suffix = agent
                    .archived_at
                    .map(|ts| format!(" at {}", format_history_timestamp(ts)))
                    .unwrap_or_default();
                format!("archived{suffix}")
            } else {
                match agent.last_seen {
                    Some(ts) => {
                        let ago = now - ts;
                        if ago < 60 {
                            format!("active ({}s ago)", ago)
                        } else if ago < 600 {
                            format!("idle ({}m ago)", ago / 60)
                        } else {
                            format!("stale ({}m ago)", ago / 60)
                        }
                    }
                    None => "unknown".to_string(),
                }
            };
            let capability_suffix = format!(
                " [client: {}, protocol: {}]",
                effective_client_type(agent),
                effective_protocol_version(agent)
            );
            println!(
                "  {} (role: {}) — {}{}",
                agent.id, agent.role, status, capability_suffix
            );
        }
    }
    Ok(())
}

fn cmd_send(options: &SendOptions) -> Result<()> {
    let workspace = find_workspace()?;
    let store = open_store(&workspace)?;
    let now = chrono::Utc::now().timestamp();
    ensure_agent_exists(&store, &options.from)?;
    check_session(&workspace, &store, &options.from)?;
    store.touch_agent(&options.from)?;
    if options.to == "@all" {
        if options.task_id.is_some() || options.reply_to.is_some() {
            bail!("task-linked send metadata is only supported for direct messages");
        }
        let recipients = store.broadcast_message(&options.from, &options.message)?;
        println!(
            "Broadcast to {} agents: {}",
            recipients.len(),
            recipients.join(", ")
        );
        if let Some(warning) = stale_broadcast_warning(&store.list_agents(false)?, &recipients, now)
        {
            println!("{warning}");
        }
    } else {
        store.send_message_checked_with_metadata(
            &options.from,
            &options.to,
            &options.message,
            options.task_id.as_deref(),
            options.reply_to,
        )?;
        println!("Sent to {}.", options.to);
        if let Some(agent) = store
            .list_agents(false)?
            .into_iter()
            .find(|agent| agent.id == options.to)
        {
            if let Some(warning) = stale_direct_warning(&agent, now) {
                println!("{warning}");
            }
        }
    }
    Ok(())
}

fn stale_minutes(last_seen: Option<i64>, now: i64) -> Option<i64> {
    let ago = now - last_seen?;
    if ago >= 600 {
        Some(ago / 60)
    } else {
        None
    }
}

fn stale_direct_warning(agent: &squad::store::AgentRecord, now: i64) -> Option<String> {
    let minutes = stale_minutes(agent.last_seen, now)?;
    Some(format!(
        "Warning: {} is stale (last seen {}m ago). Message was queued but may not be seen soon.",
        agent.id, minutes
    ))
}

fn stale_broadcast_warning(
    agents: &[squad::store::AgentRecord],
    recipients: &[String],
    now: i64,
) -> Option<String> {
    let stale: Vec<String> = agents
        .iter()
        .filter(|agent| recipients.iter().any(|recipient| recipient == &agent.id))
        .filter_map(|agent| {
            let minutes = stale_minutes(agent.last_seen, now)?;
            Some(format!("{} ({}m ago)", agent.id, minutes))
        })
        .collect();
    if stale.is_empty() {
        None
    } else {
        Some(format!("Warning: stale recipients: {}.", stale.join(", ")))
    }
}

fn cmd_receive(agent: &str, wait: bool, timeout_secs: u64, json: bool) -> Result<()> {
    let workspace = find_workspace()?;

    // Validate session at entry (catches displacement immediately)
    let store = open_store(&workspace)?;
    ensure_agent_exists(&store, agent)?;
    let session_token = store.get_session_token(agent)?;
    check_session(&workspace, &store, agent)?;
    store.touch_agent(agent)?;

    if wait {
        // Acquire exclusive file lock to prevent multiple concurrent receive --wait
        // processes from competing for the same agent's messages.
        let lock_dir = workspace.join("locks");
        std::fs::create_dir_all(&lock_dir)?;
        let lock_path = lock_dir.join(format!("{}.receive.lock", agent));
        let lock_file = std::fs::File::create(&lock_path)
            .with_context(|| format!("failed to create lock file: {}", lock_path.display()))?;
        if lock_file.try_lock_exclusive().is_err() {
            bail!(
                "Another `squad receive --wait` is already running for agent '{}'. \
                 Only one receive --wait per agent is allowed. Use `squad receive {}` \
                 (without --wait) for non-blocking polling.",
                agent,
                agent
            );
        }
        // Keep _lock_file alive for the duration of the wait loop (lock released on drop).
        let _lock_guard = lock_file;

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
        let mut last_heartbeat = std::time::Instant::now();
        loop {
            let store = open_store(&workspace)?;

            // Re-check for displacement on each poll (~500ms)
            check_session_token(&workspace, &store, agent, session_token.as_deref())?;

            // Heartbeat: update last_seen every 30s so other agents know we're alive
            if last_heartbeat.elapsed() >= std::time::Duration::from_secs(30) {
                store.touch_agent(agent)?;
                last_heartbeat = std::time::Instant::now();
            }

            if store.has_unread_messages(agent)? {
                let messages = store.receive_messages(agent)?;
                if !messages.is_empty() {
                    if json {
                        print_json_messages(&store, messages)?;
                    } else {
                        print_messages(&store, &messages, Some(agent))?;
                    }
                    return Ok(());
                }
            }
            if std::time::Instant::now() > deadline {
                if json {
                    return Ok(());
                } else {
                    println!(
                        "No new messages (timed out after {timeout_secs}s). Run `squad receive {agent} --wait` to continue listening."
                    );
                }
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    } else {
        let messages = store.receive_messages(agent)?;
        if messages.is_empty() {
            if json {
                return Ok(());
            } else {
                println!("No new messages. Run `squad receive {agent} --wait` to keep listening.");
            }
        } else {
            if json {
                print_json_messages(&store, messages)?;
            } else {
                print_messages(&store, &messages, Some(agent))?;
            }
        }
        Ok(())
    }
}

fn cmd_task(args: Vec<String>) -> Result<()> {
    let subcommand = args.first().map(String::as_str).unwrap_or_default();
    match subcommand {
        "create" => {
            if args.len() < 5 {
                bail!("Usage: squad task create <from> <to> --title <title> [--body <body>]");
            }
            let (title, body) = parse_task_create_args(&args[1..])?;
            cmd_task_create(&args[1], &args[2], &title, body.as_deref().unwrap_or(""))
        }
        "ack" => {
            if args.len() != 3 {
                bail!("Usage: squad task ack <agent> <task-id>");
            }
            cmd_task_ack(&args[1], &args[2])
        }
        "complete" => {
            if args.len() < 5 {
                bail!("Usage: squad task complete <agent> <task-id> --summary <text>");
            }
            let summary = parse_task_complete_args(&args[1..])?;
            cmd_task_complete(&args[1], &args[2], &summary)
        }
        "requeue" => {
            if args.len() != 2 && args.len() != 4 {
                bail!("Usage: squad task requeue <task-id> [--to <agent>]");
            }
            let assignee = match args.get(2).map(String::as_str) {
                Some("--to") => Some(args.get(3).context("--to requires an agent id")?.as_str()),
                Some(flag) => bail!("unknown task requeue flag: {flag}"),
                None => None,
            };
            cmd_task_requeue(&args[1], assignee)
        }
        "list" => cmd_task_list(parse_task_list_args(&args[1..])?),
        _ => bail!("Usage: squad task <create|ack|complete|requeue|list> ..."),
    }
}

fn parse_task_list_args(args: &[String]) -> Result<TaskListOptions> {
    let mut options = TaskListOptions::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--agent" => {
                let value = args.get(i + 1).context("--agent requires an agent id")?;
                options.assigned_to = Some(value.clone());
                i += 2;
            }
            "--status" => {
                let value = args.get(i + 1).context("--status requires a value")?;
                options.status = Some(value.clone());
                i += 2;
            }
            flag => bail!("unknown task list flag: {flag}"),
        }
    }
    Ok(options)
}

fn parse_task_create_args(args: &[String]) -> Result<(String, Option<String>)> {
    let mut title = None;
    let mut body = None;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--title" => {
                let value = args.get(i + 1).context("--title requires a value")?;
                title = Some(value.clone());
                i += 2;
            }
            "--body" => {
                let value = args.get(i + 1).context("--body requires a value")?;
                body = Some(value.clone());
                i += 2;
            }
            flag => bail!("unknown task create flag: {flag}"),
        }
    }

    let title = title.context("--title is required")?;
    Ok((title, body))
}

fn parse_task_complete_args(args: &[String]) -> Result<String> {
    let mut summary = None;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--summary" => {
                let value = args.get(i + 1).context("--summary requires a value")?;
                summary = Some(value.clone());
                i += 2;
            }
            flag => bail!("unknown task complete flag: {flag}"),
        }
    }

    summary.context("--summary is required")
}

fn cmd_task_create(from: &str, to: &str, title: &str, body: &str) -> Result<()> {
    let workspace = find_workspace()?;
    let store = open_store(&workspace)?;
    ensure_agent_exists(&store, from)?;
    check_session(&workspace, &store, from)?;
    store.touch_agent(from)?;
    let task_id = store.create_task(from, to, title, body)?;
    println!("Created task {task_id} for {to}: {title}");
    Ok(())
}

fn cmd_task_ack(agent: &str, task_id: &str) -> Result<()> {
    let workspace = find_workspace()?;
    let store = open_store(&workspace)?;
    ensure_agent_exists(&store, agent)?;
    check_session(&workspace, &store, agent)?;
    store.touch_agent(agent)?;
    store.ack_task(agent, task_id)?;
    println!("Acked task {task_id}.");
    Ok(())
}

fn cmd_task_complete(agent: &str, task_id: &str, summary: &str) -> Result<()> {
    let workspace = find_workspace()?;
    let store = open_store(&workspace)?;
    ensure_agent_exists(&store, agent)?;
    check_session(&workspace, &store, agent)?;
    store.touch_agent(agent)?;
    store.complete_task(agent, task_id, summary)?;
    println!("Completed task {task_id}: {summary}");
    Ok(())
}

fn cmd_task_requeue(task_id: &str, assigned_to: Option<&str>) -> Result<()> {
    let workspace = find_workspace()?;
    let store = open_store(&workspace)?;
    store.requeue_task(task_id, assigned_to)?;
    match assigned_to {
        Some(agent) => println!("Requeued task {task_id} to {agent}."),
        None => println!("Requeued task {task_id}."),
    }
    Ok(())
}

fn cmd_task_list(options: TaskListOptions) -> Result<()> {
    let workspace = find_workspace()?;
    let store = open_store(&workspace)?;
    let tasks = store.list_tasks(options.assigned_to.as_deref(), options.status.as_deref())?;
    if tasks.is_empty() {
        println!("No tasks found.");
    } else {
        for task in tasks {
            println!("[task {}] {}", task.id, task.status,);
            println!(
                "  assigned_to: {}",
                task.assigned_to.unwrap_or_else(|| "unassigned".to_string())
            );
            println!(
                "  lease_owner: {}",
                task.lease_owner.unwrap_or_else(|| "unleased".to_string())
            );
            println!("  title: {}", task.title);
            println!("  created_by: {}", task.created_by);
            if task.body.contains('\n') {
                println!("  body:");
                for line in task.body.lines() {
                    println!("    {line}");
                }
            } else {
                println!("  body: {}", task.body);
            }
            if let Some(summary) = task.result_summary {
                println!("  result: {summary}");
            }
        }
    }
    Ok(())
}

fn cmd_pending() -> Result<()> {
    let workspace = find_workspace()?;
    let store = open_store(&workspace)?;
    let messages = store.pending_messages()?;
    if messages.is_empty() {
        println!("No pending messages.");
    } else {
        println!("Pending messages:");
        for msg in &messages {
            let preview: String = msg.content.chars().take(60).collect();
            let suffix = if msg.content.chars().count() > 60 {
                "..."
            } else {
                ""
            };
            println!(
                "  {} -> {}: {}{}",
                msg.from_agent, msg.to_agent, preview, suffix
            );
        }
    }
    Ok(())
}

fn cmd_history(options: &HistoryOptions) -> Result<()> {
    let workspace = find_workspace()?;
    let store = open_store(&workspace)?;
    let messages = store
        .all_messages(options.agent.as_deref())?
        .into_iter()
        .filter(|msg| {
            options
                .from
                .as_ref()
                .map(|from| msg.from_agent == *from)
                .unwrap_or(true)
        })
        .filter(|msg| {
            options
                .to
                .as_ref()
                .map(|to| msg.to_agent == *to)
                .unwrap_or(true)
        })
        .filter(|msg| {
            options
                .since
                .map(|since| msg.created_at >= since)
                .unwrap_or(true)
        })
        .collect::<Vec<_>>();
    if messages.is_empty() {
        println!("No message history.");
    } else {
        for msg in &messages {
            println!("{}", format_history_entry(msg));
        }
    }
    Ok(())
}

fn cmd_roles() -> Result<()> {
    let workspace = find_workspace()?;
    let roles = squad::roles::list_roles(&workspace);
    println!("Available roles:");
    for role in &roles {
        println!("  {role}");
    }
    Ok(())
}

fn cmd_teams() -> Result<()> {
    let workspace = find_workspace()?;
    let teams = squad::teams::list_teams(&workspace);
    println!("Available teams:");
    for team in &teams {
        println!("  {team}");
    }
    Ok(())
}

fn cmd_team(name: &str) -> Result<()> {
    let workspace = find_workspace()?;
    let team = squad::teams::load_team(&workspace, name)?;
    println!("Team: {}", team.name);
    println!("Roles:");
    for (role_id, role) in &team.roles {
        println!("  {role_id} (prompt: {})", role.prompt_file);
        println!("    → squad join {role_id} --role {}", role.prompt_file);
    }
    Ok(())
}

fn cmd_clean() -> Result<()> {
    let workspace = find_workspace()?;
    let db_path = workspace.join(".squad").join("messages.db");
    if db_path.exists() {
        std::fs::remove_file(&db_path)?;
    }
    // Also remove WAL and SHM files
    let wal = workspace.join(".squad").join("messages.db-wal");
    let shm = workspace.join(".squad").join("messages.db-shm");
    if wal.exists() {
        std::fs::remove_file(&wal)?;
    }
    if shm.exists() {
        std::fs::remove_file(&shm)?;
    }
    squad::session::delete_all(&workspace.join(".squad").join("sessions"))?;
    println!("Cleaned squad state.");
    Ok(())
}

fn cmd_cleanup() -> Result<()> {
    let removed = squad::setup::cleanup_commands();
    if removed.is_empty() {
        println!("No slash command files found to remove.");
    } else {
        for (name, path) in &removed {
            println!("  Removed {} → {}", name, path.display());
        }
        println!("Cleaned up {} slash command file(s).", removed.len());
    }
    Ok(())
}

fn cmd_doctor() -> Result<()> {
    let workspace = find_workspace()?;
    let store = open_store(&workspace)?;

    // 1. Template diagnostics
    let home = PathBuf::from(std::env::var("HOME").context("HOME not set")?);
    let installed_platforms: Vec<&squad::setup::Platform> = squad::setup::PLATFORMS
        .iter()
        .filter(|p| squad::setup::is_installed(p.binary))
        .collect();
    let template_diags =
        squad::setup::diagnose_templates_for_platforms(&installed_platforms, &home)?;
    for line in &template_diags {
        println!("{line}");
    }

    // 2. Archived agents with pending tasks
    let archived_pending = store.archived_agents_with_pending_tasks()?;
    if archived_pending.is_empty() {
        println!("OK: no archived agents with pending tasks");
    } else {
        for (agent_id, task_ids) in &archived_pending {
            println!(
                "WARN: archived agent {} has pending tasks: {}",
                agent_id,
                task_ids.join(", ")
            );
        }
    }

    // 3. Protocol compatibility for active agents
    let below_protocol = store.active_agents_below_protocol(
        squad::setup::SUPPORTED_PROTOCOL_VERSION,
        squad::setup::DEFAULT_PROTOCOL_VERSION,
    )?;
    if below_protocol.is_empty() {
        println!("OK: all agents meet protocol threshold");
    } else {
        for (agent_id, effective_version) in &below_protocol {
            println!(
                "WARN: {} has effective_protocol_version={}; task commands should fall back to send/receive",
                agent_id, effective_version
            );
        }
    }

    Ok(())
}

fn cmd_setup(target: Option<&str>) -> Result<()> {
    match target {
        Some("--list") => {
            println!("Supported platforms:");
            for p in squad::setup::PLATFORMS {
                let status = if squad::setup::is_installed(p.binary) {
                    "installed"
                } else {
                    "not found"
                };
                println!("  {} ({}: {})", p.name, p.binary, status);
            }
            Ok(())
        }
        Some(name) => {
            let platform = squad::setup::PLATFORMS
                .iter()
                .find(|p| p.name == name)
                .with_context(|| format!("unknown platform: {name}. Run 'squad setup --list'"))?;
            let path = squad::setup::install_for_platform(platform)?;
            println!("Installed squad for {} → {}", name, path.display());
            Ok(())
        }
        None => {
            println!("Detecting installed AI tools...");
            let results = squad::setup::run_setup();
            if results.is_empty() {
                println!("No supported AI tools found in PATH.");
                println!("Supported: claude, gemini, codex, opencode");
                return Ok(());
            }
            for (name, path, result) in &results {
                match result {
                    Ok(()) => println!("  {} → {}", name, path.display()),
                    Err(e) => println!("  {} — {}", name, e),
                }
            }
            let ok_count = results.iter().filter(|(_, _, r)| r.is_ok()).count();
            println!("Installed squad for {} tool(s).", ok_count);
            Ok(())
        }
    }
}

fn print_usage() {
    print!("{HELP_TEXT}");
}

const HELP_TEXT: &str = r#"squad — Multi-AI-agent terminal collaboration

COMMANDS
  squad init [--refresh-roles]              Initialize workspace (`--refresh-roles` rewrites builtin roles only)
  squad join <id> [--role <role>] [--client <claude|gemini|codex|opencode>] [--protocol-version <n>]
                                             Join as agent (role defaults to id; omitted metadata stays NULL)
  squad leave <id>                           Archive agent
  squad agents [--all] [--json]              List online agents (`--all` includes archived agents; `--json` emits one JSON object per line with raw/effective capability fields)
  squad send [--task-id <id>] [--reply-to <message-id>] <from> <to> <message>
                                             Send message (`squad send --file <path-or-> <from> <to>` reads from file/stdin)
  squad receive <id> [--wait] [--timeout N] [--json]
                                             Check inbox (`--wait` blocks until a message arrives, default 86400s; `--json` emits one JSON object per line)
  squad task create <from> <to> --title <title> [--body <body>]
                                             Create a structured task assignment
  squad task ack <agent> <task-id>           Acknowledge a queued task
  squad task complete <agent> <task-id> --summary <text>
                                             Complete an acked task with a result summary
  squad task requeue <task-id> [--to <agent>]
                                             Requeue a task, optionally to a new assignee
  squad task list [--agent <id>] [--status <status>]
                                             List tasks with optional filters
  squad autopilot init                       Initialize Autopilot config and generated-role/team directories
  squad autopilot plan <PRD.md>              Read a PRD and print an Autopilot task status summary
  squad autopilot run <PRD.md>               Create an Autopilot run, team, graph, sessions, and initial assignments
  squad autopilot launch --run-id <id> [--execute] [--terminal-backend <tmux|macos-terminal>]
                                             Print planned terminal sessions; `--execute` creates tmux or Terminal.app windows
  squad pending                              Show all unread messages
  squad history [agent] [--from <id>] [--to <id>] [--since <RFC3339|unix-seconds>]
                                             Show messages with timestamps and optional filters
  squad roles                                List available roles
  squad teams                                List available teams
  squad team <name>                          Show team template
  squad doctor                                 Run compatibility diagnostics (read-only)
  squad setup [platform]                      Install /squad slash command for AI tools
  squad setup --list                         List supported platforms
  squad clean                                Clear all state
  squad cleanup                              Remove installed slash commands from all AI tools

QUICK START
  1. squad init                              Set up workspace
  2. squad join manager --role manager --client claude --protocol-version 2
                                             Join as manager in terminal 1
  3. squad join worker --role worker --client codex --protocol-version 2
                                             Join as worker in terminal 2
  4. squad task create manager worker --title "task" --body "details..."
                                             Manager assigns a structured task
  5. squad receive worker                     Worker checks once for tasks
  6. squad task ack worker <task-id>          Worker claims the task
  7. squad task complete worker <task-id> --summary "done..."
                                             Worker reports structured completion

HOW TO PARTICIPATE
  When told a role (e.g. "you are manager"), run:
  1. squad join <role> --role <role>          Register and read role instructions
                                             Optional: add `--client ... --protocol-version ...` to record capabilities
  2. Do your work as instructed by the role
  3. Prefer `squad task ...` when tracking assignment state matters
  4. Use `squad send` / `squad receive` as the fallback path for freeform coordination
  5. squad receive <your-id>                  Check once for next task or feedback

EXAMPLES
  squad task create manager worker --title "auth-module" --body "implement auth module with JWT"
  squad send --task-id <task-id> inspector worker "follow-up on edge cases"
  squad send manager @all "API contract updated, rebase your work"
  squad receive worker --json
  squad history worker --from manager --since 2024-01-02T00:00:00Z
"#;
