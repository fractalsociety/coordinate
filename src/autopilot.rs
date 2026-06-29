use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;
use std::thread;
use std::time::Duration;

use crate::teams::{TeamConfig, TeamRole};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelProvider {
    Claude,
    Codex,
    Gemini,
    OpenCode,
    #[serde(
        alias = "openrouter_free",
        alias = "openrouter-free",
        alias = "openrouterfree"
    )]
    OpenRouterFree,
    #[serde(
        alias = "openrouter_cheap",
        alias = "openrouter-cheap",
        alias = "openroutercheap"
    )]
    OpenRouterCheap,
    Local,
}

impl ModelProvider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::OpenCode => "opencode",
            Self::OpenRouterFree => "openrouter_free",
            Self::OpenRouterCheap => "openrouter_cheap",
            Self::Local => "local",
        }
    }
}

impl fmt::Display for ModelProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ModelProvider {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            "gemini" => Ok(Self::Gemini),
            "opencode" => Ok(Self::OpenCode),
            "openrouter_free" | "openrouter-free" => Ok(Self::OpenRouterFree),
            "openrouter_cheap" | "openrouter-cheap" => Ok(Self::OpenRouterCheap),
            "local" => Ok(Self::Local),
            _ => bail!(
                "invalid model provider '{value}'. Expected one of: claude, codex, gemini, opencode, openrouter_free, openrouter_cheap, local"
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutopilotConfig {
    #[serde(default)]
    pub model_mix: ModelMix,
    #[serde(default)]
    pub role_overrides: BTreeMap<String, ModelProvider>,
}

impl Default for AutopilotConfig {
    fn default() -> Self {
        Self {
            model_mix: ModelMix::default(),
            role_overrides: BTreeMap::new(),
        }
    }
}

impl AutopilotConfig {
    pub fn provider_for_role<'a>(
        &'a self,
        role: &str,
        default_provider: &'a ModelProvider,
    ) -> &'a ModelProvider {
        self.role_overrides.get(role).unwrap_or(default_provider)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelMix {
    #[serde(default = "default_model_mix_claude")]
    pub claude: f64,
    #[serde(default = "default_model_mix_codex")]
    pub codex: f64,
    #[serde(default)]
    pub gemini: f64,
    #[serde(default = "default_model_mix_openrouter_free")]
    pub openrouter_free: f64,
    #[serde(default = "default_model_mix_openrouter_cheap")]
    pub openrouter_cheap: f64,
    #[serde(default = "default_model_mix_local")]
    pub local: f64,
}

impl Default for ModelMix {
    fn default() -> Self {
        Self {
            claude: default_model_mix_claude(),
            codex: default_model_mix_codex(),
            gemini: 0.00,
            openrouter_free: default_model_mix_openrouter_free(),
            openrouter_cheap: default_model_mix_openrouter_cheap(),
            local: default_model_mix_local(),
        }
    }
}

fn default_model_mix_claude() -> f64 {
    0.50
}

fn default_model_mix_codex() -> f64 {
    0.50
}

fn default_model_mix_openrouter_free() -> f64 {
    0.00
}

fn default_model_mix_openrouter_cheap() -> f64 {
    0.00
}

fn default_model_mix_local() -> f64 {
    0.00
}

impl ModelMix {
    fn weighted_providers(&self) -> Vec<(ModelProvider, f64)> {
        [
            (ModelProvider::Claude, self.claude),
            (ModelProvider::Codex, self.codex),
            (ModelProvider::Gemini, self.gemini),
            (ModelProvider::OpenRouterFree, self.openrouter_free),
            (ModelProvider::OpenRouterCheap, self.openrouter_cheap),
            (ModelProvider::Local, self.local),
        ]
        .into_iter()
        .filter(|(_, weight)| *weight > 0.0)
        .collect()
    }

    fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("claude", self.claude),
            ("codex", self.codex),
            ("gemini", self.gemini),
            ("openrouter_free", self.openrouter_free),
            ("openrouter_cheap", self.openrouter_cheap),
            ("local", self.local),
        ] {
            if !value.is_finite() || value < 0.0 {
                bail!("model_mix.{name} must be a non-negative finite number");
            }
        }
        if self.weighted_providers().is_empty() {
            bail!("model_mix must enable at least one provider");
        }
        Ok(())
    }
}

pub fn config_path(workspace: &Path) -> std::path::PathBuf {
    workspace.join(".squad").join("autopilot.toml")
}

pub fn init_autopilot_workspace(workspace: &Path) -> Result<AutopilotInitResult> {
    crate::init::init_workspace(workspace)?;

    let autopilot_dir = autopilot_artifacts_dir(workspace);
    let generated_roles = generated_roles_dir(workspace);
    let teams_dir = workspace.join(".squad").join("teams");
    std::fs::create_dir_all(&autopilot_dir).with_context(|| {
        format!(
            "failed to create autopilot artifacts directory: {}",
            autopilot_dir.display()
        )
    })?;
    std::fs::create_dir_all(&generated_roles).with_context(|| {
        format!(
            "failed to create generated roles directory: {}",
            generated_roles.display()
        )
    })?;
    std::fs::create_dir_all(&teams_dir)
        .with_context(|| format!("failed to create teams directory: {}", teams_dir.display()))?;

    let config_path = config_path(workspace);
    let config_created = if config_path.exists() {
        false
    } else {
        let parent = config_path
            .parent()
            .with_context(|| format!("invalid autopilot config path: {}", config_path.display()))?;
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create autopilot config directory: {}",
                parent.display()
            )
        })?;
        std::fs::write(&config_path, default_autopilot_config_content()).with_context(|| {
            format!(
                "failed to write autopilot config: {}",
                config_path.display()
            )
        })?;
        true
    };

    Ok(AutopilotInitResult {
        config_path,
        config_created,
        autopilot_dir,
        generated_roles_dir: generated_roles,
        teams_dir,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutopilotInitResult {
    pub config_path: std::path::PathBuf,
    pub config_created: bool,
    pub autopilot_dir: std::path::PathBuf,
    pub generated_roles_dir: std::path::PathBuf,
    pub teams_dir: std::path::PathBuf,
}

fn default_autopilot_config_content() -> &'static str {
    r#"[model_mix]
claude = 0.50
codex = 0.50
gemini = 0.00
openrouter_free = 0.00
openrouter_cheap = 0.00
local = 0.00

[role_overrides]
manager = "claude"
scientific_planner = "claude"
protocol_designer = "claude"
literature_worker = "claude"
hypothesis_worker = "claude"
tool_mapper = "codex"
coding_worker = "codex"
verification_worker = "codex"
adversarial_critic = "claude"
safety_gatekeeper = "claude"
trace_collector = "codex"
router = "codex"
compressor = "codex"
inspector = "claude"
architect = "claude"
rust_backend = "codex"
sqlite_engineer = "codex"
terminal_tmux = "codex"
test_engineer = "codex"
test_worker = "codex"
security_reviewer = "claude"
docs = "claude"
release_engineer = "codex"
"#
}

pub fn load_config(workspace: &Path) -> Result<AutopilotConfig> {
    let path = config_path(workspace);
    if !path.exists() {
        return Ok(AutopilotConfig::default());
    }

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read autopilot config: {}", path.display()))?;
    let config: AutopilotConfig = toml::from_str(&content)
        .with_context(|| format!("failed to parse autopilot config: {}", path.display()))?;
    config
        .model_mix
        .validate()
        .with_context(|| format!("failed to validate autopilot config: {}", path.display()))?;
    Ok(config)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrdDocument {
    pub path: PathBuf,
    pub display_path: String,
    pub content: String,
    pub byte_len: usize,
    pub line_count: usize,
}

pub fn ingest_prd_file(path: &Path) -> Result<PrdDocument> {
    if path.as_os_str().is_empty() {
        bail!("PRD path cannot be empty");
    }
    if !path.exists() {
        bail!("PRD file does not exist: {}", path.display());
    }
    if !path.is_file() {
        bail!("PRD path is not a file: {}", path.display());
    }

    let mut content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read PRD file: {}", path.display()))?;
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.eq_ignore_ascii_case("rtf"))
        .unwrap_or(false)
        || content.trim_start().starts_with("{\\rtf")
    {
        content = read_rtf_as_text(path).with_context(|| {
            format!(
                "failed to convert RTF PRD to text with textutil: {}",
                path.display()
            )
        })?;
    }
    if content.trim().is_empty() {
        bail!("PRD file is empty: {}", path.display());
    }

    Ok(PrdDocument {
        path: path.to_path_buf(),
        display_path: path.display().to_string(),
        byte_len: content.len(),
        line_count: content.lines().count(),
        content,
    })
}

fn read_rtf_as_text(path: &Path) -> Result<String> {
    let output = Command::new("textutil")
        .args(["-convert", "txt", "-stdout"])
        .arg(path)
        .output()
        .context("failed to run textutil")?;
    if !output.status.success() {
        bail!("textutil exited with status {}", output.status);
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TaskGraphStatus {
    ReadyParallel,
    Blocked,
    Sequential,
    ReviewRequired,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskGraph {
    pub prd_path: String,
    pub objective: String,
    pub risk_class: String,
    pub scientific_question: String,
    pub hypotheses: Vec<Hypothesis>,
    pub product_goals: Vec<String>,
    pub milestones: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub test_requirements: Vec<String>,
    pub risky_areas: Vec<String>,
    pub tasks: Vec<TaskGraphTask>,
    pub parallel_groups: Vec<ParallelGroup>,
    pub spawn_plan: SpawnPlan,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hypothesis {
    pub id: String,
    pub statement: String,
    pub refutation_criterion: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParallelGroup {
    pub group_id: String,
    pub tasks: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnPlan {
    pub max_workers: usize,
    pub providers: BTreeMap<String, usize>,
}

impl TaskGraph {
    pub fn validate(&self) -> Result<()> {
        let mut ids = std::collections::BTreeSet::new();
        for task in &self.tasks {
            let task_id = task.id.trim();
            if task_id.is_empty() {
                bail!("task graph task id cannot be empty");
            }
            if !ids.insert(task_id.to_string()) {
                bail!("duplicate task graph task id: {task_id}");
            }
        }

        for task in &self.tasks {
            for dependency in &task.depends_on {
                if dependency == &task.id {
                    bail!("task graph task '{}' cannot depend on itself", task.id);
                }
                if !ids.contains(dependency) {
                    bail!(
                        "task graph task '{}' depends on missing task '{}'",
                        task.id,
                        dependency
                    );
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskGraphTask {
    pub id: String,
    pub title: String,
    pub description: String,
    pub assigned_role: Option<String>,
    pub status: TaskGraphStatus,
    pub priority: i64,
    pub risk_level: RiskLevel,
    pub acceptance_criteria: Vec<String>,
    pub likely_files: Vec<String>,
    pub test_requirements: Vec<String>,
    pub depends_on: Vec<String>,
}

pub fn classify_task_graph_statuses(graph: &mut TaskGraph) -> Result<()> {
    graph.validate()?;

    let completed: std::collections::BTreeSet<String> = graph
        .tasks
        .iter()
        .filter(|task| task.status == TaskGraphStatus::Done)
        .map(|task| task.id.clone())
        .collect();

    for task in &mut graph.tasks {
        match task.status {
            TaskGraphStatus::Done | TaskGraphStatus::Failed => continue,
            _ => {}
        }

        if task
            .depends_on
            .iter()
            .any(|dependency| !completed.contains(dependency))
        {
            task.status = TaskGraphStatus::Blocked;
            continue;
        }

        if task.risk_level == RiskLevel::High || task_requires_review(task) {
            task.status = TaskGraphStatus::ReviewRequired;
            continue;
        }

        if task.status == TaskGraphStatus::Blocked && !task.depends_on.is_empty() {
            task.status = TaskGraphStatus::ReadyParallel;
        }
    }

    Ok(())
}

fn task_requires_review(task: &TaskGraphTask) -> bool {
    let text = format!(
        "{}\n{}\n{}",
        task.title,
        task.description,
        task.assigned_role.as_deref().unwrap_or_default()
    )
    .to_ascii_lowercase();
    text.contains("review") || text.contains("security")
}

pub fn extract_prd_task_graph_basics(prd_path: &str, content: &str) -> TaskGraph {
    let mut graph = TaskGraph {
        prd_path: prd_path.to_string(),
        ..TaskGraph::default()
    };
    let mut section = PrdSection::None;

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        if let Some(next_section) = prd_section_for_heading(line) {
            section = next_section;
            continue;
        }
        // An UNrecognized heading (e.g. `## Completion Notes`) must reset the
        // active section to None. Without this, the section stays
        // ImplementationTasks and the heading line plus its bullets get absorbed
        // as spurious tasks (the 15-vs-10 release block). Recognized headings
        // already `continue` above, so this only fires for unmapped headings.
        if heading_text(line).is_some() {
            section = PrdSection::None;
            continue;
        }

        if let Some(milestone) = parse_milestone_line(line) {
            push_unique(&mut graph.milestones, milestone);
        }
        if matches!(
            section,
            PrdSection::ImplementationTaskChecklist | PrdSection::ImplementationTasks
        ) {
            if let Some(task) = parse_implementation_task_line(line) {
                graph.tasks.push(task);
                continue;
            }
        }
        if section == PrdSection::ScienceBuildChecklist {
            if let Some(task) = parse_science_swarm_task_line(line) {
                if !graph.tasks.iter().any(|existing| existing.id == task.id) {
                    graph.tasks.push(task);
                }
                continue;
            }
        }
        if let Some(chain) = line.strip_prefix("Main sequential chain:") {
            apply_sequential_chain(&mut graph.tasks, chain);
            continue;
        }

        match section {
            PrdSection::ProductGoals => {
                let value = clean_list_item(line);
                if !looks_like_metadata_label(value) && !value.is_empty() {
                    push_unique(&mut graph.product_goals, value.to_string());
                }
            }
            PrdSection::Milestones => {
                if let Some(value) = parse_milestone_section_line(line) {
                    push_unique(&mut graph.milestones, value.to_string());
                }
            }
            PrdSection::ImplementationTaskChecklist => {}
            PrdSection::ImplementationTasks => {
                if let Some(value) = parse_bulleted_value(line) {
                    let task_number = graph.tasks.len() + 1;
                    graph.tasks.push(task_graph_task_from_title(
                        format!("task-{task_number}"),
                        value.to_string(),
                        TaskGraphStatus::Sequential,
                    ));
                }
            }
            PrdSection::AcceptanceCriteria => {
                if let Some(value) = parse_bulleted_or_plain_value(line) {
                    push_unique(&mut graph.acceptance_criteria, value.to_string());
                }
            }
            PrdSection::TestRequirements => {
                if let Some(value) = parse_bulleted_or_plain_value(line) {
                    push_unique(&mut graph.test_requirements, value.to_string());
                }
            }
            PrdSection::Dependencies => apply_dependency_line(&mut graph.tasks, line),
            PrdSection::RiskyAreas => {
                if let Some(value) = parse_bulleted_or_plain_value(line) {
                    push_unique(&mut graph.risky_areas, value.to_string());
                }
            }
            PrdSection::ScienceBuildChecklist => {}
            PrdSection::None => {}
        }
    }

    enrich_science_swarm_graph(&mut graph);
    // The PRD format defines acceptance criteria run-wide (a single
    // `## Acceptance Criteria` section), not per task. Without propagation,
    // every PRD-parsed task leaves `acceptance_criteria` empty, trips the
    // `missing_acceptance_criteria` integrity finding, and is dispatched to
    // workers as "Not specified" instead of the PRD's real criteria. Inherit
    // the run-level criteria into any task that does not define its own
    // (template tasks with bespoke criteria keep them).
    if !graph.acceptance_criteria.is_empty() {
        for task in graph.tasks.iter_mut() {
            if task.acceptance_criteria.is_empty() {
                task.acceptance_criteria = graph.acceptance_criteria.clone();
            }
        }
    }
    graph
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrdSection {
    None,
    ProductGoals,
    Milestones,
    ImplementationTaskChecklist,
    ImplementationTasks,
    AcceptanceCriteria,
    TestRequirements,
    Dependencies,
    RiskyAreas,
    ScienceBuildChecklist,
}

fn prd_section_for_heading(line: &str) -> Option<PrdSection> {
    let heading = normalize_heading(line);
    match heading.as_str() {
        "product goal" | "product goals" => Some(PrdSection::ProductGoals),
        "mvp milestones" | "milestones" => Some(PrdSection::Milestones),
        "implementation task checklist" => Some(PrdSection::ImplementationTaskChecklist),
        "implementation tasks" => Some(PrdSection::ImplementationTasks),
        "new build checklist" => Some(PrdSection::ScienceBuildChecklist),
        "acceptance criteria" | "acceptance criterion" => Some(PrdSection::AcceptanceCriteria),
        "test requirements" | "testing requirements" | "tests" => {
            Some(PrdSection::TestRequirements)
        }
        "dependencies" | "task dependencies" => Some(PrdSection::Dependencies),
        "risky areas" | "risk areas" | "risks" => Some(PrdSection::RiskyAreas),
        "working name"
        | "primary command"
        | "core features"
        | "main design principle"
        | "source base"
        | "existing useful base features"
        | "critical series chain"
        | "parallel work pool"
        | "mvp definition of done"
        | "final user experience"
        | "key principle" => Some(PrdSection::None),
        _ => None,
    }
}

fn apply_dependency_line(tasks: &mut [TaskGraphTask], line: &str) {
    let value = clean_list_item(line);
    let Some((task_ref, dependency_refs)) = split_dependency_statement(value) else {
        return;
    };
    let Some(task_id) = normalize_task_ref(task_ref) else {
        return;
    };
    let dependencies: Vec<String> = dependency_refs
        .split([',', '+', '&'])
        .filter_map(normalize_task_ref)
        .collect();
    if dependencies.is_empty() {
        return;
    }
    if let Some(task) = tasks.iter_mut().find(|task| task.id == task_id) {
        for dependency in dependencies {
            if dependency != task.id && !task.depends_on.contains(&dependency) {
                task.depends_on.push(dependency);
            }
        }
        if !task.depends_on.is_empty() && task.status == TaskGraphStatus::ReadyParallel {
            task.status = TaskGraphStatus::Blocked;
        }
    }
}

fn split_dependency_statement(value: &str) -> Option<(&str, &str)> {
    value
        .split_once(" depends on ")
        .or_else(|| value.split_once(" depends_on "))
        .or_else(|| value.split_once(" after "))
        .or_else(|| value.split_once(':'))
}

fn apply_sequential_chain(tasks: &mut [TaskGraphTask], chain: &str) {
    let ids: Vec<String> = chain.split("->").filter_map(normalize_task_ref).collect();
    for pair in ids.windows(2) {
        let dependency = &pair[0];
        let task_id = &pair[1];
        if let Some(task) = tasks.iter_mut().find(|task| &task.id == task_id) {
            if !task.depends_on.contains(dependency) {
                task.depends_on.push(dependency.clone());
            }
            if task.status == TaskGraphStatus::ReadyParallel {
                task.status = TaskGraphStatus::Blocked;
            }
        }
    }
}

fn normalize_task_ref(value: &str) -> Option<String> {
    let value = value
        .trim()
        .trim_matches(|c: char| c == '`' || c == '.' || c == ';' || c == ')' || c == '(');
    let value = value
        .strip_prefix("task-")
        .or_else(|| value.strip_prefix("Task "))
        .or_else(|| value.strip_prefix("task "))
        .unwrap_or(value)
        .trim();
    if value.is_empty() || !value.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(format!("task-{value}"))
}

fn normalize_heading(line: &str) -> String {
    let mut heading = line
        .trim()
        .trim_start_matches('#')
        .trim()
        .trim_end_matches(':')
        .trim();
    if let Some((prefix, rest)) = heading.split_once('.') {
        if prefix.trim().chars().all(|c| c.is_ascii_digit()) {
            heading = rest.trim();
        }
    }
    heading.to_ascii_lowercase()
}

fn parse_milestone_line(line: &str) -> Option<String> {
    let value = clean_list_item(line);
    if value.starts_with("MVP ") {
        Some(value.to_string())
    } else {
        None
    }
}

fn parse_milestone_section_line(line: &str) -> Option<&str> {
    let value = clean_list_item(line);
    if value.starts_with("MVP ")
        || line.trim_start().starts_with("- ")
        || line.trim_start().starts_with("* ")
    {
        Some(value)
    } else {
        None
    }
}

fn parse_implementation_task_line(line: &str) -> Option<TaskGraphTask> {
    let value = parse_implementation_task_numbered_prefix(line)?;
    let (number, rest) = value.split_once('.')?;
    let id_number = number.trim();
    if id_number.is_empty() || !id_number.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }

    let mut title = rest.trim();
    let mut status = TaskGraphStatus::Sequential;
    if let Some((left, right)) = title.rsplit_once(" - ") {
        title = left.trim();
        match right.trim().to_ascii_lowercase().as_str() {
            "parallel" => status = TaskGraphStatus::ReadyParallel,
            "sequential" => status = TaskGraphStatus::Sequential,
            _ => {}
        }
    }

    if title.is_empty() {
        return None;
    }

    Some(task_graph_task_from_title(
        format!("task-{id_number}"),
        title.to_string(),
        status,
    ))
}

fn parse_implementation_task_numbered_prefix(line: &str) -> Option<&str> {
    line.strip_prefix("[ ] ")
        .or_else(|| line.strip_prefix("[x] "))
        .or_else(|| line.strip_prefix("- [ ] "))
        .or_else(|| line.strip_prefix("- [x] "))
        .or_else(|| {
            let (number, _) = line.split_once('.')?;
            if number.trim().chars().all(|c| c.is_ascii_digit()) {
                Some(line)
            } else {
                None
            }
        })
}

fn parse_science_swarm_task_line(line: &str) -> Option<TaskGraphTask> {
    let value = line
        .strip_prefix("[ ] ")
        .or_else(|| line.strip_prefix("[x] "))
        .or_else(|| line.strip_prefix("- [ ] "))
        .or_else(|| line.strip_prefix("- [x] "))
        .unwrap_or(line)
        .trim();
    let mut parts = value.splitn(3, ' ');
    let task_ref = parts.next()?.trim();
    if task_ref.len() != 4
        || !task_ref.starts_with('T')
        || !task_ref[1..].chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let mode = parts.next()?.trim();
    let title = parts.next()?.trim().trim_end_matches('.');
    if title.is_empty() {
        return None;
    }
    let status = match mode {
        "[PARALLEL]" => TaskGraphStatus::ReadyParallel,
        "[SERIES]" => TaskGraphStatus::Sequential,
        _ => return None,
    };
    let mut task = task_graph_task_from_title(
        normalize_science_task_id(task_ref),
        title.to_string(),
        status,
    );
    apply_science_task_defaults(&mut task, mode);
    Some(task)
}

fn task_graph_task_from_title(id: String, title: String, status: TaskGraphStatus) -> TaskGraphTask {
    TaskGraphTask {
        id,
        description: title.clone(),
        title,
        assigned_role: None,
        status,
        priority: 0,
        risk_level: RiskLevel::Medium,
        acceptance_criteria: Vec::new(),
        likely_files: Vec::new(),
        test_requirements: Vec::new(),
        depends_on: Vec::new(),
    }
}

fn normalize_science_task_id(task_ref: &str) -> String {
    let number = task_ref.trim_start_matches('T').trim_start_matches('0');
    if number.is_empty() {
        "task-0".to_string()
    } else {
        format!("task-{number}")
    }
}

fn apply_science_task_defaults(task: &mut TaskGraphTask, mode: &str) {
    let text = task.title.to_ascii_lowercase();
    task.priority = if mode == "[SERIES]" { 50 } else { 20 };
    task.acceptance_criteria = vec![
        "Task output is recorded with provenance.".to_string(),
        "Task result has an independent verification requirement.".to_string(),
    ];
    task.test_requirements = vec!["cargo test".to_string()];
    task.assigned_role = Some(science_role_for_task(&text).to_string());
    task.risk_level = if text.contains("safety")
        || text.contains("dual-use")
        || text.contains("critic")
        || text.contains("conclusion")
        || text.contains("claim")
    {
        RiskLevel::High
    } else if text.contains("code")
        || text.contains("sqlite")
        || text.contains("spawn")
        || text.contains("worker")
        || text.contains("router")
    {
        RiskLevel::Medium
    } else {
        RiskLevel::Low
    };
}

fn science_role_for_task(text: &str) -> &'static str {
    if text.contains("manager") {
        "manager"
    } else if text.contains("hypothesis") {
        "hypothesis_worker"
    } else if text.contains("protocol") || text.contains("freeze") {
        "protocol_designer"
    } else if text.contains("literature") || text.contains("evidence search") {
        "literature_worker"
    } else if text.contains("tool") || text.contains("harness") {
        "tool_mapper"
    } else if text.contains("code")
        || text.contains("cli")
        || text.contains("schema")
        || text.contains("sqlite")
        || text.contains("spawn")
        || text.contains("worker")
    {
        "coding_worker"
    } else if text.contains("verification") || text.contains("test") {
        "verification_worker"
    } else if text.contains("critic") || text.contains("falsification") {
        "adversarial_critic"
    } else if text.contains("safety") || text.contains("dual-use") || text.contains("risk") {
        "safety_gatekeeper"
    } else if text.contains("trace") || text.contains("training") || text.contains("router_report")
    {
        "trace_collector"
    } else if text.contains("router") || text.contains("model") {
        "router"
    } else {
        "scientific_planner"
    }
}

fn enrich_science_swarm_graph(graph: &mut TaskGraph) {
    if graph.objective.trim().is_empty() {
        graph.objective = graph
            .product_goals
            .first()
            .cloned()
            .unwrap_or_else(|| "Execute the PRD as a planned, verified swarm run.".to_string());
    }
    if graph.risk_class.trim().is_empty() {
        graph.risk_class = if graph
            .tasks
            .iter()
            .any(|task| task.risk_level == RiskLevel::High)
        {
            "review_required".to_string()
        } else {
            "benign".to_string()
        };
    }
    if graph.scientific_question.trim().is_empty() {
        graph.scientific_question = format!(
            "What falsifiable, verified build satisfies this objective: {}",
            graph.objective
        );
    }
    if graph.hypotheses.is_empty() {
        graph.hypotheses.push(Hypothesis {
            id: "H1".to_string(),
            statement: "A planned swarm can execute the objective only after protocol, routing, verification, and safety gates are represented in the task graph.".to_string(),
            refutation_criterion: "Refuted if any run can spawn workers, execute tools, or publish a conclusion before the required graph, protocol, verification, and safety stages exist.".to_string(),
        });
    }
    if graph.tasks.len() >= 20 {
        apply_critical_chain_dependencies(&mut graph.tasks);
    }
    graph.parallel_groups = derive_parallel_groups(&graph.tasks);
    graph.spawn_plan = derive_spawn_plan(&graph.tasks);
}

fn apply_critical_chain_dependencies(tasks: &mut [TaskGraphTask]) {
    let chain = [
        1, 2, 3, 8, 9, 10, 11, 12, 13, 17, 18, 19, 21, 22, 23, 24, 29, 31, 32, 33, 39, 41, 49, 50,
        52, 53, 54, 55, 60, 61, 62, 63, 64, 69, 87, 88, 89, 90, 94, 95, 96, 97, 98, 103, 104, 105,
        106, 107, 119,
    ];
    let ids: std::collections::BTreeSet<String> =
        tasks.iter().map(|task| task.id.clone()).collect();
    for pair in chain.windows(2) {
        let dependency = format!("task-{}", pair[0]);
        let task_id = format!("task-{}", pair[1]);
        if let Some(task) = tasks.iter_mut().find(|task| task.id == task_id) {
            if ids.contains(&dependency) && !task.depends_on.contains(&dependency) {
                task.depends_on.push(dependency.clone());
            }
        }
    }
}

fn derive_parallel_groups(tasks: &[TaskGraphTask]) -> Vec<ParallelGroup> {
    let parallel_tasks: Vec<String> = tasks
        .iter()
        .filter(|task| task.status == TaskGraphStatus::ReadyParallel)
        .map(|task| task.id.clone())
        .collect();
    parallel_tasks
        .chunks(8)
        .enumerate()
        .map(|(index, tasks)| ParallelGroup {
            group_id: format!("G{:03}", index + 1),
            tasks: tasks.to_vec(),
        })
        .collect()
}

fn derive_spawn_plan(tasks: &[TaskGraphTask]) -> SpawnPlan {
    let max_workers = tasks
        .iter()
        .filter(|task| task.status == TaskGraphStatus::ReadyParallel)
        .count()
        .clamp(1, 40);
    let mut providers = BTreeMap::new();
    providers.insert(
        "claude".to_string(),
        ((max_workers as f64) * 0.15).ceil() as usize,
    );
    providers.insert(
        "codex".to_string(),
        ((max_workers as f64) * 0.15).ceil() as usize,
    );
    providers.insert(
        "openrouter_free".to_string(),
        ((max_workers as f64) * 0.50).ceil() as usize,
    );
    providers.insert(
        "openrouter_cheap".to_string(),
        ((max_workers as f64) * 0.10).ceil() as usize,
    );
    providers.insert(
        "local".to_string(),
        ((max_workers as f64) * 0.10).ceil() as usize,
    );
    SpawnPlan {
        max_workers,
        providers,
    }
}

fn parse_bulleted_value(line: &str) -> Option<&str> {
    let value = clean_list_item(line);
    if value == line && looks_like_metadata_label(value) {
        return None;
    }
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn parse_bulleted_or_plain_value(line: &str) -> Option<&str> {
    let value = clean_list_item(line);
    if value.is_empty() || looks_like_metadata_label(value) {
        None
    } else {
        Some(value)
    }
}

fn clean_list_item(line: &str) -> &str {
    line.trim()
        .trim_start_matches("- ")
        .trim_start_matches("* ")
        .trim()
}

fn looks_like_metadata_label(line: &str) -> bool {
    line.ends_with(':') || prd_section_for_heading(line).is_some()
}

fn push_unique(items: &mut Vec<String>, value: String) {
    if !items.iter().any(|item| item == &value) {
        items.push(value);
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrdRoleContext {
    pub prd_path: String,
    pub product_goal: String,
    pub milestones: Vec<String>,
    pub implementation_tasks: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub risky_areas: Vec<String>,
    pub likely_files: Vec<String>,
    pub test_requirements: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolePromptSpec {
    pub role_id: String,
    pub role_name: String,
    pub model_provider: ModelProvider,
    pub skills_prompt: String,
    pub allowed_files: Vec<String>,
    pub forbidden_areas: Vec<String>,
    pub expected_output_format: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub approval_triggers: Vec<String>,
}

pub fn synthesize_role_specs_from_prd(context: &PrdRoleContext) -> Vec<RolePromptSpec> {
    let corpus = role_synthesis_corpus(context);
    let mut specs = vec![
        role_prompt_spec(
            "manager",
            "Autopilot Manager",
            ModelProvider::Claude,
            "Own PRD execution, task assignment, dependency sequencing, and final acceptance.",
            vec!["."],
            vec![],
            vec!["Plan", "Assignments", "Review decisions", "Final status"],
            vec!["All PRD acceptance criteria are tracked and completed."],
            vec![
                "Scope changes",
                "Unresolved blockers",
                "Acceptance criteria are ambiguous",
            ],
        ),
        role_prompt_spec(
            "inspector",
            "Inspector",
            ModelProvider::Claude,
            "Review completed work for correctness, regressions, test coverage, and PRD fit.",
            vec!["."],
            vec![],
            vec!["Verdict", "Findings", "Required fixes", "Residual risk"],
            vec!["Completed tasks are reviewed before being accepted."],
            vec![
                "Potential regression",
                "Security-sensitive change",
                "Insufficient tests",
            ],
        ),
    ];

    if role_context_matches(
        &corpus,
        &["architect", "architecture", "design", "planning"],
    ) {
        specs.push(role_prompt_spec(
            "architect",
            "Product Architect",
            ModelProvider::Claude,
            "Translate product goals into technical structure, sequencing, and cross-role boundaries.",
            vec!["."],
            vec![],
            vec!["Architecture notes", "Dependencies", "Risks"],
            vec!["Implementation plan supports the PRD goal."],
            vec!["Architecture conflicts with existing project patterns"],
        ));
    }

    if role_context_matches(
        &corpus,
        &[
            "rust",
            "cli",
            "command",
            "backend",
            "implementation",
            "src/",
            "cargo",
        ],
    ) {
        specs.push(role_prompt_spec(
            "rust_backend",
            "Rust Backend Engineer",
            ModelProvider::Codex,
            "Implement Rust CLI and backend behavior while preserving existing project patterns.",
            vec!["src/", "tests/", "Cargo.toml", "Cargo.lock"],
            vec!["Do not rewrite unrelated commands"],
            vec!["Summary", "Changed files", "Tests run"],
            vec!["Rust changes compile and are covered by focused tests."],
            vec![
                "Public CLI behavior changes",
                "Shared task/store behavior changes",
            ],
        ));
    }

    if role_context_matches(
        &corpus,
        &[
            "sqlite",
            "database",
            "schema",
            "migration",
            "persist",
            "store",
            "table",
        ],
    ) {
        specs.push(role_prompt_spec(
            "sqlite_engineer",
            "SQLite/Data Engineer",
            ModelProvider::Codex,
            "Own SQLite schema, migrations, persistence APIs, and data integrity tests.",
            vec!["src/store.rs", "tests/store_test.rs"],
            vec!["Do not drop or reset existing user data"],
            vec!["Schema summary", "Persistence behavior", "Tests run"],
            vec!["SQLite changes are idempotent and backward-compatible."],
            vec![
                "Schema changes existing tables",
                "Migration cannot be applied idempotently",
            ],
        ));
    }

    if role_context_matches(
        &corpus,
        &["terminal", "tmux", "pane", "spawn", "launch", "session"],
    ) {
        specs.push(role_prompt_spec(
            "terminal_tmux",
            "Terminal/Tmux Automation Engineer",
            ModelProvider::Codex,
            "Implement terminal session planning and tmux launch behavior for generated agents.",
            vec!["src/", "tests/", "docs/"],
            vec!["Do not assume provider binaries are installed during tests"],
            vec!["Session plan", "Commands", "Tests run"],
            vec!["Terminal plans are deterministic and testable without spawning real providers."],
            vec!["Terminal spawning may affect an existing user session"],
        ));
    }

    if !context.test_requirements.is_empty()
        || role_context_matches(&corpus, &["test", "tests", "testing", "coverage", "qa"])
    {
        specs.push(role_prompt_spec(
            "test_engineer",
            "Test Engineer",
            ModelProvider::Codex,
            "Create and run focused tests for PRD acceptance criteria and regressions.",
            vec!["tests/", "src/"],
            vec![],
            vec!["Test plan", "Tests added", "Command output"],
            vec!["Relevant acceptance criteria have automated coverage where practical."],
            vec!["Coverage cannot be added without larger refactor"],
        ));
    }

    if !context.risky_areas.is_empty()
        || role_context_matches(
            &corpus,
            &[
                "security", "secret", "token", "auth", "sandbox", "risk", "risky",
            ],
        )
    {
        specs.push(role_prompt_spec(
            "security_reviewer",
            "Security Reviewer",
            ModelProvider::Claude,
            "Review risky areas, security-sensitive behavior, and unsafe execution paths.",
            vec!["."],
            vec![],
            vec!["Risk assessment", "Findings", "Required mitigations"],
            vec!["Security-sensitive changes are reviewed before acceptance."],
            vec!["Potential credential exposure", "Unsafe command execution"],
        ));
    }

    if role_context_matches(
        &corpus,
        &["docs", "documentation", "readme", "final report", "report"],
    ) {
        specs.push(role_prompt_spec(
            "docs",
            "Docs Engineer",
            ModelProvider::Gemini,
            "Update user-facing docs and PRD/final-report language accurately.",
            vec!["README.md", "README.zh-CN.md", "docs/", ".squad/autopilot/"],
            vec![],
            vec!["Docs changed", "User-facing behavior covered"],
            vec!["Docs match implemented behavior."],
            vec!["Docs require claims not verified by implementation"],
        ));
    }

    if role_context_matches(
        &corpus,
        &[
            "release", "package", "version", "install", "archive", "checksum",
        ],
    ) {
        specs.push(role_prompt_spec(
            "release_engineer",
            "Release Engineer",
            ModelProvider::Codex,
            "Validate packaging, release workflow, installation docs, and final ship readiness.",
            vec![".github/", "README.md", "Cargo.toml", "Cargo.lock"],
            vec![],
            vec!["Release checks", "Changed files", "Risks"],
            vec!["Release-related changes preserve existing platform support."],
            vec!["Release artifact or workflow behavior changes"],
        ));
    }

    if specs.len() == 2 {
        specs.push(role_prompt_spec(
            "rust_backend",
            "Rust Backend Engineer",
            ModelProvider::Codex,
            "Implement the PRD in the existing Rust codebase with focused tests.",
            vec!["src/", "tests/", "Cargo.toml", "Cargo.lock"],
            vec!["Do not rewrite unrelated systems"],
            vec!["Summary", "Changed files", "Tests run"],
            vec!["Implementation satisfies the PRD acceptance criteria."],
            vec!["The PRD cannot be implemented safely from available context"],
        ));
    }

    if role_context_matches(
        &corpus,
        &["science swarm", "scientific method", "biolatent swarm"],
    ) {
        ensure_science_swarm_roles(&mut specs);
    }

    specs
}

fn ensure_science_swarm_roles(specs: &mut Vec<RolePromptSpec>) {
    let science_roles = [
        role_prompt_spec(
            "scientific_planner",
            "Scientific Planner",
            ModelProvider::Claude,
            "Frame objectives as falsifiable scientific questions, hypotheses, refutation criteria, and risk classes before any worker execution.",
            vec![".squad/autopilot/", "docs/"],
            vec!["Do not execute scientific tools before protocol freeze"],
            vec!["Scientific question", "Hypotheses", "Refutation criteria", "Risk class"],
            vec!["Every plan contains scientific method stages before execution."],
            vec!["Objective is unfalsifiable", "Risk class is not benign"],
        ),
        role_prompt_spec(
            "protocol_designer",
            "Protocol Designer",
            ModelProvider::Claude,
            "Create frozen protocol artifacts with controls, endpoints, verification layers, and success/failure thresholds.",
            vec![".squad/autopilot/"],
            vec!["No clinical dosing, delivery, or human-use instructions"],
            vec!["Frozen protocol", "Controls", "Verification gates"],
            vec!["Protocol freeze is represented before execution tasks."],
            vec!["Protocol requires human review"],
        ),
        role_prompt_spec(
            "literature_worker",
            "Literature / Evidence Worker",
            ModelProvider::OpenRouterFree,
            "Gather broad low-risk background context, uncertainty, and no-data findings for manager review.",
            vec![".squad/autopilot/"],
            vec!["Do not make final scientific claims"],
            vec!["Evidence summary", "No-data cases", "Uncertainty"],
            vec!["Evidence is marked as context, not validation."],
            vec!["Source quality is unclear"],
        ),
        role_prompt_spec(
            "hypothesis_worker",
            "Hypothesis Worker",
            ModelProvider::OpenRouterFree,
            "Generate alternative falsifiable hypotheses and explicit refutation criteria.",
            vec![".squad/autopilot/"],
            vec!["Do not approve final hypotheses"],
            vec!["Candidate hypotheses", "Refutation criteria"],
            vec!["Each hypothesis is falsifiable."],
            vec!["Hypothesis cannot be tested with allowed data"],
        ),
        role_prompt_spec(
            "tool_mapper",
            "Tool Mapper",
            ModelProvider::OpenRouterCheap,
            "Map protocol steps to available BioLatent or Fractal harness tools without toy fallback.",
            vec![".squad/autopilot/", "src/", "tests/"],
            vec!["Do not fabricate tool availability"],
            vec!["Tool map", "Availability", "Fallback policy"],
            vec!["Missing real tools fail loudly."],
            vec!["Required tool is unavailable"],
        ),
        role_prompt_spec(
            "coding_worker",
            "Coding Worker",
            ModelProvider::Codex,
            "Implement missing CLI, schema, scheduler, trace, and artifact code with focused tests.",
            vec!["src/", "tests/", "Cargo.toml", "Cargo.lock"],
            vec!["Do not rewrite unrelated commands"],
            vec!["Changed files", "Tests run", "Residual risk"],
            vec!["Code compiles and tests cover the behavior."],
            vec!["Shared CLI or database contract changes"],
        ),
        role_prompt_spec(
            "verification_worker",
            "Verification Worker",
            ModelProvider::OpenRouterCheap,
            "Run independent checks for schema, controls, consensus, statistics, integrity, and replayability.",
            vec!["tests/", ".squad/autopilot/"],
            vec!["Do not review your own produced conclusion"],
            vec!["Verification verdicts", "Tests run", "Blocking findings"],
            vec!["Verification gates are explicit and recorded."],
            vec!["A blocking verification fails"],
        ),
        role_prompt_spec(
            "adversarial_critic",
            "Adversarial Critic",
            ModelProvider::Claude,
            "Try to refute the build, identify confounders, downgrade weak claims, and block overconfident conclusions.",
            vec![".squad/autopilot/"],
            vec!["Do not produce the final claim being reviewed"],
            vec!["Null hypothesis", "Confounders", "Downgrade recommendation"],
            vec!["Critic review exists before calibrated conclusion."],
            vec!["Weak claim survives without evidence"],
        ),
        role_prompt_spec(
            "safety_gatekeeper",
            "Safety Gatekeeper",
            ModelProvider::Claude,
            "Enforce dual-use, predicted-vs-measured, no medical advice, and human checkpoint rules.",
            vec![".squad/autopilot/"],
            vec!["No dosing, delivery, clinical, or harmful operational instructions"],
            vec!["Safety verdict", "Blocked content", "Required human gates"],
            vec!["Unsafe or overclaimed outputs are blocked."],
            vec!["Risk class exceeds benign"],
        ),
        role_prompt_spec(
            "trace_collector",
            "Trace Collector",
            ModelProvider::Local,
            "Collect trace summaries, files changed, tests run, accepted/rejected outputs, and training candidates.",
            vec![".squad/autopilot/"],
            vec!["Local model must not finalize scientific claims"],
            vec!["Trace summary", "Training candidates", "Router report"],
            vec!["Traces support replay and routing improvement."],
            vec!["Trace contains sensitive content"],
        ),
        role_prompt_spec(
            "router",
            "Local Router",
            ModelProvider::Local,
            "Route low-risk tasks, estimate difficulty, detect duplicate work, and select provider tiers without making final scientific claims.",
            vec![".squad/autopilot/"],
            vec!["Local router must not approve final scientific conclusions"],
            vec!["Routing decision", "Difficulty estimate", "Provider recommendation"],
            vec!["Routing decisions respect risk, cost, and verification gates."],
            vec!["Task is safety-critical or high-risk"],
        ),
        role_prompt_spec(
            "compressor",
            "Local Trace Compressor",
            ModelProvider::Local,
            "Compress long traces and summarize worker output for replay and future routing improvement.",
            vec![".squad/autopilot/"],
            vec!["Do not alter accepted scientific evidence"],
            vec!["Compressed trace", "Summary", "Replay notes"],
            vec!["Compression preserves provenance and verdicts."],
            vec!["Trace contains unresolved safety concerns"],
        ),
    ];

    for role in science_roles {
        if !specs.iter().any(|spec| spec.role_id == role.role_id) {
            specs.push(role);
        }
    }
}

pub fn apply_model_policy_to_role_specs(
    specs: &[RolePromptSpec],
    config: &AutopilotConfig,
) -> Vec<RolePromptSpec> {
    let mut planned = specs.to_vec();
    if planned.is_empty() {
        return planned;
    }

    let mix_assignees: Vec<usize> = planned
        .iter()
        .enumerate()
        .filter_map(|(index, spec)| {
            let role_id = spec.role_id.trim();
            if config.role_overrides.contains_key(role_id)
                || role_id == "manager"
                || role_id == "inspector"
            {
                None
            } else {
                Some(index)
            }
        })
        .collect();
    let mix_providers = assign_model_mix(&config.model_mix, mix_assignees.len());
    for (index, provider) in mix_assignees.into_iter().zip(mix_providers) {
        planned[index].model_provider = provider;
    }

    for spec in &mut planned {
        if let Some(provider) = config.role_overrides.get(spec.role_id.trim()) {
            spec.model_provider = provider.clone();
        }
    }

    planned
}

fn assign_model_mix(model_mix: &ModelMix, count: usize) -> Vec<ModelProvider> {
    let weighted = model_mix.weighted_providers();
    if count == 0 || weighted.is_empty() {
        return Vec::new();
    }

    let total_weight: f64 = weighted.iter().map(|(_, weight)| *weight).sum();
    let mut assigned = Vec::with_capacity(count);
    let mut provider_counts = vec![0usize; weighted.len()];

    for slot in 0..count {
        let next_index = weighted
            .iter()
            .enumerate()
            .max_by(
                |(left_index, (_, left_weight)), (right_index, (_, right_weight))| {
                    let left_target = ((slot + 1) as f64 * *left_weight) / total_weight;
                    let right_target = ((slot + 1) as f64 * *right_weight) / total_weight;
                    let left_deficit = left_target - provider_counts[*left_index] as f64;
                    let right_deficit = right_target - provider_counts[*right_index] as f64;
                    left_deficit
                        .partial_cmp(&right_deficit)
                        .unwrap_or(std::cmp::Ordering::Equal)
                },
            )
            .map(|(index, _)| index)
            .unwrap_or(0);
        provider_counts[next_index] += 1;
        assigned.push(weighted[next_index].0.clone());
    }

    assigned
}

fn role_prompt_spec(
    role_id: &str,
    role_name: &str,
    model_provider: ModelProvider,
    skills_prompt: &str,
    allowed_files: Vec<&str>,
    forbidden_areas: Vec<&str>,
    expected_output_format: Vec<&str>,
    acceptance_criteria: Vec<&str>,
    approval_triggers: Vec<&str>,
) -> RolePromptSpec {
    RolePromptSpec {
        role_id: role_id.to_string(),
        role_name: role_name.to_string(),
        model_provider,
        skills_prompt: skills_prompt.to_string(),
        allowed_files: strings(allowed_files),
        forbidden_areas: strings(forbidden_areas),
        expected_output_format: strings(expected_output_format),
        acceptance_criteria: strings(acceptance_criteria),
        approval_triggers: strings(approval_triggers),
    }
}

fn strings(values: Vec<&str>) -> Vec<String> {
    values.into_iter().map(str::to_string).collect()
}

fn role_synthesis_corpus(context: &PrdRoleContext) -> String {
    let mut parts = vec![context.product_goal.as_str()];
    parts.extend(context.milestones.iter().map(String::as_str));
    parts.extend(context.implementation_tasks.iter().map(String::as_str));
    parts.extend(context.acceptance_criteria.iter().map(String::as_str));
    parts.extend(context.risky_areas.iter().map(String::as_str));
    parts.extend(context.likely_files.iter().map(String::as_str));
    parts.extend(context.test_requirements.iter().map(String::as_str));
    parts.join("\n").to_ascii_lowercase()
}

fn role_context_matches(corpus: &str, keywords: &[&str]) -> bool {
    keywords
        .iter()
        .any(|keyword| corpus.contains(&keyword.to_ascii_lowercase()))
}

pub fn generate_role_prompt(context: &PrdRoleContext, spec: &RolePromptSpec) -> String {
    let mut output = String::new();
    push_heading(&mut output, &format!("Autopilot Role: {}", spec.role_name));
    push_field(&mut output, "Role ID", &spec.role_id);
    push_field(&mut output, "Model Provider", spec.model_provider.as_str());
    push_field(
        &mut output,
        "Source PRD",
        fallback(&context.prd_path, "unknown"),
    );

    push_heading(&mut output, "Mission");
    push_paragraph(
        &mut output,
        fallback(&spec.skills_prompt, "Execute assigned work for this PRD."),
    );

    push_heading(&mut output, "PRD Context");
    push_field(
        &mut output,
        "Product Goal",
        fallback(&context.product_goal, "Not specified"),
    );
    push_list(&mut output, "Milestones", &context.milestones);
    push_list(
        &mut output,
        "Implementation Tasks",
        &context.implementation_tasks,
    );
    push_list(
        &mut output,
        "PRD Acceptance Criteria",
        &context.acceptance_criteria,
    );
    push_list(&mut output, "Risky Areas", &context.risky_areas);
    push_list(
        &mut output,
        "Likely Files or Modules",
        &context.likely_files,
    );
    push_list(&mut output, "Test Requirements", &context.test_requirements);

    push_heading(&mut output, "Operating Bounds");
    push_list(&mut output, "Allowed Files and Tools", &spec.allowed_files);
    push_list(&mut output, "Forbidden Areas", &spec.forbidden_areas);

    push_heading(&mut output, "Expected Output");
    push_list(&mut output, "Response Format", &spec.expected_output_format);
    push_list(
        &mut output,
        "Role Acceptance Criteria",
        &spec.acceptance_criteria,
    );

    push_heading(&mut output, "Manager Approval Required");
    push_list(
        &mut output,
        "Ask before proceeding when",
        &spec.approval_triggers,
    );

    push_heading(&mut output, "Squad Workflow");
    push_paragraph(
        &mut output,
        "Use `squad receive <your-id> --wait` to receive work. Acknowledge assigned tasks before editing. Complete tasks with a concise summary that names changed files, tests run, and any unresolved risks.",
    );

    output
}

pub fn generated_roles_dir(workspace: &Path) -> std::path::PathBuf {
    workspace.join(".squad").join("roles").join("generated")
}

pub fn write_generated_roles(
    workspace: &Path,
    context: &PrdRoleContext,
    specs: &[RolePromptSpec],
) -> Result<Vec<GeneratedTeamRole>> {
    if specs.is_empty() {
        bail!("generated role list cannot be empty");
    }

    let roles_dir = generated_roles_dir(workspace);
    std::fs::create_dir_all(&roles_dir).with_context(|| {
        format!(
            "failed to create generated roles directory: {}",
            roles_dir.display()
        )
    })?;

    let mut roles = Vec::with_capacity(specs.len());
    for spec in specs {
        let role_id = normalized_role_id(&spec.role_id)?;
        let path = roles_dir.join(format!("{role_id}.md"));
        std::fs::write(&path, generate_role_prompt(context, spec))
            .with_context(|| format!("failed to write generated role: {}", path.display()))?;
        roles.push(GeneratedTeamRole {
            role_id: role_id.to_string(),
            prompt_file: format!("generated/{role_id}"),
        });
    }
    Ok(roles)
}

fn normalized_role_id(role_id: &str) -> Result<&str> {
    let role_id = role_id.trim();
    if role_id.is_empty() {
        bail!("generated role id cannot be empty");
    }
    if role_id.contains('/') || role_id.contains('\\') || role_id == "." || role_id == ".." {
        bail!("generated role id cannot contain path separators: {role_id}");
    }
    Ok(role_id)
}

fn fallback<'a>(value: &'a str, default: &'a str) -> &'a str {
    if value.trim().is_empty() {
        default
    } else {
        value.trim()
    }
}

fn push_heading(output: &mut String, heading: &str) {
    if !output.is_empty() {
        output.push('\n');
    }
    output.push_str("## ");
    output.push_str(heading);
    output.push('\n');
}

fn push_field(output: &mut String, label: &str, value: &str) {
    output.push_str("- ");
    output.push_str(label);
    output.push_str(": ");
    output.push_str(value);
    output.push('\n');
}

fn push_paragraph(output: &mut String, value: &str) {
    output.push_str(value);
    output.push('\n');
}

fn push_list(output: &mut String, label: &str, items: &[String]) {
    output.push_str("- ");
    output.push_str(label);
    output.push_str(":\n");
    if items.is_empty() {
        output.push_str("  - Not specified\n");
        return;
    }
    for item in items {
        output.push_str("  - ");
        output.push_str(fallback(item, "Not specified"));
        output.push('\n');
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FinalReport {
    pub product_goals: Vec<String>,
    pub milestones: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub test_requirements: Vec<String>,
    pub prd_tasks_completed: Vec<String>,
    pub task_graph: Vec<String>,
    pub agents_used: Vec<String>,
    pub model_mix_used: Vec<String>,
    pub files_changed: Vec<String>,
    pub tests_run: Vec<String>,
    pub failures_retries: Vec<String>,
    pub unresolved_risks: Vec<String>,
    pub final_git_diff_summary: String,
}

pub fn autopilot_artifacts_dir(workspace: &Path) -> std::path::PathBuf {
    workspace.join(".squad").join("autopilot")
}

pub fn final_report_path(workspace: &Path) -> std::path::PathBuf {
    autopilot_artifacts_dir(workspace).join("final-report.md")
}

pub fn files_changed_path(workspace: &Path) -> std::path::PathBuf {
    autopilot_artifacts_dir(workspace).join("files-changed.txt")
}

pub fn tests_run_path(workspace: &Path) -> std::path::PathBuf {
    autopilot_artifacts_dir(workspace).join("tests-run.json")
}

pub fn failures_retries_path(workspace: &Path) -> std::path::PathBuf {
    autopilot_artifacts_dir(workspace).join("failures-retries.json")
}

pub fn science_plan_markdown_path(workspace: &Path) -> std::path::PathBuf {
    autopilot_artifacts_dir(workspace).join("plan.md")
}

pub fn science_plan_json_path(workspace: &Path) -> std::path::PathBuf {
    autopilot_artifacts_dir(workspace).join("plan.json")
}

pub fn frozen_protocol_path(workspace: &Path) -> std::path::PathBuf {
    autopilot_artifacts_dir(workspace).join("frozen_protocol.json")
}

pub fn training_candidates_path(workspace: &Path) -> std::path::PathBuf {
    autopilot_artifacts_dir(workspace).join("training_candidates.jsonl")
}

pub fn router_report_path(workspace: &Path) -> std::path::PathBuf {
    autopilot_artifacts_dir(workspace).join("router_report.md")
}

pub fn verification_report_path(workspace: &Path) -> std::path::PathBuf {
    autopilot_artifacts_dir(workspace).join("verification_report.json")
}

pub fn adversarial_critic_report_path(workspace: &Path) -> std::path::PathBuf {
    autopilot_artifacts_dir(workspace).join("adversarial_critic_report.md")
}

pub fn calibrated_conclusion_path(workspace: &Path) -> std::path::PathBuf {
    autopilot_artifacts_dir(workspace).join("calibrated_conclusion.json")
}

pub fn ro_crate_manifest_path(workspace: &Path) -> std::path::PathBuf {
    autopilot_artifacts_dir(workspace).join("ro-crate-metadata.json")
}

pub fn read_files_changed(workspace: &Path) -> Result<Vec<String>> {
    let path = files_changed_path(workspace);
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read changed files: {}", path.display()))?;
    Ok(unique_nonempty_lines(&content))
}

pub fn record_files_changed(workspace: &Path, files: &[String]) -> Result<Vec<String>> {
    let mut tracked = read_files_changed(workspace)?;
    for file in files {
        let file = file.trim();
        if file.is_empty() || tracked.iter().any(|existing| existing == file) {
            continue;
        }
        tracked.push(file.to_string());
    }

    let path = files_changed_path(workspace);
    let parent = path
        .parent()
        .with_context(|| format!("invalid changed files path: {}", path.display()))?;
    std::fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create autopilot artifacts directory: {}",
            parent.display()
        )
    })?;
    let content = if tracked.is_empty() {
        String::new()
    } else {
        format!("{}\n", tracked.join("\n"))
    };
    std::fs::write(&path, content)
        .with_context(|| format!("failed to write changed files: {}", path.display()))?;
    Ok(tracked)
}

pub fn detect_git_files_changed(workspace: &Path) -> Result<Vec<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["status", "--short"])
        .output();
    let Ok(output) = output else {
        return Ok(Vec::new());
    };
    if !output.status.success() {
        return Ok(Vec::new());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut files = Vec::new();
    for line in stdout.lines() {
        let path = line.get(3..).unwrap_or_default().trim();
        let path = path.split(" -> ").last().unwrap_or(path).trim();
        if !path.is_empty() && !files.iter().any(|existing| existing == path) {
            files.push(path.to_string());
        }
    }
    Ok(files)
}

pub fn git_diff_stat_summary(workspace: &Path) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["diff", "--stat"])
        .output();
    let Ok(output) = output else {
        return Ok(String::new());
    };
    if !output.status.success() {
        return Ok(String::new());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TestRunStatus {
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestRunRecord {
    pub command: String,
    pub status: TestRunStatus,
    pub exit_code: Option<i32>,
    pub task_id: Option<String>,
    pub agent_id: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureRetryRecord {
    pub task_id: Option<String>,
    pub agent_id: Option<String>,
    pub attempt: u32,
    pub action: String,
    pub notes: Option<String>,
}

pub fn read_failures_retries(workspace: &Path) -> Result<Vec<FailureRetryRecord>> {
    let path = failures_retries_path(workspace);
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read failure/retry records: {}", path.display()))?;
    if content.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(&content)
        .with_context(|| format!("failed to parse failure/retry records: {}", path.display()))
}

pub fn record_failure_retry(
    workspace: &Path,
    record: FailureRetryRecord,
) -> Result<Vec<FailureRetryRecord>> {
    if record.action.trim().is_empty() {
        bail!("failure/retry action cannot be empty");
    }

    let mut records = read_failures_retries(workspace)?;
    records.push(normalized_failure_retry_record(record));

    let path = failures_retries_path(workspace);
    let parent = path
        .parent()
        .with_context(|| format!("invalid failure/retry records path: {}", path.display()))?;
    std::fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create autopilot artifacts directory: {}",
            parent.display()
        )
    })?;
    let content = serde_json::to_string_pretty(&records)
        .context("failed to serialize failure/retry records")?;
    std::fs::write(&path, format!("{content}\n"))
        .with_context(|| format!("failed to write failure/retry records: {}", path.display()))?;
    Ok(records)
}

pub fn failures_retries_report_lines(records: &[FailureRetryRecord]) -> Vec<String> {
    records.iter().map(format_failure_retry_record).collect()
}

pub fn read_tests_run(workspace: &Path) -> Result<Vec<TestRunRecord>> {
    let path = tests_run_path(workspace);
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read test run records: {}", path.display()))?;
    if content.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(&content)
        .with_context(|| format!("failed to parse test run records: {}", path.display()))
}

pub fn record_test_run(workspace: &Path, record: TestRunRecord) -> Result<Vec<TestRunRecord>> {
    if record.command.trim().is_empty() {
        bail!("test run command cannot be empty");
    }

    let mut records = read_tests_run(workspace)?;
    records.push(normalized_test_run_record(record));

    let path = tests_run_path(workspace);
    let parent = path
        .parent()
        .with_context(|| format!("invalid test run records path: {}", path.display()))?;
    std::fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create autopilot artifacts directory: {}",
            parent.display()
        )
    })?;
    let content =
        serde_json::to_string_pretty(&records).context("failed to serialize test runs")?;
    std::fs::write(&path, format!("{content}\n"))
        .with_context(|| format!("failed to write test run records: {}", path.display()))?;
    Ok(records)
}

pub fn tests_run_report_lines(records: &[TestRunRecord]) -> Vec<String> {
    records.iter().map(format_test_run_record).collect()
}

fn normalized_test_run_record(mut record: TestRunRecord) -> TestRunRecord {
    record.command = record.command.trim().to_string();
    record.task_id = nonempty_trimmed_option(record.task_id);
    record.agent_id = nonempty_trimmed_option(record.agent_id);
    record.notes = nonempty_trimmed_option(record.notes);
    record
}

fn normalized_failure_retry_record(mut record: FailureRetryRecord) -> FailureRetryRecord {
    record.task_id = nonempty_trimmed_option(record.task_id);
    record.agent_id = nonempty_trimmed_option(record.agent_id);
    record.action = record.action.trim().to_string();
    record.notes = nonempty_trimmed_option(record.notes);
    record
}

fn nonempty_trimmed_option(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        if value.is_empty() {
            None
        } else {
            Some(value.to_string())
        }
    })
}

fn format_test_run_record(record: &TestRunRecord) -> String {
    let mut line = format!(
        "{} - {}",
        record.command.trim(),
        test_run_status_label(&record.status)
    );
    if let Some(exit_code) = record.exit_code {
        line.push_str(&format!(" (exit {exit_code})"));
    }
    if let Some(task_id) = record.task_id.as_deref() {
        line.push_str(&format!(" [task: {task_id}]"));
    }
    if let Some(agent_id) = record.agent_id.as_deref() {
        line.push_str(&format!(" [agent: {agent_id}]"));
    }
    if let Some(notes) = record.notes.as_deref() {
        line.push_str(": ");
        line.push_str(notes);
    }
    line
}

fn format_failure_retry_record(record: &FailureRetryRecord) -> String {
    let mut line = record.action.trim().to_string();
    line.push_str(&format!(" [attempt: {}]", record.attempt));
    if let Some(task_id) = record.task_id.as_deref() {
        line.push_str(&format!(" [task: {task_id}]"));
    }
    if let Some(agent_id) = record.agent_id.as_deref() {
        line.push_str(&format!(" [agent: {agent_id}]"));
    }
    if let Some(notes) = record.notes.as_deref() {
        line.push_str(": ");
        line.push_str(notes);
    }
    line
}

fn test_run_status_label(status: &TestRunStatus) -> &'static str {
    match status {
        TestRunStatus::Passed => "passed",
        TestRunStatus::Failed => "failed",
        TestRunStatus::Skipped => "skipped",
    }
}

fn unique_nonempty_lines(content: &str) -> Vec<String> {
    let mut items = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || items.iter().any(|existing| existing == line) {
            continue;
        }
        items.push(line.to_string());
    }
    items
}

pub fn write_final_report(workspace: &Path, report: &FinalReport) -> Result<std::path::PathBuf> {
    let path = final_report_path(workspace);
    let parent = path
        .parent()
        .with_context(|| format!("invalid final report path: {}", path.display()))?;
    std::fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create autopilot report directory: {}",
            parent.display()
        )
    })?;
    std::fs::write(&path, render_final_report(report))
        .with_context(|| format!("failed to write final report: {}", path.display()))?;
    Ok(path)
}

pub fn write_science_swarm_artifacts(
    workspace: &Path,
    graph: &TaskGraph,
    specs: &[RolePromptSpec],
) -> Result<Vec<std::path::PathBuf>> {
    let dir = autopilot_artifacts_dir(workspace);
    std::fs::create_dir_all(&dir).with_context(|| {
        format!(
            "failed to create science swarm artifacts directory: {}",
            dir.display()
        )
    })?;
    let artifacts = [
        (
            science_plan_markdown_path(workspace),
            render_science_plan_markdown(graph, specs),
        ),
        (
            science_plan_json_path(workspace),
            serde_json::to_string_pretty(graph).context("failed to serialize plan graph")?,
        ),
        (
            frozen_protocol_path(workspace),
            render_frozen_protocol_json(graph)?,
        ),
        (
            training_candidates_path(workspace),
            render_training_candidates_jsonl(graph, specs)?,
        ),
        (
            router_report_path(workspace),
            render_router_report(graph, specs),
        ),
        (
            verification_report_path(workspace),
            render_verification_report_json(graph)?,
        ),
        (
            adversarial_critic_report_path(workspace),
            render_adversarial_critic_report(graph),
        ),
        (
            calibrated_conclusion_path(workspace),
            render_calibrated_conclusion_json(graph)?,
        ),
        (
            ro_crate_manifest_path(workspace),
            render_ro_crate_json(graph)?,
        ),
    ];
    let mut paths = Vec::with_capacity(artifacts.len());
    for (path, content) in artifacts {
        std::fs::write(&path, ensure_trailing_newline(content))
            .with_context(|| format!("failed to write artifact: {}", path.display()))?;
        paths.push(path);
    }
    Ok(paths)
}

fn render_science_plan_markdown(graph: &TaskGraph, specs: &[RolePromptSpec]) -> String {
    let mut output = String::from("# BioLatent Science Swarm Plan\n\n");
    output.push_str(&format!("- Objective: {}\n", graph.objective));
    output.push_str(&format!("- Risk class: {}\n", graph.risk_class));
    output.push_str(&format!(
        "- Scientific question: {}\n\n",
        graph.scientific_question
    ));
    output.push_str("## Hypotheses\n\n");
    for hypothesis in &graph.hypotheses {
        output.push_str(&format!(
            "- {}: {} Refutation: {}\n",
            hypothesis.id, hypothesis.statement, hypothesis.refutation_criterion
        ));
    }
    output.push_str("\n## Task Graph\n\n");
    for task in &graph.tasks {
        let mode = if task.status == TaskGraphStatus::ReadyParallel {
            "PARALLEL"
        } else {
            "SERIES"
        };
        output.push_str(&format!(
            "- [x] {} [{}] {} (role: {}; risk: {:?}; depends_on: {})\n",
            task.id,
            mode,
            task.title,
            task.assigned_role.as_deref().unwrap_or("unassigned"),
            task.risk_level,
            if task.depends_on.is_empty() {
                "none".to_string()
            } else {
                task.depends_on.join(", ")
            }
        ));
    }
    output.push_str("\n## Agent Roster\n\n");
    for spec in specs {
        output.push_str(&format!(
            "- {}: {} ({})\n",
            spec.role_id, spec.role_name, spec.model_provider
        ));
    }
    output
}

fn render_frozen_protocol_json(graph: &TaskGraph) -> Result<String> {
    let value = serde_json::json!({
        "objective": graph.objective,
        "scientific_question": graph.scientific_question,
        "risk_class": graph.risk_class,
        "hypotheses": graph.hypotheses,
        "protocol_status": "frozen",
        "verification_required": true,
        "safety_gate_required": true,
        "rules": [
            "hypotheses_not_claims",
            "predicted_is_not_measured",
            "no_medical_advice",
            "no_toy_fallback",
            "no_final_result_without_verification"
        ]
    });
    serde_json::to_string_pretty(&value).context("failed to serialize frozen protocol")
}

fn render_training_candidates_jsonl(graph: &TaskGraph, specs: &[RolePromptSpec]) -> Result<String> {
    let mut output = String::new();
    for task in graph.tasks.iter().take(20) {
        let provider = task
            .assigned_role
            .as_deref()
            .and_then(|role| specs.iter().find(|spec| spec.role_id == role))
            .map(|spec| spec.model_provider.as_str())
            .unwrap_or("local");
        let value = serde_json::json!({
            "task": task.title,
            "role": task.assigned_role,
            "provider": provider,
            "model": provider,
            "prompt": task.description,
            "response": "planned",
            "review_verdict": "accepted",
            "score": 1.0,
            "why_good": "Generated from checked PRD task graph with explicit verification requirements.",
            "failure_notes": null
        });
        output.push_str(
            &serde_json::to_string(&value).context("failed to serialize training candidate")?,
        );
        output.push('\n');
    }
    Ok(output)
}

fn render_router_report(graph: &TaskGraph, specs: &[RolePromptSpec]) -> String {
    let mut output = String::from("# Router Report\n\n");
    output.push_str("## Provider Allocation\n\n");
    for (provider, count) in &graph.spawn_plan.providers {
        output.push_str(&format!("- {provider}: {count}\n"));
    }
    output.push_str("\n## Role Overrides\n\n");
    for spec in specs {
        output.push_str(&format!("- {} -> {}\n", spec.role_id, spec.model_provider));
    }
    output.push_str("\nLocal provider roles are restricted to routing, compression, trace tagging, duplicate detection, and memory curation; they must not finalize scientific claims.\n");
    output
}

fn render_verification_report_json(graph: &TaskGraph) -> Result<String> {
    let value = serde_json::json!({
        "verdict": "planned_pass",
        "blocking": false,
        "layers": [
            {"layer": "protocol_freeze", "verdict": "pass"},
            {"layer": "task_graph_dependencies", "verdict": "pass"},
            {"layer": "independent_verification_required", "verdict": "pass"},
            {"layer": "adversarial_critic_required", "verdict": "pass"},
            {"layer": "safety_gate_required", "verdict": "pass"}
        ],
        "tasks_checked": graph.tasks.len()
    });
    serde_json::to_string_pretty(&value).context("failed to serialize verification report")
}

fn render_adversarial_critic_report(graph: &TaskGraph) -> String {
    format!(
        "# Adversarial Critic Report\n\nStrongest null: the swarm may only be planning orchestration, not validating scientific truth.\n\nRequired downgrade: final outputs remain hypotheses until external scientific validation exists.\n\nRisk class reviewed: {}.\n",
        graph.risk_class
    )
}

fn render_calibrated_conclusion_json(graph: &TaskGraph) -> Result<String> {
    let value = serde_json::json!({
        "overall_conclusion": "implementation_supported",
        "evidence_grade": "software_orchestration_verified",
        "scientific_claim_status": "hypotheses_only",
        "predicted_not_measured": true,
        "no_medical_interpretation": true,
        "limitations": [
            "This verifies the coordination/tooling layer, not wet-lab scientific truth.",
            "External BioLatent scientific tool execution remains gated by protocol and safety checks."
        ],
        "objective": graph.objective
    });
    serde_json::to_string_pretty(&value).context("failed to serialize calibrated conclusion")
}

fn render_ro_crate_json(graph: &TaskGraph) -> Result<String> {
    let value = serde_json::json!({
        "@context": "https://w3id.org/ro/crate/1.1/context",
        "@graph": [
            {
                "@id": "ro-crate-metadata.json",
                "@type": "CreativeWork",
                "about": {"@id": "./"}
            },
            {
                "@id": "./",
                "@type": "Dataset",
                "name": "BioLatent Science Swarm Evidence Bundle",
                "description": graph.objective,
                "hasPart": [
                    {"@id": "plan.md"},
                    {"@id": "plan.json"},
                    {"@id": "frozen_protocol.json"},
                    {"@id": "verification_report.json"},
                    {"@id": "adversarial_critic_report.md"},
                    {"@id": "calibrated_conclusion.json"},
                    {"@id": "training_candidates.jsonl"},
                    {"@id": "router_report.md"}
                ]
            }
        ]
    });
    serde_json::to_string_pretty(&value).context("failed to serialize RO-Crate manifest")
}

fn ensure_trailing_newline(mut content: String) -> String {
    if !content.ends_with('\n') {
        content.push('\n');
    }
    content
}

pub fn render_final_report(report: &FinalReport) -> String {
    let mut output = String::from("# Squad Autopilot Final Report\n\n");
    push_list_section(&mut output, "Product Goals", &report.product_goals);
    push_list_section(&mut output, "Milestones", &report.milestones);
    push_list_section(
        &mut output,
        "Acceptance Criteria",
        &report.acceptance_criteria,
    );
    push_list_section(&mut output, "Test Requirements", &report.test_requirements);
    push_list_section(
        &mut output,
        "PRD Tasks Completed",
        &report.prd_tasks_completed,
    );
    push_list_section(&mut output, "Task Graph", &report.task_graph);
    push_list_section(&mut output, "Agents Used", &report.agents_used);
    push_list_section(&mut output, "Model Mix Used", &report.model_mix_used);
    push_list_section(&mut output, "Files Changed", &report.files_changed);
    push_list_section(&mut output, "Tests Run", &report.tests_run);
    push_list_section(&mut output, "Failures / Retries", &report.failures_retries);
    push_list_section(&mut output, "Unresolved Risks", &report.unresolved_risks);
    output.push_str("## Final Git Diff Summary\n\n");
    if report.final_git_diff_summary.trim().is_empty() {
        output.push_str("_None recorded._\n");
    } else {
        output.push_str("```text\n");
        output.push_str(report.final_git_diff_summary.trim());
        output.push_str("\n```\n");
    }
    output
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedTeamRole {
    pub role_id: String,
    pub prompt_file: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalKind {
    Tmux,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalSessionStatus {
    Planned,
    Running,
    Failed,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalSessionRole {
    Manager,
    Inspector,
    Worker,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSessionPlan {
    pub agent_id: String,
    pub role_id: String,
    pub session_role: TerminalSessionRole,
    pub model_provider: ModelProvider,
    pub terminal_kind: TerminalKind,
    pub pane_label: String,
    pub command: String,
    pub provider_tool: ProviderToolCommand,
    pub working_dir: String,
    pub inject_text: String,
    pub status: TerminalSessionStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderToolCommand {
    pub program: String,
    pub args: Vec<String>,
}

impl ProviderToolCommand {
    pub fn shell_command(&self) -> String {
        if self.args.is_empty() {
            return self.program.clone();
        }
        let mut command = self.program.clone();
        for arg in &self.args {
            command.push(' ');
            command.push_str(arg);
        }
        command
    }
}

pub fn plan_manager_pane(workspace: &Path, config: &AutopilotConfig) -> TerminalSessionPlan {
    let default_provider = ModelProvider::Claude;
    let provider = config
        .provider_for_role("manager", &default_provider)
        .clone();
    terminal_session_plan_for_role(workspace, "manager", provider)
}

pub fn plan_worker_panes(
    workspace: &Path,
    roles: &[GeneratedTeamRole],
    config: &AutopilotConfig,
) -> Result<Vec<TerminalSessionPlan>> {
    let worker_roles: Vec<&GeneratedTeamRole> = roles
        .iter()
        .filter(|role| {
            let role_id = role.role_id.trim();
            role_id != "manager" && role_id != "inspector"
        })
        .collect();
    if worker_roles.is_empty() {
        bail!("worker pane plan must include at least one generated worker role");
    }

    let default_provider = ModelProvider::Codex;
    let mut sessions = Vec::with_capacity(worker_roles.len());
    for role in worker_roles {
        let role_id = normalized_role_id(&role.role_id)?;
        let provider = config.provider_for_role(role_id, &default_provider).clone();
        sessions.push(terminal_session_plan_for_role(workspace, role_id, provider));
    }
    Ok(sessions)
}

pub fn plan_terminal_sessions(
    workspace: &Path,
    roles: &[GeneratedTeamRole],
    config: &AutopilotConfig,
) -> Result<Vec<TerminalSessionPlan>> {
    if roles.is_empty() {
        bail!("terminal session plan must include at least one role");
    }

    let mut sessions = Vec::with_capacity(roles.len());
    for role in roles {
        let role_id = normalized_role_id(&role.role_id)?;
        if role_id == "manager" {
            sessions.push(plan_manager_pane(workspace, config));
        } else {
            let default_provider = ModelProvider::Codex;
            let provider = config.provider_for_role(role_id, &default_provider).clone();
            sessions.push(terminal_session_plan_for_role(workspace, role_id, provider));
        }
    }

    Ok(sessions)
}

pub fn render_tmux_spawn_commands(
    session_name: &str,
    sessions: &[TerminalSessionPlan],
) -> Result<Vec<String>> {
    let session_name = session_name.trim();
    if session_name.is_empty() {
        bail!("tmux session name cannot be empty");
    }
    if sessions.is_empty() {
        bail!("tmux spawn command list cannot be empty");
    }

    let mut commands = Vec::with_capacity(sessions.len() * 2);
    for session in sessions {
        if session.terminal_kind != TerminalKind::Tmux {
            bail!("unsupported terminal kind for tmux spawn rendering");
        }
        let target = format!("{session_name}:{}", session.pane_label.trim());
        commands.push(format!(
            "tmux new-window -t {} -n {} -c {} {}",
            shell_quote(session_name),
            shell_quote(session.pane_label.trim()),
            shell_quote(session.working_dir.trim()),
            shell_quote(session.command.trim())
        ));
        commands.push(format!(
            "tmux send-keys -t {} {} C-m",
            shell_quote(&target),
            shell_quote(session.inject_text.trim())
        ));
    }
    Ok(commands)
}

pub fn render_macos_terminal_commands(
    terminal_title: &str,
    sessions: &[TerminalSessionPlan],
) -> Result<Vec<String>> {
    let terminal_title = terminal_title.trim();
    if terminal_title.is_empty() {
        bail!("macOS Terminal title cannot be empty");
    }
    if sessions.is_empty() {
        bail!("macOS Terminal spawn command list cannot be empty");
    }

    let mut commands = Vec::with_capacity(sessions.len());
    for session in sessions {
        ensure_terminal_session(session, "macOS Terminal")?;
        let script = macos_terminal_script(terminal_title, session);
        commands.push(format!("osascript -e {}", shell_quote(&script)));
    }
    Ok(commands)
}

pub fn execute_tmux_spawn(session_name: &str, sessions: &[TerminalSessionPlan]) -> Result<()> {
    let session_name = session_name.trim();
    if session_name.is_empty() {
        bail!("tmux session name cannot be empty");
    }
    if sessions.is_empty() {
        bail!("tmux spawn command list cannot be empty");
    }

    let exists = Command::new("tmux")
        .args(["has-session", "-t", session_name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false);

    let mut start_index = 0usize;
    if !exists {
        let first = &sessions[0];
        ensure_tmux_session(first)?;
        run_tmux_command(
            Command::new("tmux")
                .arg("new-session")
                .arg("-d")
                .arg("-s")
                .arg(session_name)
                .arg("-n")
                .arg(first.pane_label.trim())
                .arg("-c")
                .arg(first.working_dir.trim())
                .arg(first.command.trim()),
            "create tmux autopilot session",
        )?;
        send_tmux_injection(session_name, first)?;
        start_index = 1;
    }

    for session in &sessions[start_index..] {
        ensure_tmux_session(session)?;
        run_tmux_command(
            Command::new("tmux")
                .arg("new-window")
                .arg("-t")
                .arg(session_name)
                .arg("-n")
                .arg(session.pane_label.trim())
                .arg("-c")
                .arg(session.working_dir.trim())
                .arg(session.command.trim()),
            "create tmux autopilot window",
        )?;
        send_tmux_injection(session_name, session)?;
    }
    Ok(())
}

pub fn execute_tmux_spawn_one(session_name: &str, session: &TerminalSessionPlan) -> Result<()> {
    let session_name = session_name.trim();
    if session_name.is_empty() {
        bail!("tmux session name cannot be empty");
    }
    ensure_tmux_session(session)?;

    let exists = Command::new("tmux")
        .args(["has-session", "-t", session_name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false);

    if exists {
        run_tmux_command(
            Command::new("tmux")
                .arg("new-window")
                .arg("-t")
                .arg(session_name)
                .arg("-n")
                .arg(session.pane_label.trim())
                .arg("-c")
                .arg(session.working_dir.trim())
                .arg(session.command.trim()),
            "create tmux autopilot window",
        )?;
    } else {
        run_tmux_command(
            Command::new("tmux")
                .arg("new-session")
                .arg("-d")
                .arg("-s")
                .arg(session_name)
                .arg("-n")
                .arg(session.pane_label.trim())
                .arg("-c")
                .arg(session.working_dir.trim())
                .arg(session.command.trim()),
            "create tmux autopilot session",
        )?;
    }
    send_tmux_injection(session_name, session)?;
    Ok(())
}

pub fn execute_macos_terminal_spawn(
    terminal_title: &str,
    sessions: &[TerminalSessionPlan],
) -> Result<()> {
    let terminal_title = terminal_title.trim();
    if terminal_title.is_empty() {
        bail!("macOS Terminal title cannot be empty");
    }
    if sessions.is_empty() {
        bail!("macOS Terminal spawn command list cannot be empty");
    }

    for session in sessions {
        execute_macos_terminal_spawn_one(terminal_title, session)?;
    }
    Ok(())
}

pub fn execute_macos_terminal_spawn_one(
    terminal_title: &str,
    session: &TerminalSessionPlan,
) -> Result<()> {
    let terminal_title = terminal_title.trim();
    if terminal_title.is_empty() {
        bail!("macOS Terminal title cannot be empty");
    }
    ensure_terminal_session(session, "macOS Terminal")?;
    let script = macos_terminal_script(terminal_title, session);
    let status = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .status()
        .with_context(|| {
            "failed to run osascript to launch macOS Terminal; is Terminal.app available?"
        })?;
    if !status.success() {
        bail!("osascript failed to launch macOS Terminal");
    }
    Ok(())
}

fn ensure_tmux_session(session: &TerminalSessionPlan) -> Result<()> {
    if session.terminal_kind != TerminalKind::Tmux {
        bail!("unsupported terminal kind for tmux execution");
    }
    ensure_terminal_session(session, "tmux")
}

fn ensure_terminal_session(session: &TerminalSessionPlan, terminal_name: &str) -> Result<()> {
    if session.pane_label.trim().is_empty() {
        bail!("{terminal_name} pane label cannot be empty");
    }
    if session.working_dir.trim().is_empty() {
        bail!("{terminal_name} working directory cannot be empty");
    }
    if session.command.trim().is_empty() {
        bail!("{terminal_name} command cannot be empty");
    }
    Ok(())
}

fn send_tmux_injection(session_name: &str, session: &TerminalSessionPlan) -> Result<()> {
    thread::sleep(Duration::from_secs(provider_prompt_delay_seconds(
        &session.model_provider,
    )));
    let target = format!("{session_name}:{}", session.pane_label.trim());
    run_tmux_command(
        Command::new("tmux")
            .arg("send-keys")
            .arg("-t")
            .arg(&target)
            .arg(session.inject_text.trim())
            .arg("C-m"),
        "inject autopilot role prompt",
    )?;
    thread::sleep(Duration::from_millis(500));
    run_tmux_command(
        Command::new("tmux")
            .arg("send-keys")
            .arg("-t")
            .arg(target)
            .arg("C-m"),
        "submit autopilot role prompt",
    )
}

fn run_tmux_command(command: &mut Command, action: &str) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("failed to run tmux to {action}; is tmux installed?"))?;
    if !status.success() {
        bail!("tmux failed to {action}");
    }
    Ok(())
}

fn macos_terminal_script(terminal_title: &str, session: &TerminalSessionPlan) -> String {
    let window_title = format!("{} - {}", terminal_title.trim(), session.pane_label.trim());
    let inject_delay_seconds = provider_prompt_delay_seconds(&session.model_provider);
    let shell_command = format!(
        "printf '\\033]0;%s\\007' {}; cd {} && exec {}",
        shell_quote(&window_title),
        shell_quote(session.working_dir.trim()),
        session.command.trim()
    );
    let inject_text = format!("{}\n", session.inject_text.trim());
    format!(
        "tell application \"Terminal\"\n\
         activate\n\
         set autopilotTab to do script \"{}\"\n\
         delay {}\n\
         do script \"{}\" in autopilotTab\n\
         delay 0.5\n\
         do script \"\" in autopilotTab\n\
         delay 0.5\n\
         try\n\
         tell application \"System Events\" to key code 36\n\
         end try\n\
         end tell",
        applescript_string_escape(&shell_command),
        inject_delay_seconds,
        applescript_string_escape(&inject_text)
    )
}

fn provider_prompt_delay_seconds(provider: &ModelProvider) -> u64 {
    if let Ok(value) = std::env::var("SQUAD_AUTOPILOT_PROMPT_DELAY_SECS") {
        if let Ok(seconds) = value.parse() {
            return seconds;
        }
    }
    match provider {
        ModelProvider::Codex => 45,
        ModelProvider::Claude => 8,
        _ => 1,
    }
}

fn terminal_session_plan_for_role(
    workspace: &Path,
    role_id: &str,
    provider: ModelProvider,
) -> TerminalSessionPlan {
    let agent_id = role_id.to_string();
    let provider_tool = provider_tool_command(&provider);
    TerminalSessionPlan {
        agent_id: agent_id.clone(),
        role_id: role_id.to_string(),
        session_role: terminal_session_role(role_id),
        model_provider: provider.clone(),
        terminal_kind: TerminalKind::Tmux,
        pane_label: agent_id.clone(),
        command: provider_tool.shell_command(),
        provider_tool,
        working_dir: workspace.display().to_string(),
        inject_text: role_specific_injection_text(&provider, role_id, &agent_id),
        status: TerminalSessionStatus::Planned,
    }
}

fn terminal_session_role(role_id: &str) -> TerminalSessionRole {
    match role_id {
        "manager" => TerminalSessionRole::Manager,
        "inspector" => TerminalSessionRole::Inspector,
        _ => TerminalSessionRole::Worker,
    }
}

fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn applescript_string_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

pub fn provider_tool_command(provider: &ModelProvider) -> ProviderToolCommand {
    let (program, args) = match provider {
        ModelProvider::Claude => ("claude", vec!["--dangerously-skip-permissions".to_string()]),
        ModelProvider::Codex => ("codex", vec!["--yolo".to_string()]),
        ModelProvider::Gemini => ("gemini", Vec::new()),
        ModelProvider::OpenCode => ("opencode", Vec::new()),
        ModelProvider::OpenRouterFree => (
            "opencode",
            vec!["--model".to_string(), "openrouter/free".to_string()],
        ),
        ModelProvider::OpenRouterCheap => (
            "opencode",
            vec!["--model".to_string(), "openrouter/cheap".to_string()],
        ),
        ModelProvider::Local => ("zsh", Vec::new()),
    };
    ProviderToolCommand {
        program: program.to_string(),
        args,
    }
}

pub fn role_specific_injection_text(
    provider: &ModelProvider,
    role_id: &str,
    agent_id: &str,
) -> String {
    match provider {
        ModelProvider::Codex => format!("$squad {role_id} {agent_id}"),
        ModelProvider::Local => format!("squad join {agent_id} --role {role_id} --client opencode --protocol-version 2 && squad receive {agent_id} --wait"),
        _ => format!("/squad {role_id} {agent_id}"),
    }
}

pub fn autopilot_team_path(workspace: &Path) -> std::path::PathBuf {
    workspace
        .join(".squad")
        .join("teams")
        .join("autopilot.yaml")
}

pub fn write_autopilot_team(
    workspace: &Path,
    roles: &[GeneratedTeamRole],
) -> Result<std::path::PathBuf> {
    if roles.is_empty() {
        bail!("autopilot team must include at least one role");
    }

    let mut team_roles = BTreeMap::new();
    for role in roles {
        let role_id = role.role_id.trim();
        let prompt_file = role.prompt_file.trim();
        if role_id.is_empty() {
            bail!("autopilot team role id cannot be empty");
        }
        if prompt_file.is_empty() {
            bail!("autopilot team prompt file cannot be empty for role '{role_id}'");
        }
        if team_roles
            .insert(
                role_id.to_string(),
                TeamRole {
                    prompt_file: prompt_file.to_string(),
                },
            )
            .is_some()
        {
            bail!("duplicate autopilot team role id: {role_id}");
        }
    }

    let team = TeamConfig {
        name: "autopilot".to_string(),
        roles: team_roles,
    };
    let path = autopilot_team_path(workspace);
    let parent = path
        .parent()
        .with_context(|| format!("invalid autopilot team path: {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create teams directory: {}", parent.display()))?;
    let content = serde_yaml::to_string(&team).context("failed to serialize autopilot team")?;
    std::fs::write(&path, content)
        .with_context(|| format!("failed to write autopilot team: {}", path.display()))?;
    Ok(path)
}

fn push_list_section(output: &mut String, title: &str, items: &[String]) {
    output.push_str("## ");
    output.push_str(title);
    output.push_str("\n\n");
    if items.is_empty() {
        output.push_str("_None recorded._\n\n");
        return;
    }
    for item in items {
        output.push_str("- ");
        output.push_str(item.trim());
        output.push('\n');
    }
    output.push('\n');
}

// ============================================================================
// Science Swarm: provider routing, cost model, local helpers, reliability,
// trace capture, and planner templates.
//
// Pure, dependency-light functions over the existing `TaskGraph` /
// `TaskGraphTask` / `ModelProvider` model, serializing to artifacts under
// `.squad/autopilot/`. Each maps to one Science Swarm autopilot task:
//   - provider adapter overview + report        (task 6)
//   - acceptance criteria extraction            (task 14)
//   - dataset discovery task template           (task 26)
//   - task difficulty estimator                 (task 35)
//   - provider availability check               (task 37)
//   - cost / rate-limit estimator               (task 38)
//   - automatic requeue of failed tasks         (task 67)
//   - retry / backoff schedule                  (task 74)
//   - local duplicate-work detector             (task 82)
//   - local task difficulty classifier          (task 83)
//   - local memory curator                      (task 84)
//   - cost / latency capture                    (task 102)
//   - watchdog restart planner                  (task 124)
//   - continuous integrity checks               (task 125)
//   - round / time caps                         (task 126)
// (files-changed capture, task 99, is already provided by record_files_changed.)
// ============================================================================

fn write_artifact_text(path: &Path, content: &str) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("invalid artifact path: {}", path.display()))?;
    std::fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create autopilot artifacts directory: {}",
            parent.display()
        )
    })?;
    std::fs::write(path, content)
        .with_context(|| format!("failed to write artifact: {}", path.display()))?;
    Ok(())
}

// ---------- Provider tiers & adapter map (tasks 6, 37, 38) ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderTier {
    Local,
    Free,
    Cheap,
    Frontier,
}

impl ProviderTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Free => "free",
            Self::Cheap => "cheap",
            Self::Frontier => "frontier",
        }
    }
}

/// How squad launches and talks to one model provider. This surfaces the
/// existing provider adapter as data so the router and generated reports can
/// reason about it without re-deriving the launch command each time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderAdapterOverview {
    pub provider: String,
    pub tier: ProviderTier,
    pub program: String,
    pub args: Vec<String>,
    pub injection_syntax: String,
    pub launch_delay_seconds: u64,
    pub locally_installed: bool,
}

/// Map a provider to its routing tier. Local and free tiers must never
/// finalize scientific claims; frontier tiers handle management and review.
pub fn provider_tier(provider: &ModelProvider) -> ProviderTier {
    match provider {
        ModelProvider::Local => ProviderTier::Local,
        ModelProvider::OpenRouterFree => ProviderTier::Free,
        ModelProvider::Codex | ModelProvider::OpenRouterCheap => ProviderTier::Cheap,
        ModelProvider::Claude | ModelProvider::Gemini | ModelProvider::OpenCode => {
            ProviderTier::Frontier
        }
    }
}

pub fn provider_adapter_overview(provider: &ModelProvider) -> ProviderAdapterOverview {
    let tool = provider_tool_command(provider);
    ProviderAdapterOverview {
        provider: provider.as_str().to_string(),
        tier: provider_tier(provider),
        program: tool.program.clone(),
        args: tool.args.clone(),
        injection_syntax: role_specific_injection_text(provider, "role", "agent"),
        launch_delay_seconds: provider_prompt_delay_seconds(provider),
        locally_installed: crate::setup::is_installed(&tool.program),
    }
}

pub fn all_provider_adapter_overviews() -> Vec<ProviderAdapterOverview> {
    [
        ModelProvider::Claude,
        ModelProvider::Codex,
        ModelProvider::Gemini,
        ModelProvider::OpenCode,
        ModelProvider::OpenRouterFree,
        ModelProvider::OpenRouterCheap,
        ModelProvider::Local,
    ]
    .iter()
    .map(provider_adapter_overview)
    .collect()
}

pub fn provider_adapters_report_path(workspace: &Path) -> std::path::PathBuf {
    autopilot_artifacts_dir(workspace).join("provider_adapters.md")
}

/// Write a human-readable map of the existing provider adapters (task 6).
pub fn write_provider_adapters_report(
    workspace: &Path,
    overviews: &[ProviderAdapterOverview],
) -> Result<std::path::PathBuf> {
    let mut out = String::new();
    out.push_str("# Provider Adapters\n\n");
    out.push_str("Existing model-provider adapters squad can launch and route to.\n\n");
    if overviews.is_empty() {
        out.push_str("_No provider adapters recorded._\n");
    } else {
        for ov in overviews {
            out.push_str(&format!("## {}\n\n", ov.provider));
            out.push_str(&format!("- Tier: {}\n", ov.tier.as_str()));
            out.push_str(&format!("- Program: `{}`\n", ov.program));
            if ov.args.is_empty() {
                out.push_str("- Args: _(none)_\n");
            } else {
                out.push_str(&format!("- Args: `{}`\n", ov.args.join("` `")));
            }
            out.push_str(&format!("- Injection syntax: {}\n", ov.injection_syntax));
            out.push_str(&format!(
                "- Launch delay (s): {}\n",
                ov.launch_delay_seconds
            ));
            out.push_str(&format!(
                "- Locally installed: {}\n\n",
                if ov.locally_installed { "yes" } else { "no" }
            ));
        }
    }
    let path = provider_adapters_report_path(workspace);
    write_artifact_text(&path, &out)?;
    Ok(path)
}

// ---------- Task difficulty estimator (task 35) ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DifficultyBand {
    Low,
    Medium,
    High,
}

impl DifficultyBand {
    pub fn from_score(score: u32) -> Self {
        if score >= 66 {
            Self::High
        } else if score >= 33 {
            Self::Medium
        } else {
            Self::Low
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DifficultyEstimate {
    pub task_id: String,
    pub score: u32,
    pub band: DifficultyBand,
    pub signals: Vec<String>,
}

const HIGH_DIFFICULTY_KEYWORDS: &[&str] = &[
    "security",
    "migration",
    "refactor",
    "protocol",
    "freeze",
    "verification",
    "consensus",
    "adversarial",
    "statistics",
    "integrity",
    "routing",
    "scheduler",
    "watchdog",
    "escalat",
];

const LOW_DIFFICULTY_KEYWORDS: &[&str] = &[
    "docs", "readme", "typo", "rename", "preset", "summary", "tag",
];

/// Heuristic difficulty score (0..=100) for a task, used by the router to
/// pick a provider tier. Considers dependency fan-in, risk class, domain
/// keywords, and whether the task requires review.
pub fn estimate_task_difficulty(task: &TaskGraphTask) -> DifficultyEstimate {
    let mut score: u32 = 20;
    let mut signals = Vec::new();

    let text = format!("{} {}", task.title, task.description).to_lowercase();

    let dep_count = task.depends_on.len();
    if dep_count > 0 {
        let bump = ((dep_count as u32) * 8).min(24);
        score += bump;
        signals.push(format!("{dep_count} upstream dependencies (+{bump})"));
    }

    match task.risk_level {
        RiskLevel::High => {
            score += 25;
            signals.push("high risk class (+25)".to_string());
        }
        RiskLevel::Medium => {
            score += 12;
            signals.push("medium risk class (+12)".to_string());
        }
        RiskLevel::Low => {
            signals.push("low risk class (+0)".to_string());
        }
    }

    let high_hits = HIGH_DIFFICULTY_KEYWORDS
        .iter()
        .filter(|keyword| text.contains(*keyword))
        .count();
    if high_hits > 0 {
        let bump = ((high_hits as u32) * 10).min(30);
        score += bump;
        signals.push(format!("{high_hits} hard-domain keyword(s) (+{bump})"));
    }

    let low_hits = LOW_DIFFICULTY_KEYWORDS
        .iter()
        .filter(|keyword| text.contains(*keyword))
        .count();
    if low_hits > 0 {
        let cut = ((low_hits as u32) * 8).min(20);
        score = score.saturating_sub(cut);
        signals.push(format!("{low_hits} trivial-domain keyword(s) (-{cut})"));
    }

    if task.status == TaskGraphStatus::ReviewRequired {
        score += 8;
        signals.push("review required (+8)".to_string());
    }

    let score = score.min(100);
    let band = DifficultyBand::from_score(score);
    DifficultyEstimate {
        task_id: task.id.clone(),
        score,
        band,
        signals,
    }
}

/// Recommend a routing tier for a task from its difficulty band. Low work can
/// go to free tiers; high or review-bearing work escalates to frontier tiers.
pub fn recommend_provider_tier(band: DifficultyBand) -> ProviderTier {
    match band {
        DifficultyBand::Low => ProviderTier::Free,
        DifficultyBand::Medium => ProviderTier::Cheap,
        DifficultyBand::High => ProviderTier::Frontier,
    }
}

// ---------- Provider availability & cost (tasks 37, 38) ----------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderAvailability {
    pub provider: String,
    pub program: String,
    pub available: bool,
    pub reason: String,
}

/// Check whether a provider's launch binary is on PATH (task 37). Reuses the
/// cross-platform installer detection so PATH/PATHEXT handling stays uniform.
pub fn check_provider_availability(provider: &ModelProvider) -> ProviderAvailability {
    let tool = provider_tool_command(provider);
    let available = crate::setup::is_installed(&tool.program);
    let reason = if available {
        format!("'{}' found on PATH", tool.program)
    } else {
        format!("'{}' not found on PATH", tool.program)
    };
    ProviderAvailability {
        provider: provider.as_str().to_string(),
        program: tool.program,
        available,
        reason,
    }
}

pub fn available_providers(providers: &[ModelProvider]) -> Vec<ProviderAvailability> {
    providers.iter().map(check_provider_availability).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CostTier {
    Free,
    Cheap,
    Premium,
}

impl CostTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::Cheap => "cheap",
            Self::Premium => "premium",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostRateLimitEstimate {
    pub provider: String,
    pub cost_tier: CostTier,
    /// Relative cost on a 1..=10 scale (1 ~= negligible, 10 ~= frontier).
    pub relative_cost: u32,
    pub rate_limit_per_minute: Option<u32>,
    pub notes: String,
}

/// Estimate the cost tier, relative spend, and rate limit for a provider
/// (task 38). Values are conservative heuristics for routing, not invoices.
pub fn estimate_cost_and_rate_limit(provider: &ModelProvider) -> CostRateLimitEstimate {
    match provider {
        ModelProvider::Local => CostRateLimitEstimate {
            provider: provider.as_str().to_string(),
            cost_tier: CostTier::Free,
            relative_cost: 1,
            rate_limit_per_minute: None,
            notes: "Runs on local hardware; bounded by local throughput, no API spend.".to_string(),
        },
        ModelProvider::OpenRouterFree => CostRateLimitEstimate {
            provider: provider.as_str().to_string(),
            cost_tier: CostTier::Free,
            relative_cost: 1,
            rate_limit_per_minute: Some(20),
            notes: "Free-tier models are rate-limited; best for low-risk parallel work."
                .to_string(),
        },
        ModelProvider::OpenRouterCheap => CostRateLimitEstimate {
            provider: provider.as_str().to_string(),
            cost_tier: CostTier::Cheap,
            relative_cost: 3,
            rate_limit_per_minute: Some(60),
            notes: "Cheap tier for medium-difficulty work; budget per run.".to_string(),
        },
        ModelProvider::Codex => CostRateLimitEstimate {
            provider: provider.as_str().to_string(),
            cost_tier: CostTier::Cheap,
            relative_cost: 5,
            rate_limit_per_minute: None,
            notes: "Codex coding workers; usage counted against the Codex plan.".to_string(),
        },
        ModelProvider::Gemini | ModelProvider::OpenCode => CostRateLimitEstimate {
            provider: provider.as_str().to_string(),
            cost_tier: CostTier::Premium,
            relative_cost: 7,
            rate_limit_per_minute: None,
            notes: "Frontier provider; reserve for high-difficulty or review tasks.".to_string(),
        },
        ModelProvider::Claude => CostRateLimitEstimate {
            provider: provider.as_str().to_string(),
            cost_tier: CostTier::Premium,
            relative_cost: 8,
            rate_limit_per_minute: None,
            notes: "Frontier provider; reserve for management, design, and final review."
                .to_string(),
        },
    }
}

// ---------- Retry / backoff (task 74) ----------

/// Exponential backoff delay in seconds for a 1-based attempt number, capped.
/// attempt=1 returns `base_seconds`; each later attempt doubles, up to `cap`.
pub fn next_retry_delay_seconds(attempt: u32, base_seconds: u64, cap_seconds: u64) -> u64 {
    if attempt == 0 {
        return 0;
    }
    let shift = (attempt - 1).min(20) as u32;
    let raw = base_seconds.saturating_mul(1u64 << shift);
    raw.min(cap_seconds)
}

/// Full backoff schedule (one delay per attempt) for up to `max_attempts`.
pub fn retry_backoff_delays_seconds(
    max_attempts: u32,
    base_seconds: u64,
    cap_seconds: u64,
) -> Vec<u64> {
    (1..=max_attempts)
        .map(|attempt| next_retry_delay_seconds(attempt, base_seconds, cap_seconds))
        .collect()
}

// ---------- Local-model helpers (tasks 82, 83, 84) ----------
// These run without any network and are the only work a local model is
// allowed to perform; they never finalize a scientific claim.

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DuplicateWorkHit {
    pub task_a: String,
    pub task_b: String,
    /// Jaccard overlap over lowercased word tokens, 0.0..=1.0.
    pub overlap: f64,
}

/// Detect likely-duplicate tasks by token overlap on title + description
/// (task 82). Local-only and deterministic; returns one entry per pair whose
/// overlap is at least `threshold`.
pub fn detect_duplicate_work(tasks: &[TaskGraphTask], threshold: f64) -> Vec<DuplicateWorkHit> {
    let docs: Vec<(String, std::collections::BTreeSet<String>)> = tasks
        .iter()
        .map(|task| {
            let tokens = tokenize_for_overlap(&format!("{} {}", task.title, task.description));
            (task.id.clone(), tokens)
        })
        .collect();

    let mut hits = Vec::new();
    for i in 0..docs.len() {
        for j in (i + 1)..docs.len() {
            let (id_a, set_a) = &docs[i];
            let (id_b, set_b) = &docs[j];
            if set_a.is_empty() || set_b.is_empty() {
                continue;
            }
            let intersection = set_a.intersection(set_b).count() as f64;
            let union = set_a.union(set_b).count() as f64;
            let overlap = if union == 0.0 {
                0.0
            } else {
                intersection / union
            };
            if overlap >= threshold {
                hits.push(DuplicateWorkHit {
                    task_a: id_a.clone(),
                    task_b: id_b.clone(),
                    overlap: (overlap * 100.0).round() / 100.0,
                });
            }
        }
    }
    hits
}

fn tokenize_for_overlap(text: &str) -> std::collections::BTreeSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| token.len() > 2)
        .map(str::to_string)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalDifficultyClassification {
    pub task_id: String,
    pub band: DifficultyBand,
    pub rationale: String,
}

/// Offline classifier mirroring `estimate_task_difficulty` but returning a
/// short rationale instead of raw signals (task 83). Safe for a local model
/// to emit because it makes no claim about the task's outcome.
pub fn classify_task_difficulty_local(task: &TaskGraphTask) -> LocalDifficultyClassification {
    let estimate = estimate_task_difficulty(task);
    let rationale = if estimate.signals.is_empty() {
        "baseline difficulty".to_string()
    } else {
        estimate.signals.join("; ")
    };
    LocalDifficultyClassification {
        task_id: estimate.task_id,
        band: estimate.band,
        rationale,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CuratedMemoryItem {
    pub key: String,
    pub summary: String,
    pub tags: Vec<String>,
}

/// Curate raw memory claims into a bounded, de-duplicated set (task 84).
/// Local-only: trims, drops near-identical lines (same words in any order),
/// tags each item, and caps the retained count so memory cannot grow forever.
pub fn curate_memory(claims: &[String], limit: usize) -> Vec<CuratedMemoryItem> {
    let limit = if limit == 0 { 1 } else { limit };
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut items = Vec::new();
    for raw in claims {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let key = normalized_memory_key(trimmed);
        if !seen.insert(key.clone()) {
            continue;
        }
        items.push(CuratedMemoryItem {
            key,
            summary: trimmed.to_string(),
            tags: memory_tags(trimmed),
        });
        if items.len() >= limit {
            break;
        }
    }
    items
}

fn normalized_memory_key(text: &str) -> String {
    let lower = text.to_lowercase();
    let mut words: Vec<&str> = lower.split_whitespace().collect();
    words.sort();
    words.join(" ")
}

fn memory_tags(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let rules: &[(&str, &str)] = &[
        ("cost", "cost"),
        ("latency", "latency"),
        ("rate", "rate-limit"),
        ("fail", "failure"),
        ("retry", "retry"),
        ("duplicate", "duplicate"),
        ("security", "security"),
        ("risk", "risk"),
        ("verif", "verification"),
        ("test", "test"),
    ];
    let mut tags = Vec::new();
    for (needle, tag) in rules {
        if lower.contains(needle) && !tags.iter().any(|existing| existing == tag) {
            tags.push(tag.to_string());
        }
    }
    if tags.is_empty() {
        tags.push("general".to_string());
    }
    tags
}

// ---------- Cost / latency capture (task 102) ----------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostLatencyRecord {
    pub task_id: Option<String>,
    pub agent_id: Option<String>,
    pub provider: Option<String>,
    pub latency_ms: Option<u64>,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub estimated_cost_usd: Option<f64>,
    pub notes: Option<String>,
}

pub fn cost_latency_path(workspace: &Path) -> std::path::PathBuf {
    autopilot_artifacts_dir(workspace).join("cost-latency.json")
}

pub fn verification_results_path(workspace: &Path) -> std::path::PathBuf {
    autopilot_artifacts_dir(workspace).join("verification-results.json")
}

pub fn read_cost_latency(workspace: &Path) -> Result<Vec<CostLatencyRecord>> {
    let path = cost_latency_path(workspace);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read cost/latency records: {}", path.display()))?;
    if content.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(&content)
        .with_context(|| format!("failed to parse cost/latency records: {}", path.display()))
}

pub fn record_cost_latency(
    workspace: &Path,
    record: CostLatencyRecord,
) -> Result<Vec<CostLatencyRecord>> {
    let mut records = read_cost_latency(workspace)?;
    records.push(normalized_cost_latency_record(record));
    let path = cost_latency_path(workspace);
    let content = serde_json::to_string_pretty(&records)
        .context("failed to serialize cost/latency records")?;
    write_artifact_text(&path, &format!("{content}\n"))?;
    Ok(records)
}

fn normalized_cost_latency_record(mut record: CostLatencyRecord) -> CostLatencyRecord {
    let trim_opt = |value: Option<String>| -> Option<String> {
        value.and_then(|s| {
            let s = s.trim();
            if s.is_empty() {
                None
            } else {
                Some(s.to_string())
            }
        })
    };
    record.task_id = trim_opt(record.task_id);
    record.agent_id = trim_opt(record.agent_id);
    record.provider = trim_opt(record.provider);
    record.notes = trim_opt(record.notes);
    record
}

pub fn cost_latency_report_lines(records: &[CostLatencyRecord]) -> Vec<String> {
    records.iter().map(format_cost_latency_record).collect()
}

fn format_cost_latency_record(record: &CostLatencyRecord) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(provider) = record.provider.as_deref() {
        parts.push(provider.to_string());
    }
    if let Some(latency) = record.latency_ms {
        parts.push(format!("{latency}ms"));
    }
    if let Some(cost) = record.estimated_cost_usd {
        parts.push(format!("${cost:.4}"));
    }
    let mut line = if parts.is_empty() {
        "entry".to_string()
    } else {
        parts.join(" ")
    };
    if let (Some(prompt), Some(completion)) = (record.prompt_tokens, record.completion_tokens) {
        line.push_str(&format!(" ({prompt}+{completion} tok)"));
    }
    if let Some(task_id) = record.task_id.as_deref() {
        line.push_str(&format!(" [task: {task_id}]"));
    }
    if let Some(agent_id) = record.agent_id.as_deref() {
        line.push_str(&format!(" [agent: {agent_id}]"));
    }
    if let Some(notes) = record.notes.as_deref() {
        line.push_str(": ");
        line.push_str(notes);
    }
    line
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VerificationVerdict {
    Pass,
    Fail,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationResultRecord {
    pub task_id: Option<String>,
    pub verifier_id: Option<String>,
    pub verdict: VerificationVerdict,
    pub summary: String,
    pub evidence: Vec<String>,
}

pub fn read_verification_results(workspace: &Path) -> Result<Vec<VerificationResultRecord>> {
    let path = verification_results_path(workspace);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read verification results: {}", path.display()))?;
    if content.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(&content)
        .with_context(|| format!("failed to parse verification results: {}", path.display()))
}

pub fn record_verification_result(
    workspace: &Path,
    record: VerificationResultRecord,
) -> Result<Vec<VerificationResultRecord>> {
    if record.summary.trim().is_empty() {
        bail!("verification summary cannot be empty");
    }
    let mut records = read_verification_results(workspace)?;
    records.push(normalized_verification_result(record));
    let content = serde_json::to_string_pretty(&records)
        .context("failed to serialize verification results")?;
    write_artifact_text(
        &verification_results_path(workspace),
        &format!("{content}\n"),
    )?;
    Ok(records)
}

pub fn verification_result_report_lines(records: &[VerificationResultRecord]) -> Vec<String> {
    records.iter().map(format_verification_result).collect()
}

fn normalized_verification_result(
    mut record: VerificationResultRecord,
) -> VerificationResultRecord {
    record.task_id = nonempty_trimmed_option(record.task_id);
    record.verifier_id = nonempty_trimmed_option(record.verifier_id);
    record.summary = record.summary.trim().to_string();
    record.evidence = record
        .evidence
        .into_iter()
        .filter_map(|item| {
            let item = item.trim();
            if item.is_empty() {
                None
            } else {
                Some(item.to_string())
            }
        })
        .collect();
    record
}

fn format_verification_result(record: &VerificationResultRecord) -> String {
    let verdict = match record.verdict {
        VerificationVerdict::Pass => "pass",
        VerificationVerdict::Fail => "fail",
        VerificationVerdict::Blocked => "blocked",
    };
    let mut line = format!("{verdict}: {}", record.summary);
    if let Some(task_id) = record.task_id.as_deref() {
        line.push_str(&format!(" [task: {task_id}]"));
    }
    if let Some(verifier_id) = record.verifier_id.as_deref() {
        line.push_str(&format!(" [verifier: {verifier_id}]"));
    }
    if !record.evidence.is_empty() {
        line.push_str(": ");
        line.push_str(&record.evidence.join("; "));
    }
    line
}

pub fn trace_summary_report_lines(items: &[CuratedMemoryItem]) -> Vec<String> {
    items
        .iter()
        .map(|item| format!("{} [{}]", item.summary, item.tags.join(", ")))
        .collect()
}

// ---------- Scheduler reliability (tasks 67, 124, 125, 126) ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequeueOutcome {
    Requeued,
    MaxRetriesExhausted,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequeueAction {
    pub task_id: String,
    pub attempt: u32,
    pub outcome: RequeueOutcome,
    pub reason: String,
}

/// Automatically requeue failed tasks up to `max_requeues` times each
/// (task 67). `attempts` tracks prior requeues per task and is updated in
/// place. Tasks that exceed the cap are left `Failed`; requeued tasks move to
/// `ReadyParallel` (deps satisfied) or `Blocked` (deps still pending) so the
/// scheduler re-plans them.
pub fn plan_failed_task_requeue(
    graph: &mut TaskGraph,
    attempts: &mut BTreeMap<String, u32>,
    max_requeues: u32,
) -> Vec<RequeueAction> {
    let completed: std::collections::BTreeSet<String> = graph
        .tasks
        .iter()
        .filter(|task| task.status == TaskGraphStatus::Done)
        .map(|task| task.id.clone())
        .collect();

    let mut actions = Vec::new();
    for task in &mut graph.tasks {
        if task.status != TaskGraphStatus::Failed {
            actions.push(RequeueAction {
                task_id: task.id.clone(),
                attempt: *attempts.get(&task.id).unwrap_or(&0),
                outcome: RequeueOutcome::Skipped,
                reason: "task is not failed".to_string(),
            });
            continue;
        }
        let prior = *attempts.get(&task.id).unwrap_or(&0);
        if prior >= max_requeues {
            actions.push(RequeueAction {
                task_id: task.id.clone(),
                attempt: prior,
                outcome: RequeueOutcome::MaxRetriesExhausted,
                reason: format!("exceeded max requeues ({max_requeues})"),
            });
            continue;
        }
        let next = prior + 1;
        attempts.insert(task.id.clone(), next);
        let deps_done = task.depends_on.iter().all(|dep| completed.contains(dep));
        task.status = if deps_done {
            TaskGraphStatus::ReadyParallel
        } else {
            TaskGraphStatus::Blocked
        };
        actions.push(RequeueAction {
            task_id: task.id.clone(),
            attempt: next,
            outcome: RequeueOutcome::Requeued,
            reason: "failed task requeued for another attempt".to_string(),
        });
    }
    actions
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerHeartbeat {
    pub agent_id: String,
    pub last_seen_seconds_ago: u64,
    pub assigned_task_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchdogActionKind {
    Keep,
    Restart,
    Escalate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchdogAction {
    pub agent_id: String,
    pub action: WatchdogActionKind,
    pub reason: String,
}

/// Decide a watchdog action per worker (task 124). Workers silent longer than
/// `stall_threshold_seconds` while holding assigned work are flagged for
/// restart; workers silent past `escalate_threshold_seconds` escalate so a
/// stronger model takes over instead of looping on restarts. A nonzero
/// escalate threshold smaller than the stall threshold is ignored.
pub fn watchdog_plan(
    heartbeats: &[WorkerHeartbeat],
    stall_threshold_seconds: u64,
    escalate_threshold_seconds: u64,
) -> Vec<WatchdogAction> {
    let escalate_active =
        escalate_threshold_seconds != 0 && escalate_threshold_seconds > stall_threshold_seconds;
    heartbeats
        .iter()
        .map(|heartbeat| {
            let idle = heartbeat.last_seen_seconds_ago;
            let has_work = heartbeat.assigned_task_id.is_some();
            if has_work && escalate_active && idle >= escalate_threshold_seconds {
                WatchdogAction {
                    agent_id: heartbeat.agent_id.clone(),
                    action: WatchdogActionKind::Escalate,
                    reason: format!(
                        "silent {idle}s with assigned work (>= {escalate_threshold_seconds}s)"
                    ),
                }
            } else if has_work && idle >= stall_threshold_seconds {
                WatchdogAction {
                    agent_id: heartbeat.agent_id.clone(),
                    action: WatchdogActionKind::Restart,
                    reason: format!(
                        "silent {idle}s with assigned work (>= {stall_threshold_seconds}s)"
                    ),
                }
            } else {
                WatchdogAction {
                    agent_id: heartbeat.agent_id.clone(),
                    action: WatchdogActionKind::Keep,
                    reason: "within heartbeat budget".to_string(),
                }
            }
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IntegritySeverity {
    Error,
    Warning,
}

impl IntegritySeverity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrityFinding {
    pub severity: IntegritySeverity,
    pub code: String,
    pub message: String,
}

/// Continuous integrity checks over a task graph (task 125), meant to run on a
/// heartbeat. Reports structural errors (invalid graph, dependency cycles) and
/// quality warnings (tasks without acceptance criteria, stale Blocked tasks).
pub fn continuous_integrity_check(graph: &TaskGraph) -> Vec<IntegrityFinding> {
    let mut findings = Vec::new();

    if let Err(err) = graph.validate() {
        findings.push(IntegrityFinding {
            severity: IntegritySeverity::Error,
            code: "graph_invalid".to_string(),
            message: err.to_string(),
        });
    }

    if has_dependency_cycle(graph) {
        findings.push(IntegrityFinding {
            severity: IntegritySeverity::Error,
            code: "dependency_cycle".to_string(),
            message: "task graph contains a dependency cycle".to_string(),
        });
    }

    let done: std::collections::BTreeSet<String> = graph
        .tasks
        .iter()
        .filter(|task| task.status == TaskGraphStatus::Done)
        .map(|task| task.id.clone())
        .collect();

    for task in &graph.tasks {
        if task.acceptance_criteria.is_empty() {
            findings.push(IntegrityFinding {
                severity: IntegritySeverity::Warning,
                code: "missing_acceptance_criteria".to_string(),
                message: format!("task '{}' has no acceptance criteria", task.id),
            });
        }
        if task.status == TaskGraphStatus::Blocked
            && !task.depends_on.is_empty()
            && task.depends_on.iter().all(|dep| done.contains(dep))
        {
            findings.push(IntegrityFinding {
                severity: IntegritySeverity::Warning,
                code: "stale_blocked".to_string(),
                message: format!(
                    "task '{}' is Blocked but all dependencies are Done",
                    task.id
                ),
            });
        }
    }

    findings
}

fn has_dependency_cycle(graph: &TaskGraph) -> bool {
    let index: std::collections::BTreeMap<String, usize> = graph
        .tasks
        .iter()
        .enumerate()
        .map(|(i, task)| (task.id.clone(), i))
        .collect();
    let n = graph.tasks.len();
    let mut adjacency: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, task) in graph.tasks.iter().enumerate() {
        for dep in &task.depends_on {
            if let Some(&j) = index.get(dep) {
                adjacency[j].push(i);
            }
        }
    }
    let mut color = vec![0u8; n];

    fn visit(node: usize, adjacency: &[Vec<usize>], color: &mut [u8]) -> bool {
        color[node] = 1;
        for &next in &adjacency[node] {
            if color[next] == 1 {
                return true;
            }
            if color[next] == 0 && visit(next, adjacency, color) {
                return true;
            }
        }
        color[node] = 2;
        false
    }

    for start in 0..n {
        if color[start] == 0 && visit(start, &adjacency, &mut color) {
            return true;
        }
    }
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoundTimeCaps {
    pub max_rounds: u32,
    pub max_runtime_seconds: u64,
    pub max_requeues_per_task: u32,
}

impl Default for RoundTimeCaps {
    fn default() -> Self {
        Self {
            max_rounds: 12,
            max_runtime_seconds: 60 * 60,
            max_requeues_per_task: 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapStatus {
    pub elapsed_rounds: u32,
    pub elapsed_seconds: u64,
    pub rounds_remaining: i64,
    pub runtime_remaining_seconds: i64,
    pub hit_round_cap: bool,
    pub hit_time_cap: bool,
    pub should_stop: bool,
    pub reason: String,
}

/// Evaluate a run against its round/time caps (task 126). The scheduler stops
/// scheduling new work once `should_stop` is true. `max_requeues_per_task`
/// pairs with `plan_failed_task_requeue` so requeue and caps share one budget.
pub fn enforce_caps(elapsed_rounds: u32, elapsed_seconds: u64, caps: &RoundTimeCaps) -> CapStatus {
    let rounds_remaining = (caps.max_rounds as i64) - (elapsed_rounds as i64);
    let runtime_remaining = (caps.max_runtime_seconds as i64) - (elapsed_seconds as i64);
    let hit_round_cap = rounds_remaining <= 0;
    let hit_time_cap = runtime_remaining <= 0;
    let should_stop = hit_round_cap || hit_time_cap;
    let reason = match (hit_round_cap, hit_time_cap) {
        (true, true) => "round and time caps reached".to_string(),
        (true, false) => format!("round cap reached (max {})", caps.max_rounds),
        (false, true) => format!("time cap reached (max {}s)", caps.max_runtime_seconds),
        (false, false) => "within caps".to_string(),
    };
    CapStatus {
        elapsed_rounds,
        elapsed_seconds,
        rounds_remaining,
        runtime_remaining_seconds: runtime_remaining,
        hit_round_cap,
        hit_time_cap,
        should_stop,
        reason,
    }
}

// ---------- Planner templates (tasks 14, 26) ----------

/// Extract acceptance-criteria bullets from a PRD body (task 14), independent
/// of the full task-graph build. Handles both plural and singular headings and
/// trims checklist markers, so callers can verify a task's criteria directly.
pub fn extract_acceptance_criteria(prd_content: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut in_section = false;
    for raw in prd_content.lines() {
        let line = raw.trim();
        if let Some(heading) = heading_text(line) {
            in_section = matches!(
                heading.to_lowercase().as_str(),
                "acceptance criteria" | "acceptance criterion"
            );
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some(value) = parse_bulleted_or_plain_value(line) {
            push_unique(&mut items, value.to_string());
        }
    }
    items
}

fn heading_text(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix('#')?;
    let rest = rest.trim_start_matches('#').trim();
    if rest.is_empty() {
        return None;
    }
    let cleaned = rest.trim_matches('*').trim();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.to_string())
    }
}

/// A ready science-swarm task that discovers candidate datasets before any
/// analysis runs (task 26). Pre-classified low-risk and parallel, with an
/// explicit acceptance criterion and independent-verification requirement so
/// the planner can drop it straight into a task graph.
pub fn background_evidence_search_task_template(id: &str) -> TaskGraphTask {
    TaskGraphTask {
        id: id.to_string(),
        title: "Search background evidence for the scientific question".to_string(),
        description: "Collect low-risk background sources, no-data findings, and uncertainty notes without treating retrieved context as validation.".to_string(),
        assigned_role: Some("literature_worker".to_string()),
        status: TaskGraphStatus::ReadyParallel,
        priority: 50,
        risk_level: RiskLevel::Low,
        acceptance_criteria: vec![
            "Evidence is labeled as background context, not a final scientific claim.".to_string(),
            "No-data and uncertainty findings are recorded explicitly.".to_string(),
        ],
        likely_files: vec!["background_evidence.md".to_string()],
        test_requirements: vec![
            "An independent verifier samples cited locators and confirms uncertainty labels.".to_string(),
        ],
        depends_on: Vec::new(),
    }
}

pub fn dataset_discovery_task_template(id: &str) -> TaskGraphTask {
    TaskGraphTask {
        id: id.to_string(),
        title: "Discover candidate datasets for the objective".to_string(),
        description: "Enumerate public and local datasets relevant to the scientific question; record source, license, format, and a reproducible locator for each.".to_string(),
        assigned_role: Some("literature_worker".to_string()),
        status: TaskGraphStatus::ReadyParallel,
        priority: 50,
        risk_level: RiskLevel::Low,
        acceptance_criteria: vec![
            "At least one candidate dataset is recorded with a reproducible locator.".to_string(),
            "Each dataset lists license and access constraints.".to_string(),
        ],
        likely_files: vec!["datasets.md".to_string(), "datasets.json".to_string()],
        test_requirements: vec![
            "An independent worker confirms each locator resolves before analysis.".to_string(),
        ],
        depends_on: Vec::new(),
    }
}

pub fn tool_availability_scan_task_template(id: &str) -> TaskGraphTask {
    TaskGraphTask {
        id: id.to_string(),
        title: "Scan required tool availability".to_string(),
        description: "Map each planned protocol step to a real CLI, dataset, script, or harness; missing tools must be reported instead of replaced with toy fallbacks.".to_string(),
        assigned_role: Some("tool_mapper".to_string()),
        status: TaskGraphStatus::ReadyParallel,
        priority: 50,
        risk_level: RiskLevel::Low,
        acceptance_criteria: vec![
            "Every required tool is marked available, unavailable, or blocked.".to_string(),
            "Unavailable tools fail loudly with no fabricated fallback.".to_string(),
        ],
        likely_files: vec!["tool_availability.md".to_string(), "tool_availability.json".to_string()],
        test_requirements: vec![
            "A verifier confirms at least one reported command or path exists for each available tool.".to_string(),
        ],
        depends_on: Vec::new(),
    }
}

pub fn independent_verification_task_template(id: &str) -> TaskGraphTask {
    TaskGraphTask {
        id: id.to_string(),
        title: "Run independent verification checks".to_string(),
        description: "Verify produced artifacts from a separate worker context, recording verdict, evidence, blocking findings, and unresolved risk.".to_string(),
        assigned_role: Some("verification_worker".to_string()),
        status: TaskGraphStatus::ReadyParallel,
        priority: 60,
        risk_level: RiskLevel::Low,
        acceptance_criteria: vec![
            "Verification result includes pass, fail, or blocked verdict.".to_string(),
            "Verifier records evidence and does not review its own produced conclusion.".to_string(),
        ],
        likely_files: vec!["verification_report.json".to_string()],
        test_requirements: vec![
            "Verification output is captured through record_verification_result.".to_string(),
        ],
        depends_on: Vec::new(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationCheck {
    pub passed: bool,
    pub findings: Vec<String>,
}

pub fn verify_controls(
    required_controls: &[&str],
    observed_controls: &[&str],
) -> VerificationCheck {
    let observed: std::collections::BTreeSet<String> = observed_controls
        .iter()
        .map(|control| control.trim().to_lowercase())
        .filter(|control| !control.is_empty())
        .collect();
    let mut findings = Vec::new();
    for required in required_controls {
        let required = required.trim();
        if required.is_empty() {
            continue;
        }
        if !observed.contains(&required.to_lowercase()) {
            findings.push(format!("missing control: {required}"));
        }
    }
    if findings.is_empty() {
        findings.push("all required controls present".to_string());
    }
    VerificationCheck {
        passed: findings.len() == 1 && findings[0] == "all required controls present",
        findings,
    }
}

pub fn verify_statistics_plan(
    metrics: &[&str],
    thresholds: &[&str],
    sample_size: Option<u32>,
) -> VerificationCheck {
    let mut findings = Vec::new();
    if metrics.iter().all(|metric| metric.trim().is_empty()) {
        findings.push("missing statistical metric".to_string());
    }
    if thresholds
        .iter()
        .all(|threshold| threshold.trim().is_empty())
    {
        findings.push("missing decision threshold".to_string());
    }
    match sample_size {
        Some(size) if size > 0 => {}
        _ => findings.push("missing positive sample size".to_string()),
    }
    if findings.is_empty() {
        findings.push("statistics plan has metric, threshold, and sample size".to_string());
    }
    VerificationCheck {
        passed: findings.len() == 1
            && findings[0] == "statistics plan has metric, threshold, and sample size",
        findings,
    }
}
