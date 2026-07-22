use anyhow::{Context, Result};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::autopilot::{
    claude_task_eligibility, AdaptiveSchedulingConfig, RiskLevel, TaskGraphStatus, TaskGraphTask,
    TerminalKind, TerminalSessionPlan, TerminalSessionStatus,
};
use crate::tasks::TaskRecord;

const DEFAULT_MESSAGE_KIND: &str = "note";
const TASK_ASSIGNED_KIND: &str = "task_assigned";
const TASK_STATUS_QUEUED: &str = "queued";
const TASK_STATUS_ACKED: &str = "acked";
const TASK_STATUS_COMPLETED: &str = "completed";
const TASK_LEASE_SECS: i64 = 15 * 60;
const WORKER_STATE_STARTING: &str = "starting";
const WORKER_STATE_READY: &str = "ready";
const WORKER_STATE_ASSIGNED: &str = "assigned";
const WORKER_STATE_WORKING: &str = "working";
const WORKER_STATE_BLOCKED: &str = "blocked";
const WORKER_STATE_OFFLINE: &str = "offline";
const TASK_STATE_QUEUED: &str = "queued";
const TASK_STATE_ASSIGNED: &str = "assigned";
const TASK_STATE_ACKED: &str = "acked";
const TASK_STATE_WORKING: &str = "working";
const TASK_STATE_REPORTED: &str = "reported";
const TASK_STATE_VERIFIED: &str = "verified";
const TASK_STATE_COMPLETE: &str = "complete";
const TASK_STATE_FAILED: &str = "failed";
const TASK_STATE_BLOCKED: &str = "blocked";
const DEFAULT_SERVICE_TASK_MAX_ATTEMPTS: i64 = 3;
pub const DEFAULT_SERVICE_CLAIM_LEASE_SECS: i64 = 15 * 60;
pub const DEFAULT_SERVICE_LANE_ESCAPE_SECS: i64 = 300;

pub fn normalize_service_provider_lane(spec: Option<&str>) -> Option<String> {
    let spec = spec?.trim();
    if spec.is_empty() {
        return None;
    }
    let parts = spec
        .split(',')
        .map(|part| part.trim().to_ascii_lowercase())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.is_empty() || parts.iter().any(|part| part == "any") {
        return None;
    }
    Some(parts.join(","))
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AgentRecord {
    pub id: String,
    pub role: String,
    pub joined_at: i64,
    pub last_seen: Option<i64>,
    pub status: String,
    pub archived_at: Option<i64>,
    pub client_type_raw: Option<String>,
    pub protocol_version_raw: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MessageRecord {
    pub id: i64,
    pub from_agent: String,
    pub to_agent: String,
    pub content: String,
    pub created_at: i64,
    pub read: bool,
    pub kind: String,
    pub task_id: Option<String>,
    pub reply_to: Option<i64>,
}

pub struct Store {
    conn: Connection,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServiceHealthRecord {
    pub status: String,
    pub version: String,
    pub database: String,
    pub scheduler: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServiceWorkerRecord {
    pub id: String,
    pub kind: String,
    pub role: String,
    pub status: String,
    pub capacity: i64,
    pub current_task_id: Option<String>,
    pub last_heartbeat_at: Option<String>,
    pub metadata: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServiceTaskRecord {
    pub id: String,
    pub title: String,
    pub description: String,
    pub acceptance_criteria: Vec<String>,
    pub source_prd_path: String,
    pub source_task_number: Option<String>,
    pub state: String,
    pub priority: i64,
    pub preferred_model: String,
    pub role: String,
    pub parallelizable: bool,
    pub dependencies: Vec<String>,
    pub scheduling_pool: String,
    pub estimated_size: String,
    pub claude_suitable: bool,
    pub assigned_worker_id: Option<String>,
    pub eligible_providers: Option<String>,
    pub lease_owner: Option<String>,
    pub lease_expires_at: Option<String>,
    pub claimed_at: Option<String>,
    pub completed_worker_id: Option<String>,
    pub completed_worker_kind: Option<String>,
    pub claim_duration_secs: Option<i64>,
    pub retry_count: i64,
    pub failure_reason: Option<String>,
    pub attempt: i64,
    pub max_attempts: i64,
    pub acked_at: Option<String>,
    pub started_at: Option<String>,
    pub reported_at: Option<String>,
    pub verified_at: Option<String>,
    pub completed_at: Option<String>,
    pub last_error: Option<String>,
    pub retry_reason: Option<String>,
    pub report_hash: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServiceTaskReportRecord {
    pub task_id: String,
    pub summary: String,
    pub files_inspected: Vec<String>,
    pub changed_files: Vec<String>,
    pub tests_run: Vec<String>,
    pub verification: String,
    pub risks: String,
    pub raw_report: String,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServicePrdRecord {
    pub id: String,
    pub path: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServiceEventRecord {
    pub id: String,
    pub event_type: String,
    pub worker_id: Option<String>,
    pub task_id: Option<String>,
    pub payload: String,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServiceStaleRequeueResult {
    pub requeued: Vec<ServiceTaskRecord>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServiceStaleWorkerResult {
    pub offline_workers: Vec<ServiceWorkerRecord>,
    pub requeued: Vec<ServiceTaskRecord>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServiceQueueReapResult {
    pub lease_expired: Vec<ServiceTaskRecord>,
    pub assignment_released: Vec<ServiceTaskRecord>,
    pub failed: Vec<ServiceTaskRecord>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServiceQueueLaneStat {
    pub lane: String,
    pub queued: i64,
    pub oldest_age_secs: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ServiceQueueProviderStat {
    pub worker_kind: String,
    pub in_flight: i64,
    pub completed_window: i64,
    pub avg_claim_duration_secs: Option<f64>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ServiceQueueStats {
    pub lanes: Vec<ServiceQueueLaneStat>,
    pub directly_assigned_queued: i64,
    pub total_queued: i64,
    pub total_in_flight: i64,
    pub providers: Vec<ServiceQueueProviderStat>,
    pub pool_sizing_hint: Option<String>,
    pub window_secs: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServiceWorkerInput {
    pub id: String,
    pub kind: String,
    pub role: String,
    pub status: Option<String>,
    pub capacity: Option<i64>,
    pub metadata: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServiceTaskInput {
    pub title: String,
    pub description: String,
    pub acceptance_criteria: Vec<String>,
    pub source_prd_path: String,
    pub source_task_number: Option<String>,
    pub priority: i64,
    pub preferred_model: String,
    pub role: String,
    pub parallelizable: bool,
    pub max_attempts: Option<i64>,
    pub dependencies: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServiceTaskReportInput {
    pub summary: String,
    pub files_inspected: Vec<String>,
    pub changed_files: Vec<String>,
    pub tests_run: Vec<String>,
    pub verification: String,
    pub risks: String,
    pub raw_report: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServiceTaskProgressInput {
    pub summary: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct AutopilotRunRecord {
    pub id: i64,
    pub prd_path: String,
    pub status: String,
    pub created_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct AutopilotAgentInput {
    pub name: String,
    pub role: String,
    pub model_provider: String,
    pub skills_prompt: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct AutopilotAgentRecord {
    pub id: i64,
    pub run_id: i64,
    pub name: String,
    pub role: String,
    pub model_provider: String,
    pub skills_prompt: String,
    pub status: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct AutopilotTaskRecord {
    pub id: i64,
    pub run_id: i64,
    pub title: String,
    pub description: String,
    pub assigned_role: Option<String>,
    pub assigned_agent_id: Option<i64>,
    pub status: String,
    pub priority: i64,
    pub risk_level: Option<String>,
    pub acceptance_criteria: Vec<String>,
    pub created_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct AutopilotTaskDependencyRecord {
    pub task_id: i64,
    pub depends_on_task_id: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq, Eq)]
pub struct AutopilotTaskStatusCounts {
    pub ready_parallel: i64,
    pub blocked: i64,
    pub sequential: i64,
    pub review_required: i64,
    pub done: i64,
    pub failed: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct AutopilotReviewRecord {
    pub id: i64,
    pub task_id: i64,
    pub reviewer_agent_id: Option<i64>,
    pub verdict: String,
    pub notes: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct AutopilotTerminalSessionRecord {
    pub id: i64,
    pub run_id: i64,
    pub agent_id: i64,
    pub terminal_kind: String,
    pub command: String,
    pub status: String,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open database: {}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;",
        )?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS agents (
                id TEXT PRIMARY KEY,
                role TEXT NOT NULL,
                joined_at INTEGER NOT NULL,
                session_token TEXT,
                last_seen INTEGER,
                status TEXT NOT NULL DEFAULT 'active',
                archived_at INTEGER,
                client_type TEXT,
                protocol_version INTEGER
             );
             CREATE TABLE IF NOT EXISTS messages (
                  id INTEGER PRIMARY KEY AUTOINCREMENT,
                  from_agent TEXT NOT NULL,
                 to_agent TEXT NOT NULL,
                 content TEXT NOT NULL,
                 created_at INTEGER NOT NULL,
                 read INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS tasks (
                 id TEXT PRIMARY KEY,
                 title TEXT NOT NULL,
                 body TEXT NOT NULL,
                 created_by TEXT NOT NULL,
                 assigned_to TEXT,
                 status TEXT NOT NULL,
                 lease_owner TEXT,
                 lease_expires_at INTEGER,
                 result_summary TEXT,
                 created_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL,
                 completed_at INTEGER
             );
             CREATE TABLE IF NOT EXISTS autopilot_runs (
                 id INTEGER PRIMARY KEY,
                 prd_path TEXT NOT NULL,
                 status TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 completed_at TEXT
             );
             CREATE TABLE IF NOT EXISTS autopilot_agents (
                 id INTEGER PRIMARY KEY,
                 run_id INTEGER NOT NULL,
                 name TEXT NOT NULL,
                 role TEXT NOT NULL,
                 model_provider TEXT NOT NULL,
                 skills_prompt TEXT NOT NULL,
                 status TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS autopilot_tasks (
                 id INTEGER PRIMARY KEY,
                 run_id INTEGER NOT NULL,
                 title TEXT NOT NULL,
                 description TEXT NOT NULL,
                 assigned_role TEXT,
                 assigned_agent_id INTEGER,
                 status TEXT NOT NULL,
                 priority INTEGER DEFAULT 0,
                 risk_level TEXT,
                 acceptance_criteria TEXT,
                 created_at TEXT NOT NULL,
                 completed_at TEXT
             );
             CREATE TABLE IF NOT EXISTS autopilot_task_dependencies (
                 task_id INTEGER NOT NULL,
                 depends_on_task_id INTEGER NOT NULL,
                 PRIMARY KEY (task_id, depends_on_task_id)
             );
             CREATE TABLE IF NOT EXISTS autopilot_reviews (
                 id INTEGER PRIMARY KEY,
                 task_id INTEGER NOT NULL,
                 reviewer_agent_id INTEGER,
                 verdict TEXT NOT NULL,
                 notes TEXT,
                 created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS autopilot_terminal_sessions (
                 id INTEGER PRIMARY KEY,
                 run_id INTEGER NOT NULL,
                 agent_id INTEGER NOT NULL,
                 terminal_kind TEXT NOT NULL,
                 command TEXT NOT NULL,
                 status TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS science_swarm_runs (
                 id INTEGER PRIMARY KEY,
                 objective TEXT NOT NULL,
                 prd_path TEXT,
                 status TEXT NOT NULL,
                 risk_class TEXT,
                 created_at TEXT NOT NULL,
                 completed_at TEXT
             );
             CREATE TABLE IF NOT EXISTS science_swarm_tasks (
                 id INTEGER PRIMARY KEY,
                 run_id INTEGER NOT NULL,
                 task_number TEXT NOT NULL,
                 title TEXT NOT NULL,
                 description TEXT NOT NULL,
                 task_kind TEXT NOT NULL,
                 execution_mode TEXT NOT NULL,
                 status TEXT NOT NULL,
                 assigned_agent_id INTEGER,
                 assigned_provider TEXT,
                 assigned_model TEXT,
                 risk_level TEXT,
                 acceptance_criteria TEXT,
                 verification_required INTEGER DEFAULT 1,
                 created_at TEXT NOT NULL,
                 completed_at TEXT
             );
             CREATE TABLE IF NOT EXISTS science_swarm_task_dependencies (
                 task_id INTEGER NOT NULL,
                 depends_on_task_id INTEGER NOT NULL,
                 PRIMARY KEY (task_id, depends_on_task_id)
             );
             CREATE TABLE IF NOT EXISTS science_swarm_agents (
                 id INTEGER PRIMARY KEY,
                 run_id INTEGER NOT NULL,
                 name TEXT NOT NULL,
                 role TEXT NOT NULL,
                 provider TEXT NOT NULL,
                 model TEXT NOT NULL,
                 skills_prompt TEXT NOT NULL,
                 status TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS science_swarm_traces (
                 id INTEGER PRIMARY KEY,
                 run_id INTEGER NOT NULL,
                 task_id INTEGER,
                 agent_id INTEGER,
                 provider TEXT,
                 model TEXT,
                 prompt TEXT NOT NULL,
                 response TEXT,
                 tool_calls TEXT,
                 files_changed TEXT,
                 tests_run TEXT,
                 score REAL,
                 accepted INTEGER DEFAULT 0,
                 failure_reason TEXT,
                 cost_usd REAL,
                 latency_ms INTEGER,
                 created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS science_swarm_verifications (
                 id INTEGER PRIMARY KEY,
                 run_id INTEGER NOT NULL,
                 task_id INTEGER,
                 layer TEXT NOT NULL,
                 verdict TEXT NOT NULL,
                 evidence TEXT,
                 blocking INTEGER DEFAULT 0,
                 created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS service_workers (
                 id TEXT PRIMARY KEY,
                 kind TEXT NOT NULL,
                 role TEXT NOT NULL,
                 status TEXT NOT NULL,
                 capacity INTEGER NOT NULL DEFAULT 1,
                 current_task_id TEXT,
                 last_heartbeat_at TEXT,
                 metadata TEXT,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS service_tasks (
                 id TEXT PRIMARY KEY,
                 title TEXT NOT NULL,
                 description TEXT NOT NULL,
                 acceptance_criteria TEXT NOT NULL,
                 source_prd_path TEXT NOT NULL,
                 source_task_number TEXT,
                 state TEXT NOT NULL,
                 priority INTEGER NOT NULL DEFAULT 0,
	                 preferred_model TEXT NOT NULL,
	                 role TEXT NOT NULL,
	                 parallelizable INTEGER NOT NULL DEFAULT 1,
	                 dependencies TEXT NOT NULL DEFAULT '[]',
	                 scheduling_pool TEXT NOT NULL DEFAULT 'codex',
		                 estimated_size TEXT NOT NULL DEFAULT 'small',
		                 claude_suitable INTEGER NOT NULL DEFAULT 0,
		                 assigned_worker_id TEXT,
		                 eligible_providers TEXT,
		                 lease_owner TEXT,
		                 lease_expires_at TEXT,
		                 claimed_at TEXT,
		                 completed_worker_id TEXT,
		                 completed_worker_kind TEXT,
		                 claim_duration_secs INTEGER,
		                 retry_count INTEGER NOT NULL DEFAULT 0,
		                 failure_reason TEXT,
	                 attempt INTEGER NOT NULL DEFAULT 0,
	                 max_attempts INTEGER NOT NULL DEFAULT 3,
	                 acked_at TEXT,
	                 started_at TEXT,
	                 reported_at TEXT,
	                 verified_at TEXT,
	                 completed_at TEXT,
	                 last_error TEXT,
	                 retry_reason TEXT,
	                 report_hash TEXT,
	                 created_at TEXT NOT NULL,
	                 updated_at TEXT NOT NULL
	             );
             CREATE TABLE IF NOT EXISTS service_task_reports (
                 task_id TEXT PRIMARY KEY,
                 summary TEXT NOT NULL,
                 files_inspected TEXT NOT NULL,
                 changed_files TEXT NOT NULL,
                 tests_run TEXT NOT NULL,
                 verification TEXT NOT NULL,
                 risks TEXT NOT NULL,
                 raw_report TEXT NOT NULL,
                 created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS service_prds (
                 id TEXT PRIMARY KEY,
                 path TEXT NOT NULL UNIQUE,
                 title TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS service_events (
                 id TEXT PRIMARY KEY,
                 type TEXT NOT NULL,
                 worker_id TEXT,
                 task_id TEXT,
                 payload TEXT NOT NULL,
                 created_at TEXT NOT NULL
             );",
        )?;
        // Migrations: add columns if missing (existing DBs)
        let _ = conn.execute_batch("ALTER TABLE agents ADD COLUMN session_token TEXT;");
        let _ = conn.execute_batch("ALTER TABLE agents ADD COLUMN last_seen INTEGER;");
        let _ = conn
            .execute_batch("ALTER TABLE agents ADD COLUMN status TEXT NOT NULL DEFAULT 'active';");
        let _ = conn.execute_batch("ALTER TABLE agents ADD COLUMN archived_at INTEGER;");
        let _ = conn.execute_batch("ALTER TABLE agents ADD COLUMN client_type TEXT;");
        let _ = conn.execute_batch("ALTER TABLE agents ADD COLUMN protocol_version INTEGER;");
        let _ = conn
            .execute_batch("ALTER TABLE messages ADD COLUMN kind TEXT NOT NULL DEFAULT 'note';");
        let _ = conn.execute_batch("ALTER TABLE messages ADD COLUMN task_id TEXT;");
        let _ = conn.execute_batch("ALTER TABLE messages ADD COLUMN reply_to INTEGER;");
        let _ = conn.execute(
            "UPDATE agents SET status = 'active' WHERE status IS NULL OR status = ''",
            [],
        );
        let _ = conn.execute(
            "UPDATE messages SET kind = ?1 WHERE kind IS NULL OR kind = ''",
            [DEFAULT_MESSAGE_KIND],
        );
        for migration in [
            "ALTER TABLE service_tasks ADD COLUMN attempt INTEGER NOT NULL DEFAULT 0;",
            "ALTER TABLE service_tasks ADD COLUMN max_attempts INTEGER NOT NULL DEFAULT 3;",
            "ALTER TABLE service_tasks ADD COLUMN acked_at TEXT;",
            "ALTER TABLE service_tasks ADD COLUMN started_at TEXT;",
            "ALTER TABLE service_tasks ADD COLUMN reported_at TEXT;",
            "ALTER TABLE service_tasks ADD COLUMN verified_at TEXT;",
            "ALTER TABLE service_tasks ADD COLUMN completed_at TEXT;",
            "ALTER TABLE service_tasks ADD COLUMN last_error TEXT;",
            "ALTER TABLE service_tasks ADD COLUMN retry_reason TEXT;",
            "ALTER TABLE service_tasks ADD COLUMN report_hash TEXT;",
            "ALTER TABLE service_tasks ADD COLUMN dependencies TEXT NOT NULL DEFAULT '[]';",
            "ALTER TABLE service_tasks ADD COLUMN scheduling_pool TEXT NOT NULL DEFAULT 'codex';",
            "ALTER TABLE service_tasks ADD COLUMN estimated_size TEXT NOT NULL DEFAULT 'small';",
            "ALTER TABLE service_tasks ADD COLUMN claude_suitable INTEGER NOT NULL DEFAULT 0;",
            "ALTER TABLE service_tasks ADD COLUMN eligible_providers TEXT;",
            "ALTER TABLE service_tasks ADD COLUMN lease_owner TEXT;",
            "ALTER TABLE service_tasks ADD COLUMN lease_expires_at TEXT;",
            "ALTER TABLE service_tasks ADD COLUMN claimed_at TEXT;",
            "ALTER TABLE service_tasks ADD COLUMN completed_worker_id TEXT;",
            "ALTER TABLE service_tasks ADD COLUMN completed_worker_kind TEXT;",
            "ALTER TABLE service_tasks ADD COLUMN claim_duration_secs INTEGER;",
        ] {
            let _ = conn.execute_batch(migration);
        }
        Ok(Self { conn })
    }

    pub fn create_autopilot_run(&self, prd_path: &str) -> Result<AutopilotRunRecord> {
        let prd_path = prd_path.trim();
        if prd_path.is_empty() {
            anyhow::bail!("autopilot PRD path cannot be empty");
        }

        let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        self.conn.execute(
            "INSERT INTO autopilot_runs (prd_path, status, created_at, completed_at)
             VALUES (?1, 'running', ?2, NULL)",
            params![prd_path, created_at],
        )?;

        let id = self.conn.last_insert_rowid();
        Ok(AutopilotRunRecord {
            id,
            prd_path: prd_path.to_string(),
            status: "running".to_string(),
            created_at,
            completed_at: None,
        })
    }

    pub fn get_autopilot_run(&self, id: i64) -> Result<Option<AutopilotRunRecord>> {
        self.conn
            .query_row(
                "SELECT id, prd_path, status, created_at, completed_at
                 FROM autopilot_runs
                 WHERE id = ?1",
                [id],
                map_autopilot_run_row,
            )
            .optional()
            .context("failed to fetch autopilot run")
    }

    pub fn create_autopilot_agents(
        &self,
        run_id: i64,
        agents: &[AutopilotAgentInput],
    ) -> Result<Vec<AutopilotAgentRecord>> {
        if agents.is_empty() {
            anyhow::bail!("autopilot agent list cannot be empty");
        }
        if self.get_autopilot_run(run_id)?.is_none() {
            anyhow::bail!("autopilot run does not exist: {run_id}");
        }

        let mut normalized_agents = Vec::with_capacity(agents.len());
        for agent in agents {
            let name = required_autopilot_agent_field("name", &agent.name)?;
            let role = required_autopilot_agent_field("role", &agent.role)?;
            let model_provider =
                required_autopilot_agent_field("model_provider", &agent.model_provider)?;
            let skills_prompt =
                required_autopilot_agent_field("skills_prompt", &agent.skills_prompt)?;
            normalized_agents.push(AutopilotAgentInput {
                name,
                role,
                model_provider,
                skills_prompt,
            });
        }

        let mut records = Vec::with_capacity(normalized_agents.len());
        for agent in normalized_agents {
            self.conn.execute(
                "INSERT INTO autopilot_agents (
                    run_id, name, role, model_provider, skills_prompt, status
                 ) VALUES (?1, ?2, ?3, ?4, ?5, 'planned')",
                params![
                    run_id,
                    agent.name,
                    agent.role,
                    agent.model_provider,
                    agent.skills_prompt
                ],
            )?;
            records.push(AutopilotAgentRecord {
                id: self.conn.last_insert_rowid(),
                run_id,
                name: agent.name,
                role: agent.role,
                model_provider: agent.model_provider,
                skills_prompt: agent.skills_prompt,
                status: "planned".to_string(),
            });
        }
        Ok(records)
    }

    pub fn list_autopilot_agents(&self, run_id: i64) -> Result<Vec<AutopilotAgentRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, run_id, name, role, model_provider, skills_prompt, status
             FROM autopilot_agents
             WHERE run_id = ?1
             ORDER BY id",
        )?;
        let records = stmt
            .query_map([run_id], map_autopilot_agent_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(records)
    }

    pub fn create_autopilot_tasks(
        &self,
        run_id: i64,
        tasks: &[TaskGraphTask],
    ) -> Result<Vec<AutopilotTaskRecord>> {
        if tasks.is_empty() {
            anyhow::bail!("autopilot task list cannot be empty");
        }
        if self.get_autopilot_run(run_id)?.is_none() {
            anyhow::bail!("autopilot run does not exist: {run_id}");
        }

        let mut normalized_tasks = Vec::with_capacity(tasks.len());
        let mut graph_task_ids = std::collections::BTreeSet::new();
        for task in tasks {
            let graph_task_id = required_autopilot_task_field("id", &task.id)?;
            if !graph_task_ids.insert(graph_task_id.clone()) {
                anyhow::bail!("duplicate autopilot task graph id: {graph_task_id}");
            }
            let title = required_autopilot_task_field("title", &task.title)?;
            let description = required_autopilot_task_field("description", &task.description)?;
            normalized_tasks.push((task, title, description));
        }
        for task in tasks {
            for dependency in &task.depends_on {
                let dependency = dependency.trim();
                if dependency == task.id.trim() {
                    anyhow::bail!("autopilot task '{}' cannot depend on itself", task.id);
                }
                if !graph_task_ids.contains(dependency) {
                    anyhow::bail!(
                        "autopilot task '{}' depends on missing task '{}'",
                        task.id,
                        dependency
                    );
                }
            }
        }

        let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let mut records = Vec::with_capacity(normalized_tasks.len());
        let mut graph_to_db_id = BTreeMap::new();
        for (task, title, description) in normalized_tasks {
            let acceptance_criteria = serde_json::to_string(&task.acceptance_criteria)
                .context("failed to serialize autopilot task acceptance criteria")?;
            let status = task_status_label(&task.status);
            let risk_level = risk_level_label(&task.risk_level);
            self.conn.execute(
                "INSERT INTO autopilot_tasks (
                    run_id, title, description, assigned_role, assigned_agent_id, status,
                    priority, risk_level, acceptance_criteria, created_at, completed_at
                 ) VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, ?8, ?9, NULL)",
                params![
                    run_id,
                    title,
                    description,
                    task.assigned_role.as_deref(),
                    status,
                    task.priority,
                    risk_level,
                    acceptance_criteria,
                    created_at
                ],
            )?;
            let db_id = self.conn.last_insert_rowid();
            graph_to_db_id.insert(task.id.trim().to_string(), db_id);
            records.push(AutopilotTaskRecord {
                id: db_id,
                run_id,
                title,
                description,
                assigned_role: task.assigned_role.clone(),
                assigned_agent_id: None,
                status: status.to_string(),
                priority: task.priority,
                risk_level: Some(risk_level.to_string()),
                acceptance_criteria: task.acceptance_criteria.clone(),
                created_at: created_at.clone(),
                completed_at: None,
            });
        }
        for task in tasks {
            let task_id = graph_to_db_id
                .get(task.id.trim())
                .copied()
                .with_context(|| format!("missing persisted task id for '{}'", task.id))?;
            for dependency in &task.depends_on {
                let depends_on_task_id = graph_to_db_id
                    .get(dependency.trim())
                    .copied()
                    .with_context(|| {
                        format!("missing persisted dependency id for '{}'", dependency)
                    })?;
                self.conn.execute(
                    "INSERT OR IGNORE INTO autopilot_task_dependencies (
                        task_id, depends_on_task_id
                     ) VALUES (?1, ?2)",
                    params![task_id, depends_on_task_id],
                )?;
            }
        }
        Ok(records)
    }

    pub fn list_autopilot_tasks(&self, run_id: i64) -> Result<Vec<AutopilotTaskRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, run_id, title, description, assigned_role, assigned_agent_id, status,
                    priority, risk_level, acceptance_criteria, created_at, completed_at
             FROM autopilot_tasks
             WHERE run_id = ?1
             ORDER BY id",
        )?;
        let records = stmt
            .query_map([run_id], map_autopilot_task_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(records)
    }

    pub fn ready_autopilot_tasks(&self, run_id: i64) -> Result<Vec<AutopilotTaskRecord>> {
        if self.get_autopilot_run(run_id)?.is_none() {
            anyhow::bail!("autopilot run does not exist: {run_id}");
        }

        let mut stmt = self.conn.prepare(
            "SELECT id, run_id, title, description, assigned_role, assigned_agent_id, status,
                    priority, risk_level, acceptance_criteria, created_at, completed_at
             FROM autopilot_tasks t
             WHERE t.run_id = ?1
               AND t.assigned_agent_id IS NULL
               AND t.status IN ('READY_PARALLEL', 'SEQUENTIAL')
               AND NOT EXISTS (
                   SELECT 1
                   FROM autopilot_task_dependencies d
                   JOIN autopilot_tasks dependency ON dependency.id = d.depends_on_task_id
                   WHERE d.task_id = t.id
                     AND dependency.status <> 'DONE'
               )
             ORDER BY t.priority DESC, t.id",
        )?;
        let records = stmt
            .query_map([run_id], map_autopilot_task_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(records)
    }

    pub fn assign_ready_autopilot_tasks(&self, run_id: i64) -> Result<Vec<AutopilotTaskRecord>> {
        let ready_tasks = self.ready_autopilot_tasks(run_id)?;
        if ready_tasks.is_empty() {
            return Ok(Vec::new());
        }

        let workers: Vec<AutopilotAgentRecord> = self
            .list_autopilot_agents(run_id)?
            .into_iter()
            .filter(|agent| agent.role != "manager" && agent.role != "inspector")
            .collect();
        if workers.is_empty() {
            anyhow::bail!("autopilot run {run_id} has no worker agents");
        }

        let workers_by_role: BTreeMap<String, AutopilotAgentRecord> = workers
            .iter()
            .cloned()
            .map(|worker| (worker.role.clone(), worker))
            .collect();
        let claude_worker = workers
            .iter()
            .find(|worker| worker.model_provider.eq_ignore_ascii_case("claude"));
        let codex_worker = workers
            .iter()
            .find(|worker| worker.model_provider.eq_ignore_ascii_case("codex"));
        let adaptive_policy = AdaptiveSchedulingConfig::default();
        let mut assigned = Vec::with_capacity(ready_tasks.len());
        let mut next_worker_index = 0usize;
        for task in ready_tasks {
            let worker = if let Some(role) = task.assigned_role.as_deref() {
                let role_worker = workers_by_role.get(role).with_context(|| {
                    format!(
                        "ready autopilot task '{}' has no worker for role '{role}'",
                        task.id
                    )
                })?;
                let worker = if role_worker.model_provider.eq_ignore_ascii_case("claude")
                    && !autopilot_record_is_claude_eligible(&task, &adaptive_policy)
                {
                    codex_worker.unwrap_or(role_worker)
                } else {
                    role_worker
                };
                if let Some(worker_index) = workers
                    .iter()
                    .position(|candidate| candidate.id == worker.id)
                {
                    next_worker_index = worker_index + 1;
                }
                worker
            } else if let Some(worker) = claude_worker
                .filter(|_| autopilot_record_is_claude_eligible(&task, &adaptive_policy))
            {
                worker
            } else if claude_worker.is_some() {
                if let Some(worker) = codex_worker {
                    worker
                } else {
                    let worker = &workers[next_worker_index % workers.len()];
                    next_worker_index += 1;
                    worker
                }
            } else {
                let worker = &workers[next_worker_index % workers.len()];
                next_worker_index += 1;
                worker
            };
            self.conn.execute(
                "UPDATE autopilot_tasks
                 SET assigned_agent_id = ?1,
                     assigned_role = COALESCE(assigned_role, ?2)
                WHERE id = ?3
                   AND run_id = ?4
                   AND assigned_agent_id IS NULL",
                params![worker.id, worker.role.as_str(), task.id, run_id],
            )?;
            assigned
                .push(self.get_autopilot_task(task.id)?.with_context(|| {
                    format!("assigned autopilot task disappeared: {}", task.id)
                })?);
        }
        Ok(assigned)
    }

    pub fn autopilot_task_launch_blockers(&self, task_id: i64) -> Result<Vec<String>> {
        let task = self
            .get_autopilot_task(task_id)?
            .with_context(|| format!("autopilot task does not exist: {task_id}"))?;
        let mut blockers = Vec::new();
        if task.assigned_agent_id.is_some() {
            blockers.push("task is already assigned".to_string());
        }
        if !matches!(task.status.as_str(), "READY_PARALLEL" | "SEQUENTIAL") {
            blockers.push(format!("task status is {}", task.status));
        }

        let mut stmt = self.conn.prepare(
            "SELECT dependency.id, dependency.title, dependency.status
             FROM autopilot_task_dependencies d
             JOIN autopilot_tasks dependency ON dependency.id = d.depends_on_task_id
             WHERE d.task_id = ?1
               AND dependency.status <> 'DONE'
             ORDER BY dependency.id",
        )?;
        let dependency_blockers = stmt
            .query_map([task_id], |row| {
                let id: i64 = row.get(0)?;
                let title: String = row.get(1)?;
                let status: String = row.get(2)?;
                Ok(format!("dependency {id} ({title}) is {status}"))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        blockers.extend(dependency_blockers);
        Ok(blockers)
    }

    pub fn autopilot_task_status_counts(&self, run_id: i64) -> Result<AutopilotTaskStatusCounts> {
        if self.get_autopilot_run(run_id)?.is_none() {
            anyhow::bail!("autopilot run does not exist: {run_id}");
        }
        let mut counts = AutopilotTaskStatusCounts::default();
        let mut stmt = self.conn.prepare(
            "SELECT status, COUNT(*)
             FROM autopilot_tasks
             WHERE run_id = ?1
             GROUP BY status",
        )?;
        let rows = stmt
            .query_map([run_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (status, count) in rows {
            match status.as_str() {
                "READY_PARALLEL" => counts.ready_parallel = count,
                "BLOCKED" => counts.blocked = count,
                "SEQUENTIAL" => counts.sequential = count,
                "REVIEW_REQUIRED" => counts.review_required = count,
                "DONE" => counts.done = count,
                "FAILED" => counts.failed = count,
                _ => {}
            }
        }
        Ok(counts)
    }

    pub fn get_autopilot_task(&self, id: i64) -> Result<Option<AutopilotTaskRecord>> {
        self.conn
            .query_row(
                "SELECT id, run_id, title, description, assigned_role, assigned_agent_id, status,
                        priority, risk_level, acceptance_criteria, created_at, completed_at
                 FROM autopilot_tasks
                 WHERE id = ?1",
                [id],
                map_autopilot_task_row,
            )
            .optional()
            .context("failed to fetch autopilot task")
    }

    pub fn submit_autopilot_task_for_review(&self, task_id: i64) -> Result<AutopilotTaskRecord> {
        let task = self
            .get_autopilot_task(task_id)?
            .with_context(|| format!("autopilot task does not exist: {task_id}"))?;
        if task.assigned_agent_id.is_none() {
            anyhow::bail!("autopilot task {task_id} is not assigned to a worker");
        }
        if !matches!(task.status.as_str(), "READY_PARALLEL" | "SEQUENTIAL") {
            anyhow::bail!(
                "autopilot task {task_id} cannot be submitted for review from status {}",
                task.status
            );
        }

        let completed_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let updated = self.conn.execute(
            "UPDATE autopilot_tasks
             SET status = 'REVIEW_REQUIRED',
                 completed_at = ?1
             WHERE id = ?2
               AND status IN ('READY_PARALLEL', 'SEQUENTIAL')
               AND assigned_agent_id IS NOT NULL",
            params![completed_at, task_id],
        )?;
        ensure_autopilot_task_updated(updated, task_id)?;
        self.get_autopilot_task(task_id)?
            .with_context(|| format!("review autopilot task disappeared: {task_id}"))
    }

    pub fn create_autopilot_review(
        &self,
        task_id: i64,
        reviewer_agent_id: Option<i64>,
        verdict: &str,
        notes: Option<&str>,
    ) -> Result<AutopilotReviewRecord> {
        let task = self
            .get_autopilot_task(task_id)?
            .with_context(|| format!("autopilot task does not exist: {task_id}"))?;
        let verdict = normalized_review_verdict(verdict)?;
        if let Some(reviewer_agent_id) = reviewer_agent_id {
            let agent_run_id = self
                .conn
                .query_row(
                    "SELECT run_id FROM autopilot_agents WHERE id = ?1",
                    [reviewer_agent_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .with_context(|| {
                    format!("autopilot reviewer agent does not exist: {reviewer_agent_id}")
                })?;
            if agent_run_id != task.run_id {
                anyhow::bail!(
                    "autopilot reviewer agent {reviewer_agent_id} does not belong to run {}",
                    task.run_id
                );
            }
        }

        let notes = notes.and_then(|value| {
            let value = value.trim();
            if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            }
        });
        let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        self.conn.execute(
            "INSERT INTO autopilot_reviews (
                task_id, reviewer_agent_id, verdict, notes, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                task_id,
                reviewer_agent_id,
                verdict,
                notes.as_deref(),
                created_at
            ],
        )?;
        Ok(AutopilotReviewRecord {
            id: self.conn.last_insert_rowid(),
            task_id,
            reviewer_agent_id,
            verdict: verdict.to_string(),
            notes,
            created_at,
        })
    }

    pub fn list_autopilot_reviews(&self, task_id: i64) -> Result<Vec<AutopilotReviewRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, task_id, reviewer_agent_id, verdict, notes, created_at
             FROM autopilot_reviews
             WHERE task_id = ?1
             ORDER BY id",
        )?;
        let records = stmt
            .query_map([task_id], map_autopilot_review_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(records)
    }

    pub fn accept_autopilot_task_result(
        &self,
        task_id: i64,
        reviewer_agent_id: Option<i64>,
        notes: Option<&str>,
    ) -> Result<AutopilotTaskRecord> {
        let task = self
            .get_autopilot_task(task_id)?
            .with_context(|| format!("autopilot task does not exist: {task_id}"))?;
        if task.status != "REVIEW_REQUIRED" {
            anyhow::bail!(
                "autopilot task {task_id} cannot be accepted from status {}",
                task.status
            );
        }
        self.create_autopilot_review(task_id, reviewer_agent_id, "accepted", notes)?;

        let completed_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let updated = self.conn.execute(
            "UPDATE autopilot_tasks
             SET status = 'DONE',
                 completed_at = ?1
             WHERE id = ?2
               AND status = 'REVIEW_REQUIRED'",
            params![completed_at, task_id],
        )?;
        ensure_autopilot_task_updated(updated, task_id)?;
        self.get_autopilot_task(task_id)?
            .with_context(|| format!("accepted autopilot task disappeared: {task_id}"))
    }

    pub fn reject_autopilot_task_result(
        &self,
        task_id: i64,
        reviewer_agent_id: Option<i64>,
        notes: Option<&str>,
    ) -> Result<AutopilotTaskRecord> {
        let task = self
            .get_autopilot_task(task_id)?
            .with_context(|| format!("autopilot task does not exist: {task_id}"))?;
        if task.status != "REVIEW_REQUIRED" {
            anyhow::bail!(
                "autopilot task {task_id} cannot be rejected from status {}",
                task.status
            );
        }
        self.create_autopilot_review(task_id, reviewer_agent_id, "rejected", notes)?;

        let updated = self.conn.execute(
            "UPDATE autopilot_tasks
             SET status = 'FAILED'
             WHERE id = ?1
               AND status = 'REVIEW_REQUIRED'",
            [task_id],
        )?;
        ensure_autopilot_task_updated(updated, task_id)?;
        self.get_autopilot_task(task_id)?
            .with_context(|| format!("rejected autopilot task disappeared: {task_id}"))
    }

    pub fn requeue_failed_autopilot_task(&self, task_id: i64) -> Result<AutopilotTaskRecord> {
        let task = self
            .get_autopilot_task(task_id)?
            .with_context(|| format!("autopilot task does not exist: {task_id}"))?;
        if task.status != "FAILED" {
            anyhow::bail!(
                "autopilot task {task_id} cannot be requeued from status {}",
                task.status
            );
        }
        let updated = self.conn.execute(
            "UPDATE autopilot_tasks
             SET status = 'READY_PARALLEL',
                 assigned_agent_id = NULL,
                 completed_at = NULL
             WHERE id = ?1
               AND status = 'FAILED'",
            [task_id],
        )?;
        ensure_autopilot_task_updated(updated, task_id)?;
        self.get_autopilot_task(task_id)?
            .with_context(|| format!("requeued autopilot task disappeared: {task_id}"))
    }

    pub fn promote_failed_autopilot_task(&self, task_id: i64) -> Result<AutopilotTaskRecord> {
        let task = self
            .get_autopilot_task(task_id)?
            .with_context(|| format!("autopilot task does not exist: {task_id}"))?;
        if task.status != "FAILED" {
            anyhow::bail!(
                "autopilot task {task_id} cannot be promoted from status {}",
                task.status
            );
        }
        let current_rank = if let Some(agent_id) = task.assigned_agent_id {
            self.autopilot_agent_model_rank(agent_id)?.unwrap_or(0)
        } else {
            0
        };
        let workers = self.list_autopilot_agents(task.run_id)?;
        let promoted_worker = workers
            .iter()
            .filter(|agent| agent.role != "manager" && agent.role != "inspector")
            .filter_map(|agent| {
                let rank = model_provider_rank(&agent.model_provider);
                if rank > current_rank {
                    Some((rank, agent))
                } else {
                    None
                }
            })
            .max_by_key(|(rank, agent)| (*rank, -agent.id))
            .map(|(_, agent)| agent)
            .with_context(|| format!("autopilot task {task_id} has no stronger worker model"))?;

        let updated = self.conn.execute(
            "UPDATE autopilot_tasks
             SET status = 'READY_PARALLEL',
                 assigned_agent_id = ?1,
                 assigned_role = ?2,
                 completed_at = NULL
             WHERE id = ?3
               AND status = 'FAILED'",
            params![promoted_worker.id, promoted_worker.role.as_str(), task_id],
        )?;
        ensure_autopilot_task_updated(updated, task_id)?;
        self.get_autopilot_task(task_id)?
            .with_context(|| format!("promoted autopilot task disappeared: {task_id}"))
    }

    pub fn autopilot_run_acceptance_satisfied(&self, run_id: i64) -> Result<bool> {
        if self.get_autopilot_run(run_id)?.is_none() {
            anyhow::bail!("autopilot run does not exist: {run_id}");
        }
        let unfinished: i64 = self.conn.query_row(
            "SELECT COUNT(*)
             FROM autopilot_tasks
             WHERE run_id = ?1
               AND status <> 'DONE'",
            [run_id],
            |row| row.get(0),
        )?;
        Ok(unfinished == 0)
    }

    pub fn complete_autopilot_run_if_accepted(
        &self,
        run_id: i64,
    ) -> Result<Option<AutopilotRunRecord>> {
        if !self.autopilot_run_acceptance_satisfied(run_id)? {
            return Ok(None);
        }
        Ok(Some(self.mark_autopilot_run_completed(run_id)?))
    }

    pub fn mark_autopilot_run_completed(&self, run_id: i64) -> Result<AutopilotRunRecord> {
        if self.get_autopilot_run(run_id)?.is_none() {
            anyhow::bail!("autopilot run does not exist: {run_id}");
        }
        let completed_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        self.conn.execute(
            "UPDATE autopilot_runs
             SET status = 'completed',
                 completed_at = ?1
             WHERE id = ?2",
            params![completed_at, run_id],
        )?;
        self.get_autopilot_run(run_id)?
            .with_context(|| format!("completed autopilot run disappeared: {run_id}"))
    }

    fn autopilot_agent_model_rank(&self, agent_id: i64) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT model_provider FROM autopilot_agents WHERE id = ?1",
                [agent_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|provider| model_provider_rank(&provider)))
    }

    pub fn list_autopilot_task_dependencies(
        &self,
        run_id: i64,
    ) -> Result<Vec<AutopilotTaskDependencyRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT d.task_id, d.depends_on_task_id
             FROM autopilot_task_dependencies d
             JOIN autopilot_tasks t ON t.id = d.task_id
             WHERE t.run_id = ?1
             ORDER BY d.task_id, d.depends_on_task_id",
        )?;
        let records = stmt
            .query_map([run_id], map_autopilot_task_dependency_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(records)
    }

    pub fn create_autopilot_terminal_sessions(
        &self,
        run_id: i64,
        sessions: &[TerminalSessionPlan],
    ) -> Result<Vec<AutopilotTerminalSessionRecord>> {
        if sessions.is_empty() {
            anyhow::bail!("autopilot terminal session list cannot be empty");
        }
        if self.get_autopilot_run(run_id)?.is_none() {
            anyhow::bail!("autopilot run does not exist: {run_id}");
        }

        let agents = self.list_autopilot_agents(run_id)?;
        let agent_ids_by_role: BTreeMap<String, i64> = agents
            .into_iter()
            .map(|agent| (agent.role, agent.id))
            .collect();

        let mut records = Vec::with_capacity(sessions.len());
        for session in sessions {
            let role_id = session.role_id.trim();
            let Some(agent_id) = agent_ids_by_role.get(role_id).copied() else {
                anyhow::bail!(
                    "autopilot terminal session role '{}' has no persisted agent",
                    session.role_id
                );
            };
            let terminal_kind = terminal_kind_label(&session.terminal_kind);
            let status = terminal_session_status_label(&session.status);
            records.push(AutopilotTerminalSessionRecord {
                id: 0,
                run_id,
                agent_id,
                terminal_kind: terminal_kind.to_string(),
                command: session.command.clone(),
                status: status.to_string(),
            });
        }

        let existing = self.list_autopilot_terminal_sessions(run_id)?;
        if !existing.is_empty() {
            if autopilot_terminal_sessions_match_plan(&existing, &records) {
                return Ok(existing);
            }
            anyhow::bail!("autopilot run {run_id} already has a different terminal session plan");
        }

        for record in &mut records {
            self.conn.execute(
                "INSERT INTO autopilot_terminal_sessions (
                    run_id, agent_id, terminal_kind, command, status
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    run_id,
                    record.agent_id,
                    record.terminal_kind,
                    record.command,
                    record.status
                ],
            )?;
            record.id = self.conn.last_insert_rowid();
        }
        Ok(records)
    }

    pub fn list_autopilot_terminal_sessions(
        &self,
        run_id: i64,
    ) -> Result<Vec<AutopilotTerminalSessionRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, run_id, agent_id, terminal_kind, command, status
             FROM autopilot_terminal_sessions
             WHERE run_id = ?1
             ORDER BY id",
        )?;
        let records = stmt
            .query_map([run_id], map_autopilot_terminal_session_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(records)
    }

    pub fn register_agent(&self, id: &str, role: &str) -> Result<String> {
        self.register_agent_with_metadata(id, role, None, None)
    }

    pub fn register_agent_with_metadata(
        &self,
        id: &str,
        role: &str,
        client_type: Option<&str>,
        protocol_version: Option<i64>,
    ) -> Result<String> {
        let now = chrono::Utc::now().timestamp();
        let token = uuid::Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT OR REPLACE INTO agents (
                id, role, joined_at, session_token, status, archived_at, client_type, protocol_version
             ) VALUES (?1, ?2, ?3, ?4, 'active', NULL, ?5, ?6)",
            rusqlite::params![id, role, now, token, client_type, protocol_version],
        )?;
        Ok(token)
    }

    /// Register with automatic ID suffixing if the requested ID is taken.
    /// Returns (actual_id, session_token).
    pub fn register_agent_unique(
        &self,
        requested_id: &str,
        role: &str,
    ) -> Result<(String, String)> {
        self.register_agent_unique_with_metadata(requested_id, role, None, None)
    }

    pub fn register_agent_unique_with_metadata(
        &self,
        requested_id: &str,
        role: &str,
        client_type: Option<&str>,
        protocol_version: Option<i64>,
    ) -> Result<(String, String)> {
        let now = chrono::Utc::now().timestamp();
        let candidates = std::iter::once(requested_id.to_string())
            .chain((2..=99).map(|i| format!("{}-{}", requested_id, i)));
        for candidate in candidates {
            let token = uuid::Uuid::new_v4().to_string();
            let reactivated = self.conn.execute(
                "UPDATE agents
                 SET role = ?2, joined_at = ?3, session_token = ?4, status = 'active', archived_at = NULL,
                     client_type = ?5, protocol_version = ?6
                 WHERE id = ?1 AND status = 'archived'",
                rusqlite::params![candidate, role, now, token, client_type, protocol_version],
            )?;
            if reactivated > 0 {
                return Ok((candidate, token));
            }

            let inserted = self.conn.execute(
                "INSERT OR IGNORE INTO agents (
                    id, role, joined_at, session_token, status, archived_at, client_type, protocol_version
                 ) VALUES (?1, ?2, ?3, ?4, 'active', NULL, ?5, ?6)",
                rusqlite::params![candidate, role, now, token, client_type, protocol_version],
            )?;
            if inserted > 0 {
                return Ok((candidate, token));
            }
        }
        anyhow::bail!("Too many agents with base ID '{}'", requested_id);
    }

    pub fn get_session_token(&self, id: &str) -> Result<Option<String>> {
        let token: Option<String> = self
            .conn
            .query_row(
                "SELECT session_token FROM agents WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(token)
    }

    fn agent_status(&self, id: &str) -> Result<Option<String>> {
        let status = self
            .conn
            .query_row("SELECT status FROM agents WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(status)
    }

    pub fn unregister_agent(&self, id: &str) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        let updated = self.conn.execute(
            "UPDATE agents
             SET status = 'archived', archived_at = ?2
             WHERE id = ?1 AND status = 'active'",
            rusqlite::params![id, now],
        )?;
        if updated == 1 {
            return Ok(());
        }

        match self.agent_status(id)?.as_deref() {
            Some("archived") => {
                anyhow::bail!("{id} is archived. Re-join with `squad join {id}` to reactivate it.")
            }
            Some(_) | None => {
                let names = self.agent_names()?;
                anyhow::bail!("{id} does not exist. Online agents: {}", names.join(", "))
            }
        }
    }

    pub fn list_agents(&self, include_archived: bool) -> Result<Vec<AgentRecord>> {
        let sql = if include_archived {
            "SELECT id, role, joined_at, last_seen, status, archived_at, client_type, protocol_version FROM agents ORDER BY joined_at"
        } else {
            "SELECT id, role, joined_at, last_seen, status, archived_at, client_type, protocol_version FROM agents WHERE status = 'active' ORDER BY joined_at"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let agents = stmt
            .query_map([], |row| {
                Ok(AgentRecord {
                    id: row.get(0)?,
                    role: row.get(1)?,
                    joined_at: row.get(2)?,
                    last_seen: row.get(3)?,
                    status: row.get(4)?,
                    archived_at: row.get(5)?,
                    client_type_raw: row.get(6)?,
                    protocol_version_raw: row.get(7)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(agents)
    }

    /// Update last_seen timestamp for an agent.
    pub fn touch_agent(&self, id: &str) -> Result<()> {
        self.require_active_agent(id)?;
        let now = chrono::Utc::now().timestamp();
        self.conn.execute(
            "UPDATE agents SET last_seen = ?1 WHERE id = ?2",
            rusqlite::params![now, id],
        )?;
        Ok(())
    }

    pub fn agent_exists(&self, id: &str) -> Result<bool> {
        let exists: bool = self.conn.query_row(
            "SELECT COUNT(*) > 0 FROM agents WHERE id = ?1 AND status = 'active'",
            [id],
            |row| row.get(0),
        )?;
        Ok(exists)
    }

    pub fn require_active_agent(&self, id: &str) -> Result<()> {
        match self.agent_status(id)?.as_deref() {
            Some("active") => Ok(()),
            Some("archived") => {
                anyhow::bail!("{id} is archived. Re-join with `squad join {id}` to reactivate it.")
            }
            Some(_) | None => {
                let names = self.agent_names()?;
                anyhow::bail!("{id} does not exist. Online agents: {}", names.join(", "))
            }
        }
    }

    fn agent_names(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM agents WHERE status = 'active' ORDER BY id")?;
        let names = stmt
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(names)
    }

    pub fn send_message(&self, from: &str, to: &str, content: &str) -> Result<()> {
        self.send_message_envelope(from, to, content, DEFAULT_MESSAGE_KIND, None, None)
    }

    fn send_message_envelope(
        &self,
        from: &str,
        to: &str,
        content: &str,
        kind: &str,
        task_id: Option<&str>,
        reply_to: Option<i64>,
    ) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        self.conn.execute(
            "INSERT INTO messages (from_agent, to_agent, content, created_at, read, kind, task_id, reply_to)
             VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7)",
            params![from, to, content, now, kind, task_id, reply_to],
        )?;
        Ok(())
    }

    pub fn send_message_checked(&self, from: &str, to: &str, content: &str) -> Result<()> {
        self.require_active_agent(to)?;
        self.send_message(from, to, content)
    }

    pub fn send_message_checked_with_metadata(
        &self,
        from: &str,
        to: &str,
        content: &str,
        task_id: Option<&str>,
        reply_to: Option<i64>,
    ) -> Result<()> {
        self.require_active_agent(to)?;
        self.send_message_envelope(from, to, content, DEFAULT_MESSAGE_KIND, task_id, reply_to)
    }

    /// Broadcast a message to all agents except the sender.
    pub fn broadcast_message(&self, from: &str, content: &str) -> Result<Vec<String>> {
        let agents = self.agent_names()?;
        let recipients: Vec<_> = agents.into_iter().filter(|a| a != from).collect();
        for to in &recipients {
            self.send_message(from, to, content)?;
        }
        Ok(recipients)
    }

    /// Atomically read and mark messages as read using a transaction.
    pub fn receive_messages(&self, agent_id: &str) -> Result<Vec<MessageRecord>> {
        self.require_active_agent(agent_id)?;
        let tx = self.conn.unchecked_transaction()?;
        let mut stmt = tx.prepare(
            "SELECT id, from_agent, to_agent, content, created_at, read, kind, task_id, reply_to
             FROM messages WHERE to_agent = ?1 AND read = 0 ORDER BY created_at, id",
        )?;
        let messages: Vec<MessageRecord> = stmt
            .query_map([agent_id], |row| {
                Ok(MessageRecord {
                    id: row.get(0)?,
                    from_agent: row.get(1)?,
                    to_agent: row.get(2)?,
                    content: row.get(3)?,
                    created_at: row.get(4)?,
                    read: row.get(5)?,
                    kind: row.get(6)?,
                    task_id: row.get(7)?,
                    reply_to: row.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);

        if !messages.is_empty() {
            let ids: Vec<i64> = messages.iter().map(|msg| msg.id).collect();
            let placeholders = std::iter::repeat_n("?", ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let sql =
                format!("UPDATE messages SET read = 1 WHERE read = 0 AND id IN ({placeholders})");
            tx.execute(&sql, params_from_iter(ids))?;
        }
        tx.commit()?;
        Ok(messages)
    }

    /// Check if there are unread messages for an agent (used by --wait).
    pub fn has_unread_messages(&self, agent_id: &str) -> Result<bool> {
        self.require_active_agent(agent_id)?;
        let has: bool = self.conn.query_row(
            "SELECT COUNT(*) > 0 FROM messages WHERE to_agent = ?1 AND read = 0",
            [agent_id],
            |row| row.get(0),
        )?;
        Ok(has)
    }

    pub fn pending_messages(&self) -> Result<Vec<MessageRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, from_agent, to_agent, content, created_at, read, kind, task_id, reply_to
             FROM messages WHERE read = 0 ORDER BY created_at, id",
        )?;
        let messages = stmt
            .query_map([], |row| {
                Ok(MessageRecord {
                    id: row.get(0)?,
                    from_agent: row.get(1)?,
                    to_agent: row.get(2)?,
                    content: row.get(3)?,
                    created_at: row.get(4)?,
                    read: row.get(5)?,
                    kind: row.get(6)?,
                    task_id: row.get(7)?,
                    reply_to: row.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(messages)
    }

    /// All messages (including read), optionally filtered by agent.
    pub fn all_messages(&self, agent_id: Option<&str>) -> Result<Vec<MessageRecord>> {
        fn map_row(row: &rusqlite::Row) -> rusqlite::Result<MessageRecord> {
            Ok(MessageRecord {
                id: row.get(0)?,
                from_agent: row.get(1)?,
                to_agent: row.get(2)?,
                content: row.get(3)?,
                created_at: row.get(4)?,
                read: row.get(5)?,
                kind: row.get(6)?,
                task_id: row.get(7)?,
                reply_to: row.get(8)?,
            })
        }

        let messages = match agent_id {
            Some(id) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, from_agent, to_agent, content, created_at, read, kind, task_id, reply_to
                     FROM messages WHERE from_agent = ?1 OR to_agent = ?1 ORDER BY created_at, id",
                )?;
                let rows = stmt
                    .query_map([id], map_row)?
                    .collect::<Result<Vec<_>, _>>()?;
                rows
            }
            None => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, from_agent, to_agent, content, created_at, read, kind, task_id, reply_to
                     FROM messages ORDER BY created_at, id",
                )?;
                let rows = stmt
                    .query_map([], map_row)?
                    .collect::<Result<Vec<_>, _>>()?;
                rows
            }
        };
        Ok(messages)
    }

    pub fn create_task(
        &self,
        created_by: &str,
        assigned_to: &str,
        title: &str,
        body: &str,
    ) -> Result<String> {
        let now = chrono::Utc::now().timestamp();
        let task_id = uuid::Uuid::new_v4().to_string();
        let tx = self.conn.unchecked_transaction()?;
        let inserted = tx.execute(
            "INSERT INTO tasks (
                 id, title, body, created_by, assigned_to, status,
                 lease_owner, lease_expires_at, result_summary,
                 created_at, updated_at, completed_at
             )
             SELECT ?1, ?2, ?3, creator.id, assignee.id, ?4,
                    NULL, NULL, NULL, ?5, ?5, NULL
             FROM agents AS creator
             JOIN agents AS assignee
               ON assignee.id = ?7
              AND assignee.status = 'active'
             WHERE creator.id = ?6
               AND creator.status = 'active'",
            params![
                task_id,
                title,
                body,
                TASK_STATUS_QUEUED,
                now,
                created_by,
                assigned_to,
            ],
        )?;
        if inserted != 1 {
            let created_by_status: Option<String> = tx
                .query_row(
                    "SELECT status FROM agents WHERE id = ?1",
                    [created_by],
                    |row| row.get(0),
                )
                .optional()?;
            match created_by_status.as_deref() {
                Some("active") => {}
                Some("archived") => {
                    anyhow::bail!(
                        "{created_by} is archived. Re-join with `squad join {created_by}` to reactivate it."
                    )
                }
                Some(_) | None => {
                    let names = self.agent_names()?;
                    anyhow::bail!(
                        "{created_by} does not exist. Online agents: {}",
                        names.join(", ")
                    )
                }
            }

            let assigned_to_status: Option<String> = tx
                .query_row(
                    "SELECT status FROM agents WHERE id = ?1",
                    [assigned_to],
                    |row| row.get(0),
                )
                .optional()?;
            match assigned_to_status.as_deref() {
                Some("active") => anyhow::bail!("failed to create task for {assigned_to}"),
                Some("archived") => {
                    anyhow::bail!(
                        "{assigned_to} is archived. Re-join with `squad join {assigned_to}` to reactivate it."
                    )
                }
                Some(_) | None => {
                    let names = self.agent_names()?;
                    anyhow::bail!(
                        "{assigned_to} does not exist. Online agents: {}",
                        names.join(", ")
                    )
                }
            }
        }
        tx.execute(
            "INSERT INTO messages (from_agent, to_agent, content, created_at, read, kind, task_id, reply_to)
             VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, NULL)",
            params![
                created_by,
                assigned_to,
                title,
                now,
                TASK_ASSIGNED_KIND,
                task_id.as_str(),
            ],
        )?;
        tx.commit()?;
        Ok(task_id)
    }

    pub fn get_task(&self, task_id: &str) -> Result<Option<TaskRecord>> {
        let task = self
            .conn
            .query_row(
                "SELECT id, title, body, created_by, assigned_to, status, lease_owner,
                        lease_expires_at, result_summary, created_at, updated_at, completed_at
                 FROM tasks WHERE id = ?1",
                [task_id],
                map_task_row,
            )
            .optional()?;
        Ok(task)
    }

    pub fn list_tasks(
        &self,
        assigned_to: Option<&str>,
        status: Option<&str>,
    ) -> Result<Vec<TaskRecord>> {
        let tasks = match (assigned_to, status) {
            (Some(agent), Some(status)) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, title, body, created_by, assigned_to, status, lease_owner,
                            lease_expires_at, result_summary, created_at, updated_at, completed_at
                     FROM tasks
                     WHERE assigned_to = ?1 AND status = ?2
                     ORDER BY created_at, title, id",
                )?;
                let rows = stmt
                    .query_map(params![agent, status], map_task_row)?
                    .collect::<Result<Vec<_>, _>>()?;
                rows
            }
            (Some(agent), None) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, title, body, created_by, assigned_to, status, lease_owner,
                            lease_expires_at, result_summary, created_at, updated_at, completed_at
                     FROM tasks
                     WHERE assigned_to = ?1
                     ORDER BY created_at, title, id",
                )?;
                let rows = stmt
                    .query_map([agent], map_task_row)?
                    .collect::<Result<Vec<_>, _>>()?;
                rows
            }
            (None, Some(status)) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, title, body, created_by, assigned_to, status, lease_owner,
                            lease_expires_at, result_summary, created_at, updated_at, completed_at
                     FROM tasks
                     WHERE status = ?1
                     ORDER BY created_at, title, id",
                )?;
                let rows = stmt
                    .query_map([status], map_task_row)?
                    .collect::<Result<Vec<_>, _>>()?;
                rows
            }
            (None, None) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, title, body, created_by, assigned_to, status, lease_owner,
                            lease_expires_at, result_summary, created_at, updated_at, completed_at
                     FROM tasks
                     ORDER BY created_at, title, id",
                )?;
                let rows = stmt
                    .query_map([], map_task_row)?
                    .collect::<Result<Vec<_>, _>>()?;
                rows
            }
        };
        Ok(tasks)
    }

    pub fn ack_task(&self, agent_id: &str, task_id: &str) -> Result<()> {
        self.require_active_agent(agent_id)?;
        let task = self.require_task(task_id)?;
        if task.status != TASK_STATUS_QUEUED {
            anyhow::bail!("task {task_id} is not queued");
        }
        if task.assigned_to.as_deref() != Some(agent_id) {
            anyhow::bail!("task {task_id} is not assigned to {agent_id}");
        }

        let now = chrono::Utc::now().timestamp();
        let lease_expires_at = now + TASK_LEASE_SECS;
        let updated = self.conn.execute(
            "UPDATE tasks
             SET status = ?1, lease_owner = ?2, lease_expires_at = ?3, updated_at = ?4
             WHERE id = ?5 AND status = ?6 AND assigned_to = ?2",
            params![
                TASK_STATUS_ACKED,
                agent_id,
                lease_expires_at,
                now,
                task_id,
                TASK_STATUS_QUEUED,
            ],
        )?;
        ensure_task_updated(updated, task_id)?;
        Ok(())
    }

    pub fn complete_task(&self, agent_id: &str, task_id: &str, result_summary: &str) -> Result<()> {
        self.require_active_agent(agent_id)?;
        let task = self.require_task(task_id)?;
        if task.status != TASK_STATUS_ACKED {
            anyhow::bail!("task {task_id} is not acked");
        }
        if task.lease_owner.as_deref() != Some(agent_id) {
            anyhow::bail!("task {task_id} is not leased by {agent_id}");
        }

        let now = chrono::Utc::now().timestamp();
        let updated = self.conn.execute(
            "UPDATE tasks
             SET status = ?1, result_summary = ?2, completed_at = ?3, updated_at = ?3
             WHERE id = ?4 AND status = ?5 AND lease_owner = ?6",
            params![
                TASK_STATUS_COMPLETED,
                result_summary,
                now,
                task_id,
                TASK_STATUS_ACKED,
                agent_id,
            ],
        )?;
        ensure_task_updated(updated, task_id)?;
        Ok(())
    }

    pub fn requeue_task(&self, task_id: &str, new_assignee: Option<&str>) -> Result<()> {
        let task = self.require_task(task_id)?;
        if let Some(agent_id) = new_assignee {
            self.require_active_agent(agent_id)?;
        }

        let now = chrono::Utc::now().timestamp();
        let updated = self.conn.execute(
            "UPDATE tasks
             SET assigned_to = ?1,
                 status = ?2,
                 lease_owner = NULL,
                 lease_expires_at = NULL,
                 result_summary = NULL,
                 completed_at = NULL,
                 updated_at = ?3
             WHERE id = ?4
               AND status = ?5
               AND assigned_to IS ?6
               AND lease_owner IS ?7
               AND lease_expires_at IS ?8
               AND completed_at IS ?9
               AND result_summary IS ?10",
            params![
                new_assignee,
                TASK_STATUS_QUEUED,
                now,
                task_id,
                task.status,
                task.assigned_to,
                task.lease_owner,
                task.lease_expires_at,
                task.completed_at,
                task.result_summary,
            ],
        )?;
        ensure_task_updated(updated, task_id)?;
        Ok(())
    }

    fn require_task(&self, task_id: &str) -> Result<TaskRecord> {
        self.get_task(task_id)?
            .with_context(|| format!("task {task_id} does not exist"))
    }

    pub fn service_health(&self) -> Result<ServiceHealthRecord> {
        let _: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM service_tasks", [], |row| row.get(0))
            .context("failed to query service task table")?;
        Ok(ServiceHealthRecord {
            status: "ok".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            database: "ok".to_string(),
            scheduler: "ready".to_string(),
        })
    }

    pub fn service_register_worker(
        &self,
        input: ServiceWorkerInput,
    ) -> Result<ServiceWorkerRecord> {
        let id = required_service_field("worker id", &input.id)?;
        let kind = normalized_worker_kind(&input.kind)?;
        let role = required_service_field("worker role", &input.role)?;
        let status =
            normalized_worker_status(input.status.as_deref().unwrap_or(WORKER_STATE_READY))?;
        let capacity = input.capacity.unwrap_or(1);
        if capacity < 1 {
            anyhow::bail!("worker capacity must be positive");
        }
        let now = service_now();
        self.conn.execute(
            "INSERT INTO service_workers (
                id, kind, role, status, capacity, current_task_id, last_heartbeat_at,
                metadata, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, ?8, ?8)
             ON CONFLICT(id) DO UPDATE SET
                kind = excluded.kind,
                role = excluded.role,
                status = excluded.status,
                capacity = excluded.capacity,
                last_heartbeat_at = excluded.last_heartbeat_at,
                metadata = excluded.metadata,
                updated_at = excluded.updated_at",
            params![
                id,
                kind,
                role,
                status,
                capacity,
                now,
                input.metadata.as_deref(),
                now
            ],
        )?;
        self.service_record_event(
            "worker.registered",
            Some(&id),
            None,
            &serde_json::json!({"kind": kind, "role": role}),
        )?;
        self.service_get_worker(&id)?
            .with_context(|| format!("registered service worker disappeared: {id}"))
    }

    pub fn service_heartbeat_worker(
        &self,
        worker_id: &str,
        status: Option<&str>,
    ) -> Result<ServiceWorkerRecord> {
        let worker = self
            .service_get_worker(worker_id)?
            .with_context(|| format!("service worker does not exist: {worker_id}"))?;
        let status = normalized_worker_status(status.unwrap_or(&worker.status))?;
        let now = service_now();
        let updated = self.conn.execute(
            "UPDATE service_workers
             SET status = ?1, last_heartbeat_at = ?2, updated_at = ?2
             WHERE id = ?3",
            params![status, now, worker_id],
        )?;
        ensure_service_updated(updated, "worker", worker_id)?;
        self.service_record_event(
            "worker.heartbeat",
            Some(worker_id),
            worker.current_task_id.as_deref(),
            &serde_json::json!({"status": status}),
        )?;
        self.service_get_worker(worker_id)?
            .with_context(|| format!("service worker disappeared: {worker_id}"))
    }

    pub fn service_mark_worker_ready(&self, worker_id: &str) -> Result<ServiceWorkerRecord> {
        self.service_set_worker_status(worker_id, WORKER_STATE_READY, "worker.ready")
    }

    pub fn service_mark_worker_offline(&self, worker_id: &str) -> Result<ServiceWorkerRecord> {
        let worker = self
            .service_get_worker(worker_id)?
            .with_context(|| format!("service worker does not exist: {worker_id}"))?;
        let now = service_now();
        self.conn.execute(
            "UPDATE service_workers
	             SET status = ?1, current_task_id = NULL, updated_at = ?2
	             WHERE id = ?3",
            params![WORKER_STATE_OFFLINE, now, worker_id],
        )?;
        if let Some(task_id) = worker.current_task_id.as_deref() {
            let task = self.require_service_task(task_id)?;
            if task.attempt >= task.max_attempts {
                self.conn.execute(
                    "UPDATE service_tasks
	                     SET state = ?1,
	                         failure_reason = 'worker offline exceeded max attempts',
	                         last_error = 'worker offline exceeded max attempts',
	                         updated_at = ?2
	                     WHERE id = ?3 AND state IN ('assigned', 'acked', 'working')",
                    params![TASK_STATE_FAILED, now, task_id],
                )?;
            } else {
                self.conn.execute(
                    "UPDATE service_tasks
	                     SET state = ?1,
	                         assigned_worker_id = NULL,
	                         retry_count = retry_count + 1,
	                         attempt = attempt + 1,
	                         failure_reason = 'worker offline',
	                         retry_reason = 'worker offline',
	                         last_error = 'worker offline',
	                         updated_at = ?2
	                     WHERE id = ?3 AND state IN ('assigned', 'acked', 'working')",
                    params![TASK_STATE_QUEUED, now, task_id],
                )?;
            }
            self.service_record_event(
                "task.retry",
                Some(worker_id),
                Some(task_id),
                &serde_json::json!({"reason": "worker offline"}),
            )?;
        }
        self.service_record_event(
            "worker.offline",
            Some(worker_id),
            worker.current_task_id.as_deref(),
            &serde_json::json!({}),
        )?;
        self.service_get_worker(worker_id)?
            .with_context(|| format!("service worker disappeared: {worker_id}"))
    }

    fn service_set_worker_status(
        &self,
        worker_id: &str,
        status: &str,
        event_type: &str,
    ) -> Result<ServiceWorkerRecord> {
        let worker = self
            .service_get_worker(worker_id)?
            .with_context(|| format!("service worker does not exist: {worker_id}"))?;
        let status = normalized_worker_status(status)?;
        let now = service_now();
        let updated = self.conn.execute(
            "UPDATE service_workers
	             SET status = ?1, last_heartbeat_at = ?2, updated_at = ?2
	             WHERE id = ?3",
            params![status, now, worker_id],
        )?;
        ensure_service_updated(updated, "worker", worker_id)?;
        self.service_record_event(
            event_type,
            Some(worker_id),
            worker.current_task_id.as_deref(),
            &serde_json::json!({"status": status}),
        )?;
        self.service_get_worker(worker_id)?
            .with_context(|| format!("service worker disappeared: {worker_id}"))
    }

    pub fn service_list_workers(&self) -> Result<Vec<ServiceWorkerRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, role, status, capacity, current_task_id, last_heartbeat_at,
                    metadata, created_at, updated_at
             FROM service_workers
             ORDER BY id",
        )?;
        let records = stmt
            .query_map([], map_service_worker_row)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(anyhow::Error::from)?;
        Ok(records)
    }

    pub fn service_get_worker(&self, worker_id: &str) -> Result<Option<ServiceWorkerRecord>> {
        self.conn
            .query_row(
                "SELECT id, kind, role, status, capacity, current_task_id, last_heartbeat_at,
                        metadata, created_at, updated_at
                 FROM service_workers
                 WHERE id = ?1",
                [worker_id],
                map_service_worker_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn service_create_task(&self, input: ServiceTaskInput) -> Result<ServiceTaskRecord> {
        let title = required_service_field("task title", &input.title)?;
        let description = required_service_field("task description", &input.description)?;
        let source_prd_path = required_service_field("source PRD path", &input.source_prd_path)?;
        let preferred_model = normalized_worker_kind(&input.preferred_model)?;
        let role = required_service_field("task role", &input.role)?;
        if input.acceptance_criteria.is_empty() {
            anyhow::bail!("task acceptance criteria cannot be empty");
        }
        let max_attempts = input
            .max_attempts
            .unwrap_or(DEFAULT_SERVICE_TASK_MAX_ATTEMPTS);
        if max_attempts < 1 {
            anyhow::bail!("task max attempts must be positive");
        }
        let acceptance_criteria = input
            .acceptance_criteria
            .into_iter()
            .map(|criterion| required_service_field("acceptance criterion", &criterion))
            .collect::<Result<Vec<_>>>()?;
        let dependencies = input
            .dependencies
            .unwrap_or_default()
            .into_iter()
            .map(|dependency| required_service_field("task dependency", &dependency))
            .collect::<Result<Vec<_>>>()?;
        let scheduling =
            service_task_scheduling_metadata(&preferred_model, &description, &acceptance_criteria);
        let eligible_providers = normalize_service_provider_lane(Some(&scheduling.pool));
        let id = uuid::Uuid::new_v4().to_string();
        let now = service_now();
        self.conn.execute(
            "INSERT INTO service_tasks (
	                id, title, description, acceptance_criteria, source_prd_path,
		                source_task_number, state, priority, preferred_model, role, parallelizable,
			                dependencies, scheduling_pool, estimated_size, claude_suitable,
			                assigned_worker_id, eligible_providers, lease_owner, lease_expires_at,
			                claimed_at, completed_worker_id, completed_worker_kind, claim_duration_secs,
			                retry_count, failure_reason, attempt, max_attempts,
			                acked_at, started_at, reported_at, verified_at, completed_at, last_error,
			                retry_reason, report_hash, created_at, updated_at
			             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'queued', ?7, ?8, ?9, ?10,
			                       ?11, ?12, ?13, ?14, NULL, ?15, NULL, NULL,
			                       NULL, NULL, NULL, NULL, 0, NULL, 0, ?16,
			                       NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, ?17, ?17)",
            params![
                id,
                title,
                description,
                serde_json::to_string(&acceptance_criteria)?,
                source_prd_path,
                input.source_task_number.as_deref(),
                input.priority,
                preferred_model,
                role,
                bool_to_i64(input.parallelizable),
                serde_json::to_string(&dependencies)?,
                scheduling.pool,
                scheduling.size,
                bool_to_i64(scheduling.claude_suitable),
                eligible_providers,
                max_attempts,
                now
            ],
        )?;
        self.service_record_event(
            "task.created",
            None,
            Some(&id),
            &serde_json::json!({"title": title, "preferred_model": preferred_model}),
        )?;
        self.service_get_task(&id)?
            .with_context(|| format!("created service task disappeared: {id}"))
    }

    pub fn service_list_tasks(
        &self,
        state: Option<&str>,
        worker_id: Option<&str>,
    ) -> Result<Vec<ServiceTaskRecord>> {
        let mut sql = format!("{} FROM service_tasks", service_task_select_clause());
        let mut filters = Vec::new();
        let mut params_vec = Vec::new();
        if let Some(state) = state {
            filters.push("state = ?");
            params_vec.push(state.to_string());
        }
        if let Some(worker_id) = worker_id {
            filters.push("assigned_worker_id = ?");
            params_vec.push(worker_id.to_string());
        }
        if !filters.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&filters.join(" AND "));
        }
        sql.push_str(" ORDER BY priority DESC, created_at, id");
        let mut stmt = self.conn.prepare(&sql)?;
        let records = stmt
            .query_map(params_from_iter(params_vec), map_service_task_row)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(anyhow::Error::from)?;
        Ok(records)
    }

    pub fn service_get_task(&self, task_id: &str) -> Result<Option<ServiceTaskRecord>> {
        self.conn
            .query_row(
                &format!(
                    "{} FROM service_tasks WHERE id = ?1",
                    service_task_select_clause()
                ),
                [task_id],
                map_service_task_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn service_claim_next_task(
        &self,
        worker_id: &str,
        lease_secs: i64,
        lane_escape_secs: i64,
    ) -> Result<Option<ServiceTaskRecord>> {
        if lease_secs < 1 {
            anyhow::bail!("lease_secs must be positive");
        }
        let worker = self
            .service_get_worker(worker_id)?
            .with_context(|| format!("service worker does not exist: {worker_id}"))?;
        if !matches!(
            worker.status.as_str(),
            WORKER_STATE_READY | WORKER_STATE_ASSIGNED
        ) {
            anyhow::bail!("service worker {worker_id} is not ready to claim tasks");
        }

        for _ in 0..8 {
            let mut candidates = self.service_claim_candidates(worker_id)?;
            candidates.sort_by(|a, b| {
                service_direct_claim_rank(&worker, a)
                    .cmp(&service_direct_claim_rank(&worker, b))
                    .then_with(|| b.priority.cmp(&a.priority))
                    .then_with(|| {
                        service_lane_claim_rank(&worker, a, lane_escape_secs)
                            .cmp(&service_lane_claim_rank(&worker, b, lane_escape_secs))
                    })
                    .then_with(|| a.created_at.cmp(&b.created_at))
                    .then_with(|| a.id.cmp(&b.id))
            });

            let Some(task) = candidates
                .into_iter()
                .filter(|task| {
                    service_worker_can_claim_task(&worker, task, lane_escape_secs)
                        && self.ensure_service_task_eligible(task).is_ok()
                })
                .next()
            else {
                return Ok(None);
            };

            let now = service_now();
            let lease_expires_at = service_time_after_secs(lease_secs);
            let update_result = if task.state == TASK_STATE_ASSIGNED {
                self.conn.execute(
                    "UPDATE service_tasks
                     SET state = ?1,
                         lease_owner = ?2,
                         lease_expires_at = ?3,
                         claimed_at = COALESCE(claimed_at, ?4),
                         acked_at = COALESCE(acked_at, ?4),
                         updated_at = ?4
                     WHERE id = ?5 AND state = ?6 AND assigned_worker_id = ?2",
                    params![
                        TASK_STATE_ACKED,
                        worker.id,
                        lease_expires_at,
                        now,
                        task.id,
                        TASK_STATE_ASSIGNED
                    ],
                )?
            } else {
                self.conn.execute(
                    "UPDATE service_tasks
                     SET state = ?1,
                         assigned_worker_id = ?2,
                         lease_owner = ?2,
                         lease_expires_at = ?3,
                         claimed_at = ?4,
                         acked_at = ?4,
                         updated_at = ?4
                     WHERE id = ?5 AND state = ?6 AND assigned_worker_id IS NULL",
                    params![
                        TASK_STATE_ACKED,
                        worker.id,
                        lease_expires_at,
                        now,
                        task.id,
                        TASK_STATE_QUEUED
                    ],
                )?
            };

            if update_result == 1 {
                self.conn.execute(
                    "UPDATE service_workers
                     SET status = ?1, current_task_id = ?2, updated_at = ?3
                     WHERE id = ?4",
                    params![WORKER_STATE_WORKING, task.id, now, worker.id],
                )?;
                self.service_record_event(
                    "task.claimed",
                    Some(&worker.id),
                    Some(&task.id),
                    &serde_json::json!({
                        "lease_expires_at": lease_expires_at,
                        "eligible_providers": task.eligible_providers,
                    }),
                )?;
                return self.service_get_task(&task.id);
            }
        }
        Ok(None)
    }

    fn service_claim_candidates(&self, worker_id: &str) -> Result<Vec<ServiceTaskRecord>> {
        let mut stmt = self.conn.prepare(&format!(
            "{} FROM service_tasks
             WHERE (state = ?1 AND assigned_worker_id = ?2)
                OR (state = ?3 AND assigned_worker_id IS NULL)
             ORDER BY priority DESC, created_at, id",
            service_task_select_clause()
        ))?;
        let records = stmt
            .query_map(
                params![TASK_STATE_ASSIGNED, worker_id, TASK_STATE_QUEUED],
                map_service_task_row,
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(anyhow::Error::from)?;
        Ok(records)
    }

    pub fn service_touch_task(
        &self,
        worker_id: &str,
        task_id: &str,
        lease_secs: i64,
    ) -> Result<ServiceTaskRecord> {
        if lease_secs < 1 {
            anyhow::bail!("lease_secs must be positive");
        }
        let _worker = self
            .service_get_worker(worker_id)?
            .with_context(|| format!("service worker does not exist: {worker_id}"))?;
        let now = service_now();
        let lease_expires_at = service_time_after_secs(lease_secs);
        let updated = self.conn.execute(
            "UPDATE service_tasks
             SET lease_expires_at = ?1, updated_at = ?2
             WHERE id = ?3 AND lease_owner = ?4 AND state IN ('acked', 'working')",
            params![lease_expires_at, now, task_id, worker_id],
        )?;
        if updated != 1 {
            anyhow::bail!("service task {task_id} is not leased by {worker_id}");
        }
        self.conn.execute(
            "UPDATE service_workers SET last_heartbeat_at = ?1, updated_at = ?1 WHERE id = ?2",
            params![now, worker_id],
        )?;
        self.service_record_event(
            "task.lease_touched",
            Some(worker_id),
            Some(task_id),
            &serde_json::json!({"lease_expires_at": lease_expires_at}),
        )?;
        self.require_service_task(task_id)
    }

    pub fn service_reap_queue(&self, steal_after_secs: i64) -> Result<ServiceQueueReapResult> {
        if steal_after_secs < 1 {
            anyhow::bail!("steal_after_secs must be positive");
        }
        let now = chrono::Utc::now();
        let timestamp = service_now();
        let mut lease_expired = Vec::new();
        let mut assignment_released = Vec::new();
        let mut failed = Vec::new();

        for task in self.service_list_tasks(None, None)? {
            if matches!(task.state.as_str(), TASK_STATE_ACKED | TASK_STATE_WORKING) {
                let expired = task
                    .lease_expires_at
                    .as_deref()
                    .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                    .map(|expires| expires.with_timezone(&chrono::Utc) <= now)
                    .unwrap_or(false);
                if !expired {
                    continue;
                }
                let exhausted = task.attempt + 1 >= task.max_attempts;
                if exhausted {
                    self.conn.execute(
                        "UPDATE service_tasks
                         SET state = ?1,
                             failure_reason = 'lease expired and max attempts exhausted',
                             last_error = 'lease expired and max attempts exhausted',
                             lease_owner = NULL,
                             lease_expires_at = NULL,
                             updated_at = ?2
                         WHERE id = ?3",
                        params![TASK_STATE_FAILED, timestamp, task.id],
                    )?;
                    failed.push(self.require_service_task(&task.id)?);
                } else {
                    self.conn.execute(
                        "UPDATE service_tasks
                         SET state = ?1,
                             assigned_worker_id = NULL,
                             lease_owner = NULL,
                             lease_expires_at = NULL,
                             retry_count = retry_count + 1,
                             attempt = attempt + 1,
                             retry_reason = 'lease expired',
                             last_error = 'lease expired',
                             updated_at = ?2
                         WHERE id = ?3",
                        params![TASK_STATE_QUEUED, timestamp, task.id],
                    )?;
                    lease_expired.push(self.require_service_task(&task.id)?);
                }
                if let Some(worker_id) = task.assigned_worker_id.as_deref() {
                    self.conn.execute(
                        "UPDATE service_workers
                         SET status = ?1, current_task_id = NULL, updated_at = ?2
                         WHERE id = ?3",
                        params![WORKER_STATE_READY, timestamp, worker_id],
                    )?;
                }
                self.service_record_event(
                    "task.lease_expired",
                    task.assigned_worker_id.as_deref(),
                    Some(&task.id),
                    &serde_json::json!({"failed": exhausted}),
                )?;
                continue;
            }

            if task.state == TASK_STATE_ASSIGNED {
                let updated_at = chrono::DateTime::parse_from_rfc3339(&task.updated_at)
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .unwrap_or(now);
                if now.signed_duration_since(updated_at).num_seconds() < steal_after_secs {
                    continue;
                }
                let previous_worker = task.assigned_worker_id.clone();
                self.conn.execute(
                    "UPDATE service_tasks
                     SET state = ?1,
                         assigned_worker_id = NULL,
                         retry_reason = 'direct assignment released',
                         last_error = 'direct assignment released',
                         updated_at = ?2
                     WHERE id = ?3 AND state = ?4",
                    params![TASK_STATE_QUEUED, timestamp, task.id, TASK_STATE_ASSIGNED],
                )?;
                if let Some(worker_id) = previous_worker.as_deref() {
                    self.conn.execute(
                        "UPDATE service_workers
                         SET status = ?1, current_task_id = NULL, updated_at = ?2
                         WHERE id = ?3",
                        params![WORKER_STATE_READY, timestamp, worker_id],
                    )?;
                }
                self.service_record_event(
                    "task.assignment_released",
                    previous_worker.as_deref(),
                    Some(&task.id),
                    &serde_json::json!({"steal_after_secs": steal_after_secs}),
                )?;
                assignment_released.push(self.require_service_task(&task.id)?);
            }
        }

        Ok(ServiceQueueReapResult {
            lease_expired,
            assignment_released,
            failed,
        })
    }

    pub fn service_queue_stats(&self, window_secs: i64) -> Result<ServiceQueueStats> {
        if window_secs < 1 {
            anyhow::bail!("window_secs must be positive");
        }
        let now = chrono::Utc::now();
        let mut lanes = BTreeMap::<String, (i64, Option<i64>)>::new();
        let mut directly_assigned_queued = 0;
        let mut total_queued = 0;
        let mut total_in_flight = 0;

        for task in self.service_list_tasks(None, None)? {
            if task.state == TASK_STATE_QUEUED {
                total_queued += 1;
                if task.assigned_worker_id.is_some() {
                    directly_assigned_queued += 1;
                } else {
                    let lane = task
                        .eligible_providers
                        .clone()
                        .unwrap_or_else(|| "any".to_string());
                    let age = chrono::DateTime::parse_from_rfc3339(&task.created_at)
                        .ok()
                        .map(|created| {
                            now.signed_duration_since(created.with_timezone(&chrono::Utc))
                                .num_seconds()
                                .max(0)
                        });
                    let entry = lanes.entry(lane).or_insert((0, None));
                    entry.0 += 1;
                    entry.1 = match (entry.1, age) {
                        (Some(current), Some(candidate)) => Some(current.max(candidate)),
                        (None, Some(candidate)) => Some(candidate),
                        (current, None) => current,
                    };
                }
            }
            if matches!(task.state.as_str(), TASK_STATE_ACKED | TASK_STATE_WORKING) {
                total_in_flight += 1;
            }
        }

        let lanes = lanes
            .into_iter()
            .map(|(lane, (queued, oldest_age_secs))| ServiceQueueLaneStat {
                lane,
                queued,
                oldest_age_secs,
            })
            .collect::<Vec<_>>();

        let since = now
            .checked_sub_signed(chrono::Duration::seconds(window_secs))
            .unwrap_or(now);
        let mut providers = BTreeMap::<String, (i64, i64, i64)>::new();
        for task in self.service_list_tasks(None, None)? {
            if matches!(task.state.as_str(), TASK_STATE_ACKED | TASK_STATE_WORKING) {
                let kind = task
                    .lease_owner
                    .as_deref()
                    .and_then(|worker_id| self.service_get_worker(worker_id).ok().flatten())
                    .map(|worker| worker.kind)
                    .unwrap_or_else(|| "unknown".to_string());
                providers.entry(kind).or_insert((0, 0, 0)).0 += 1;
            }
            if task.state == TASK_STATE_COMPLETE {
                let completed_at = task
                    .completed_at
                    .as_deref()
                    .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                    .map(|dt| dt.with_timezone(&chrono::Utc));
                if completed_at.is_some_and(|completed| completed >= since) {
                    let kind = task
                        .completed_worker_kind
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string());
                    let entry = providers.entry(kind).or_insert((0, 0, 0));
                    entry.1 += 1;
                    entry.2 += task.claim_duration_secs.unwrap_or(0);
                }
            }
        }
        let providers = providers
            .into_iter()
            .map(
                |(worker_kind, (in_flight, completed_window, duration_sum))| {
                    ServiceQueueProviderStat {
                        worker_kind,
                        in_flight,
                        completed_window,
                        avg_claim_duration_secs: if completed_window > 0 {
                            Some(duration_sum as f64 / completed_window as f64)
                        } else {
                            None
                        },
                    }
                },
            )
            .collect::<Vec<_>>();
        let pool_sizing_hint = service_pool_sizing_hint(&providers);

        Ok(ServiceQueueStats {
            lanes,
            directly_assigned_queued,
            total_queued,
            total_in_flight,
            providers,
            pool_sizing_hint,
            window_secs,
        })
    }

    pub fn service_assign_task(
        &self,
        task_id: &str,
        worker_id: Option<&str>,
    ) -> Result<ServiceTaskRecord> {
        let task = self.require_service_task(task_id)?;
        if task.state != TASK_STATE_QUEUED {
            anyhow::bail!(
                "service task {task_id} cannot be assigned from state {}",
                task.state
            );
        }
        self.ensure_service_task_eligible(&task)?;
        let worker = match worker_id {
            Some(worker_id) => self
                .service_get_worker(worker_id)?
                .with_context(|| format!("service worker does not exist: {worker_id}"))?,
            None => self
                .service_list_workers()?
                .into_iter()
                .filter(|worker| service_worker_can_take_task(worker, &task))
                .next()
                .with_context(|| format!("no ready service worker for task {task_id}"))?,
        };
        if !service_worker_can_take_task(&worker, &task) {
            anyhow::bail!(
                "service worker {} cannot take task {} in pool {}",
                worker.id,
                task.id,
                task.scheduling_pool
            );
        }
        let now = service_now();
        let updated = self.conn.execute(
            "UPDATE service_tasks
	             SET state = ?1, assigned_worker_id = ?2, updated_at = ?3
	             WHERE id = ?4 AND state = ?5",
            params![
                TASK_STATE_ASSIGNED,
                worker.id,
                now,
                task_id,
                TASK_STATE_QUEUED
            ],
        )?;
        ensure_service_updated(updated, "task", task_id)?;
        self.conn.execute(
            "UPDATE service_workers
	             SET status = ?1, current_task_id = ?2, updated_at = ?3
	             WHERE id = ?4",
            params![WORKER_STATE_ASSIGNED, task_id, now, worker.id],
        )?;
        self.service_record_event(
            "task.assigned",
            Some(&worker.id),
            Some(task_id),
            &serde_json::json!({"worker_id": worker.id}),
        )?;
        self.require_service_task(task_id)
    }

    pub fn service_ack_task(&self, task_id: &str) -> Result<ServiceTaskRecord> {
        let task = self.require_service_task(task_id)?;
        if task.state != TASK_STATE_ASSIGNED {
            anyhow::bail!(
                "service task {task_id} cannot be acked from state {}",
                task.state
            );
        }
        let worker_id = task
            .assigned_worker_id
            .as_deref()
            .context("assigned task has no worker")?;
        let now = service_now();
        let updated = self.conn.execute(
            "UPDATE service_tasks
	             SET state = ?1, acked_at = ?2, updated_at = ?2
	             WHERE id = ?3 AND state = ?4",
            params![TASK_STATE_ACKED, now, task_id, TASK_STATE_ASSIGNED],
        )?;
        ensure_service_updated(updated, "task", task_id)?;
        self.conn.execute(
            "UPDATE service_workers SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![WORKER_STATE_WORKING, now, worker_id],
        )?;
        self.service_record_event(
            "task.acked",
            Some(worker_id),
            Some(task_id),
            &serde_json::json!({}),
        )?;
        self.require_service_task(task_id)
    }

    pub fn service_start_task(&self, task_id: &str) -> Result<ServiceTaskRecord> {
        let task = self.require_service_task(task_id)?;
        if task.state != TASK_STATE_ACKED {
            anyhow::bail!(
                "service task {task_id} cannot be started from state {}",
                task.state
            );
        }
        let worker_id = task
            .assigned_worker_id
            .as_deref()
            .context("acked task has no worker")?;
        let now = service_now();
        let updated = self.conn.execute(
            "UPDATE service_tasks
	             SET state = ?1, started_at = ?2, updated_at = ?2
	             WHERE id = ?3 AND state = ?4",
            params![TASK_STATE_WORKING, now, task_id, TASK_STATE_ACKED],
        )?;
        ensure_service_updated(updated, "task", task_id)?;
        self.conn.execute(
            "UPDATE service_workers SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![WORKER_STATE_WORKING, now, worker_id],
        )?;
        self.service_record_event(
            "task.working",
            Some(worker_id),
            Some(task_id),
            &serde_json::json!({}),
        )?;
        self.require_service_task(task_id)
    }

    pub fn service_progress_task(
        &self,
        task_id: &str,
        input: ServiceTaskProgressInput,
    ) -> Result<ServiceTaskRecord> {
        let task = self.require_service_task(task_id)?;
        if !matches!(task.state.as_str(), TASK_STATE_ACKED | TASK_STATE_WORKING) {
            anyhow::bail!(
                "service task {task_id} cannot record progress from state {}",
                task.state
            );
        }
        let summary =
            redact_report_text(&required_service_field("progress summary", &input.summary)?);
        self.service_record_event(
            "task.progress",
            task.assigned_worker_id.as_deref(),
            Some(task_id),
            &serde_json::json!({"summary": summary}),
        )?;
        Ok(task)
    }

    pub fn service_report_task(
        &self,
        task_id: &str,
        input: ServiceTaskReportInput,
    ) -> Result<ServiceTaskRecord> {
        let task = self.require_service_task(task_id)?;
        if !matches!(task.state.as_str(), TASK_STATE_ACKED | TASK_STATE_WORKING) {
            anyhow::bail!(
                "service task {task_id} cannot be reported from state {}",
                task.state
            );
        }
        let report = ServiceTaskReportInput {
            summary: redact_report_text(&required_service_field("report summary", &input.summary)?),
            files_inspected: redact_report_list(input.files_inspected),
            changed_files: redact_report_list(input.changed_files),
            tests_run: redact_report_list(input.tests_run),
            verification: redact_report_text(&required_service_field(
                "report verification",
                &input.verification,
            )?),
            risks: redact_report_text(&input.risks),
            raw_report: redact_report_text(&input.raw_report),
        };
        let report_hash = service_report_hash(&report)?;
        let now = service_now();
        self.conn.execute(
            "INSERT INTO service_task_reports (
                task_id, summary, files_inspected, changed_files, tests_run,
                verification, risks, raw_report, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(task_id) DO UPDATE SET
                summary = excluded.summary,
                files_inspected = excluded.files_inspected,
                changed_files = excluded.changed_files,
                tests_run = excluded.tests_run,
                verification = excluded.verification,
                risks = excluded.risks,
                raw_report = excluded.raw_report,
                created_at = excluded.created_at",
            params![
                task_id,
                report.summary,
                serde_json::to_string(&report.files_inspected)?,
                serde_json::to_string(&report.changed_files)?,
                serde_json::to_string(&report.tests_run)?,
                report.verification,
                report.risks,
                report.raw_report,
                now
            ],
        )?;
        let updated = self.conn.execute(
            "UPDATE service_tasks
	             SET state = ?1, reported_at = ?2, report_hash = ?3, updated_at = ?2
	             WHERE id = ?4",
            params![TASK_STATE_REPORTED, now, report_hash, task_id],
        )?;
        ensure_service_updated(updated, "task", task_id)?;
        self.service_record_event(
            "task.reported",
            task.assigned_worker_id.as_deref(),
            Some(task_id),
            &serde_json::json!({"report_hash": report_hash}),
        )?;
        self.require_service_task(task_id)
    }

    pub fn service_verify_task(
        &self,
        task_id: &str,
        input: ServiceTaskProgressInput,
    ) -> Result<ServiceTaskRecord> {
        let task = self.require_service_task(task_id)?;
        if task.state != TASK_STATE_REPORTED {
            anyhow::bail!(
                "service task {task_id} cannot be verified from state {}",
                task.state
            );
        }
        let summary = redact_report_text(&required_service_field(
            "verification summary",
            &input.summary,
        )?);
        let now = service_now();
        let updated = self.conn.execute(
            "UPDATE service_tasks
	             SET state = ?1, verified_at = ?2, updated_at = ?2
	             WHERE id = ?3 AND state = ?4",
            params![TASK_STATE_VERIFIED, now, task_id, TASK_STATE_REPORTED],
        )?;
        ensure_service_updated(updated, "task", task_id)?;
        self.service_record_event(
            "task.verified",
            task.assigned_worker_id.as_deref(),
            Some(task_id),
            &serde_json::json!({"summary": summary}),
        )?;
        self.require_service_task(task_id)
    }

    pub fn service_complete_task(&self, task_id: &str) -> Result<ServiceTaskRecord> {
        let task = self.require_service_task(task_id)?;
        if !matches!(
            task.state.as_str(),
            TASK_STATE_REPORTED | TASK_STATE_VERIFIED
        ) {
            anyhow::bail!(
                "service task {task_id} cannot be completed from state {}",
                task.state
            );
        }
        let report = self
            .service_get_task_report(task_id)?
            .with_context(|| format!("service task {task_id} has no report"))?;
        if report.verification.trim().is_empty() {
            anyhow::bail!("service task {task_id} cannot complete without verification evidence");
        }
        let now = service_now();
        let claim_duration_secs = task
            .claimed_at
            .as_deref()
            .and_then(service_elapsed_secs_since);
        let completed_worker_kind = task
            .assigned_worker_id
            .as_deref()
            .and_then(|worker_id| self.service_get_worker(worker_id).ok().flatten())
            .map(|worker| worker.kind);
        let updated = self.conn.execute(
            "UPDATE service_tasks
		             SET state = ?1,
                         completed_at = ?2,
                         completed_worker_id = ?3,
                         completed_worker_kind = ?4,
                         claim_duration_secs = ?5,
                         lease_owner = NULL,
                         lease_expires_at = NULL,
                         updated_at = ?2
		             WHERE id = ?6 AND state IN ('reported', 'verified')",
            params![
                TASK_STATE_COMPLETE,
                now,
                task.assigned_worker_id.as_deref(),
                completed_worker_kind.as_deref(),
                claim_duration_secs,
                task_id
            ],
        )?;
        ensure_service_updated(updated, "task", task_id)?;
        if let Some(worker_id) = task.assigned_worker_id.as_deref() {
            self.conn.execute(
                "UPDATE service_workers
	                 SET status = ?1, current_task_id = NULL, updated_at = ?2
	                 WHERE id = ?3",
                params![WORKER_STATE_READY, now, worker_id],
            )?;
        }
        self.service_record_event(
            "task.completed",
            task.assigned_worker_id.as_deref(),
            Some(task_id),
            &serde_json::json!({}),
        )?;
        self.require_service_task(task_id)
    }

    pub fn service_fail_task(
        &self,
        task_id: &str,
        blocked: bool,
        reason: &str,
    ) -> Result<ServiceTaskRecord> {
        let task = self.require_service_task(task_id)?;
        if matches!(
            task.state.as_str(),
            TASK_STATE_COMPLETE | TASK_STATE_FAILED | TASK_STATE_BLOCKED
        ) {
            anyhow::bail!(
                "service task {task_id} cannot fail from state {}",
                task.state
            );
        }
        let reason = required_service_field("failure reason", reason)?;
        let new_state = if blocked {
            TASK_STATE_BLOCKED
        } else {
            TASK_STATE_FAILED
        };
        let now = service_now();
        let updated = self.conn.execute(
            "UPDATE service_tasks
	             SET state = ?1,
	                 failure_reason = ?2,
	                 retry_count = retry_count + 1,
	                 attempt = attempt + 1,
	                 last_error = ?2,
	                 updated_at = ?3
	             WHERE id = ?4",
            params![new_state, reason, now, task_id],
        )?;
        ensure_service_updated(updated, "task", task_id)?;
        if let Some(worker_id) = task.assigned_worker_id.as_deref() {
            self.conn.execute(
                "UPDATE service_workers
	                 SET status = ?1, current_task_id = NULL, updated_at = ?2
	                 WHERE id = ?3",
                params![WORKER_STATE_READY, now, worker_id],
            )?;
        }
        self.service_record_event(
            if blocked {
                "task.blocked"
            } else {
                "task.failed"
            },
            task.assigned_worker_id.as_deref(),
            Some(task_id),
            &serde_json::json!({"reason": reason}),
        )?;
        self.require_service_task(task_id)
    }

    pub fn service_retry_task(&self, task_id: &str, reason: &str) -> Result<ServiceTaskRecord> {
        let task = self.require_service_task(task_id)?;
        if task.state == TASK_STATE_COMPLETE {
            anyhow::bail!("service task {task_id} cannot retry from state complete");
        }
        if task.attempt >= task.max_attempts {
            anyhow::bail!(
                "service task {task_id} exceeded max attempts {}",
                task.max_attempts
            );
        }
        let reason = redact_report_text(&required_service_field("retry reason", reason)?);
        let now = service_now();
        let updated = self.conn.execute(
            "UPDATE service_tasks
	             SET state = ?1,
	                 assigned_worker_id = NULL,
	                 retry_count = retry_count + 1,
	                 attempt = attempt + 1,
	                 retry_reason = ?2,
	                 last_error = ?2,
	                 updated_at = ?3
	             WHERE id = ?4",
            params![TASK_STATE_QUEUED, reason, now, task_id],
        )?;
        ensure_service_updated(updated, "task", task_id)?;
        if let Some(worker_id) = task.assigned_worker_id.as_deref() {
            self.conn.execute(
                "UPDATE service_workers
	                 SET status = ?1, current_task_id = NULL, updated_at = ?2
	                 WHERE id = ?3",
                params![WORKER_STATE_READY, now, worker_id],
            )?;
        }
        self.service_record_event(
            "task.retry",
            task.assigned_worker_id.as_deref(),
            Some(task_id),
            &serde_json::json!({"reason": reason}),
        )?;
        self.require_service_task(task_id)
    }

    pub fn service_requeue_stale_tasks(
        &self,
        stale_after_secs: i64,
    ) -> Result<ServiceStaleRequeueResult> {
        if stale_after_secs < 1 {
            anyhow::bail!("stale_after_secs must be positive");
        }
        let now = chrono::Utc::now();
        let mut requeued = Vec::new();
        for task in self.service_list_tasks(None, None)? {
            if !matches!(task.state.as_str(), TASK_STATE_ASSIGNED | TASK_STATE_ACKED) {
                continue;
            }
            let updated_at = chrono::DateTime::parse_from_rfc3339(&task.updated_at)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or(now);
            if now.signed_duration_since(updated_at).num_seconds() < stale_after_secs {
                continue;
            }
            let timestamp = service_now();
            if task.attempt >= task.max_attempts {
                self.conn.execute(
                    "UPDATE service_tasks
	                     SET state = ?1,
	                         failure_reason = 'stale assignment exceeded max attempts',
	                         last_error = 'stale assignment exceeded max attempts',
	                         updated_at = ?2
	                     WHERE id = ?3",
                    params![TASK_STATE_FAILED, timestamp, task.id],
                )?;
            } else {
                self.conn.execute(
                    "UPDATE service_tasks
	                     SET state = ?1,
	                         assigned_worker_id = NULL,
	                         retry_count = retry_count + 1,
	                         attempt = attempt + 1,
	                         failure_reason = 'stale assignment requeued',
	                         retry_reason = 'stale assignment requeued',
	                         last_error = 'stale assignment requeued',
	                         updated_at = ?2
	                     WHERE id = ?3 AND state IN ('assigned', 'acked')",
                    params![TASK_STATE_QUEUED, timestamp, task.id],
                )?;
            }
            if let Some(worker_id) = task.assigned_worker_id.as_deref() {
                self.conn.execute(
                    "UPDATE service_workers
	                     SET status = ?1, current_task_id = NULL, updated_at = ?2
	                     WHERE id = ?3",
                    params![WORKER_STATE_READY, timestamp, worker_id],
                )?;
            }
            self.service_record_event(
                "task.assignment_timeout",
                task.assigned_worker_id.as_deref(),
                Some(&task.id),
                &serde_json::json!({"stale_after_secs": stale_after_secs}),
            )?;
            requeued.push(self.require_service_task(&task.id)?);
        }
        Ok(ServiceStaleRequeueResult { requeued })
    }

    pub fn service_expire_stale_workers(
        &self,
        stale_after_secs: i64,
    ) -> Result<ServiceStaleWorkerResult> {
        if stale_after_secs < 1 {
            anyhow::bail!("stale_after_secs must be positive");
        }
        let now = chrono::Utc::now();
        let mut offline_workers = Vec::new();
        let mut requeued = Vec::new();
        for worker in self.service_list_workers()? {
            if worker.status == WORKER_STATE_OFFLINE {
                continue;
            }
            let Some(last_heartbeat_at) = worker.last_heartbeat_at.as_deref() else {
                continue;
            };
            let heartbeat_at = chrono::DateTime::parse_from_rfc3339(last_heartbeat_at)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or(now);
            if now.signed_duration_since(heartbeat_at).num_seconds() < stale_after_secs {
                continue;
            }
            let timestamp = service_now();
            self.conn.execute(
                "UPDATE service_workers
	                 SET status = ?1, current_task_id = NULL, updated_at = ?2
	                 WHERE id = ?3",
                params![WORKER_STATE_OFFLINE, timestamp, worker.id],
            )?;
            self.service_record_event(
                "worker.offline",
                Some(&worker.id),
                worker.current_task_id.as_deref(),
                &serde_json::json!({"stale_after_secs": stale_after_secs}),
            )?;
            if let Some(task_id) = worker.current_task_id.as_deref() {
                if let Some(task) = self.service_get_task(task_id)? {
                    if matches!(
                        task.state.as_str(),
                        TASK_STATE_ASSIGNED | TASK_STATE_ACKED | TASK_STATE_WORKING
                    ) {
                        if task.attempt >= task.max_attempts {
                            self.conn.execute(
	                                "UPDATE service_tasks
	                                 SET state = ?1,
	                                     failure_reason = 'stale worker heartbeat exceeded max attempts',
	                                     last_error = 'stale worker heartbeat exceeded max attempts',
	                                     updated_at = ?2
	                                 WHERE id = ?3",
	                                params![TASK_STATE_FAILED, timestamp, task_id],
	                            )?;
                            continue;
                        }
                        self.conn.execute(
                            "UPDATE service_tasks
	                             SET state = ?1,
	                                 assigned_worker_id = NULL,
	                                 retry_count = retry_count + 1,
	                                 attempt = attempt + 1,
	                                 failure_reason = 'stale worker heartbeat',
	                                 retry_reason = 'stale worker heartbeat',
	                                 last_error = 'stale worker heartbeat',
	                                 updated_at = ?2
	                             WHERE id = ?3",
                            params![TASK_STATE_QUEUED, timestamp, task_id],
                        )?;
                        self.service_record_event(
                            "task.retry",
                            Some(&worker.id),
                            Some(task_id),
                            &serde_json::json!({"reason": "stale worker heartbeat"}),
                        )?;
                        requeued.push(self.require_service_task(task_id)?);
                    }
                }
            }
            offline_workers.push(
                self.service_get_worker(&worker.id)?
                    .with_context(|| format!("service worker disappeared: {}", worker.id))?,
            );
        }
        Ok(ServiceStaleWorkerResult {
            offline_workers,
            requeued,
        })
    }

    pub fn service_get_task_report(
        &self,
        task_id: &str,
    ) -> Result<Option<ServiceTaskReportRecord>> {
        self.conn
            .query_row(
                "SELECT task_id, summary, files_inspected, changed_files, tests_run,
                        verification, risks, raw_report, created_at
                 FROM service_task_reports
                 WHERE task_id = ?1",
                [task_id],
                map_service_task_report_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn service_list_events(
        &self,
        task_id: Option<&str>,
        worker_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ServiceEventRecord>> {
        let limit = limit.clamp(1, 500);
        let mut sql =
            "SELECT id, type, worker_id, task_id, payload, created_at FROM service_events"
                .to_string();
        let mut filters = Vec::new();
        let mut params_vec = Vec::new();
        if let Some(task_id) = task_id {
            filters.push("task_id = ?");
            params_vec.push(task_id.to_string());
        }
        if let Some(worker_id) = worker_id {
            filters.push("worker_id = ?");
            params_vec.push(worker_id.to_string());
        }
        if !filters.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&filters.join(" AND "));
        }
        sql.push_str(" ORDER BY created_at DESC, id DESC LIMIT ?");
        params_vec.push(limit.to_string());
        let mut stmt = self.conn.prepare(&sql)?;
        let records = stmt
            .query_map(params_from_iter(params_vec), map_service_event_row)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(anyhow::Error::from)?;
        Ok(records)
    }

    pub fn service_import_prd(&self, path: &Path) -> Result<ServicePrdRecord> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read PRD file: {}", path.display()))?;
        let title = content
            .lines()
            .find_map(|line| line.trim().strip_prefix("# "))
            .unwrap_or("Imported PRD")
            .trim()
            .to_string();
        let canonical_path = path.to_string_lossy().to_string();
        let prd_id = uuid::Uuid::new_v4().to_string();
        let now = service_now();
        self.conn.execute(
            "INSERT INTO service_prds (id, path, title, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(path) DO UPDATE SET title = excluded.title, updated_at = excluded.updated_at",
            params![prd_id, canonical_path, title, now],
        )?;
        let prd = self
            .service_get_prd_by_path(&canonical_path)?
            .with_context(|| format!("service PRD import failed: {canonical_path}"))?;
        for parsed in parse_prd_checkbox_tasks(&canonical_path, &content) {
            let _ = self.service_create_task(parsed)?;
        }
        self.service_record_event(
            "prd.imported",
            None,
            None,
            &serde_json::json!({"path": canonical_path}),
        )?;
        Ok(prd)
    }

    pub fn service_list_prds(&self) -> Result<Vec<ServicePrdRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, title, created_at, updated_at
             FROM service_prds
             ORDER BY updated_at DESC, path",
        )?;
        let records = stmt
            .query_map([], map_service_prd_row)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(anyhow::Error::from)?;
        Ok(records)
    }

    pub fn service_get_prd_by_path(&self, path: &str) -> Result<Option<ServicePrdRecord>> {
        self.conn
            .query_row(
                "SELECT id, path, title, created_at, updated_at
                 FROM service_prds
                 WHERE path = ?1",
                [path],
                map_service_prd_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn service_prd_tasks(&self, prd_path: &str) -> Result<Vec<ServiceTaskRecord>> {
        self.service_list_tasks(None, None).map(|tasks| {
            tasks
                .into_iter()
                .filter(|task| task.source_prd_path == prd_path)
                .collect()
        })
    }

    pub fn service_sync_prd(&self, path: &Path) -> Result<usize> {
        let path_text = path.to_string_lossy().to_string();
        let tasks = self.service_prd_tasks(&path_text)?;
        let completed_numbers = tasks
            .into_iter()
            .filter(|task| task.state == "complete")
            .filter_map(|task| task.source_task_number)
            .collect::<std::collections::BTreeSet<_>>();
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read PRD file: {}", path.display()))?;
        let mut changed = 0usize;
        let updated = content
            .lines()
            .map(|line| {
                if let Some(number) = checkbox_task_number(line) {
                    if completed_numbers.contains(&number) && line.contains("[ ]") {
                        changed += 1;
                        return line.replacen("[ ]", "[x]", 1);
                    }
                }
                line.to_string()
            })
            .collect::<Vec<_>>()
            .join("\n");
        if changed > 0 {
            fs::write(path, format!("{updated}\n"))
                .with_context(|| format!("failed to write PRD file: {}", path.display()))?;
        }
        Ok(changed)
    }

    fn require_service_task(&self, task_id: &str) -> Result<ServiceTaskRecord> {
        self.service_get_task(task_id)?
            .with_context(|| format!("service task does not exist: {task_id}"))
    }

    fn ensure_service_task_eligible(&self, task: &ServiceTaskRecord) -> Result<()> {
        for dependency_id in &task.dependencies {
            let dependency = self.service_get_task(dependency_id)?.with_context(|| {
                format!("service task dependency does not exist: {dependency_id}")
            })?;
            if dependency.state != TASK_STATE_COMPLETE {
                anyhow::bail!(
                    "service task {} is blocked by incomplete dependency {}",
                    task.id,
                    dependency_id
                );
            }
        }
        if !task.parallelizable {
            for candidate in self.service_list_tasks(None, None)? {
                if candidate.id == task.id
                    || candidate.source_prd_path != task.source_prd_path
                    || candidate.state == TASK_STATE_COMPLETE
                {
                    continue;
                }
                let Some(candidate_number) = candidate
                    .source_task_number
                    .as_deref()
                    .and_then(|value| value.parse::<i64>().ok())
                else {
                    continue;
                };
                let Some(task_number) = task
                    .source_task_number
                    .as_deref()
                    .and_then(|value| value.parse::<i64>().ok())
                else {
                    continue;
                };
                if candidate_number < task_number {
                    anyhow::bail!(
                        "service task {} is blocked by earlier serial task {}",
                        task.id,
                        candidate.id
                    );
                }
            }
        }
        Ok(())
    }

    fn service_record_event(
        &self,
        event_type: &str,
        worker_id: Option<&str>,
        task_id: Option<&str>,
        payload: &serde_json::Value,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO service_events (id, type, worker_id, task_id, payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                uuid::Uuid::new_v4().to_string(),
                event_type,
                worker_id,
                task_id,
                payload.to_string(),
                service_now()
            ],
        )?;
        Ok(())
    }

    /// Return archived agents that still have pending tasks (queued or acked).
    /// Each entry is (agent_id, vec_of_task_ids), sorted by agent_id.
    pub fn archived_agents_with_pending_tasks(&self) -> Result<Vec<(String, Vec<String>)>> {
        let mut stmt = self.conn.prepare(
            "SELECT a.id, t.id
             FROM agents a
             JOIN tasks t ON (t.assigned_to = a.id OR t.lease_owner = a.id)
             WHERE a.status = 'archived'
               AND t.status IN ('queued', 'acked')
             ORDER BY a.id, t.rowid",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut result: Vec<(String, Vec<String>)> = Vec::new();
        for (agent_id, task_id) in rows {
            if let Some(last) = result.last_mut() {
                if last.0 == agent_id {
                    last.1.push(task_id);
                    continue;
                }
            }
            result.push((agent_id, vec![task_id]));
        }
        Ok(result)
    }

    /// Return active agents whose effective protocol version is below the threshold.
    /// Each entry is (agent_id, effective_version), sorted by agent_id.
    pub fn active_agents_below_protocol(
        &self,
        threshold: i64,
        default_version: i64,
    ) -> Result<Vec<(String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, protocol_version
             FROM agents
             WHERE status = 'active'
             ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], |row| {
                let id: String = row.get(0)?;
                let pv: Option<i64> = row.get(1)?;
                Ok((id, pv))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(rows
            .into_iter()
            .filter_map(|(id, pv)| {
                let effective = pv.unwrap_or(default_version);
                if effective < threshold {
                    Some((id, effective))
                } else {
                    None
                }
            })
            .collect())
    }
}

fn map_task_row(row: &rusqlite::Row) -> rusqlite::Result<TaskRecord> {
    Ok(TaskRecord {
        id: row.get(0)?,
        title: row.get(1)?,
        body: row.get(2)?,
        created_by: row.get(3)?,
        assigned_to: row.get(4)?,
        status: row.get(5)?,
        lease_owner: row.get(6)?,
        lease_expires_at: row.get(7)?,
        result_summary: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
        completed_at: row.get(11)?,
    })
}

fn map_service_worker_row(row: &rusqlite::Row) -> rusqlite::Result<ServiceWorkerRecord> {
    Ok(ServiceWorkerRecord {
        id: row.get(0)?,
        kind: row.get(1)?,
        role: row.get(2)?,
        status: row.get(3)?,
        capacity: row.get(4)?,
        current_task_id: row.get(5)?,
        last_heartbeat_at: row.get(6)?,
        metadata: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

fn map_service_task_row(row: &rusqlite::Row) -> rusqlite::Result<ServiceTaskRecord> {
    let acceptance_criteria_json: String = row.get(3)?;
    let acceptance_criteria = serde_json::from_str(&acceptance_criteria_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let parallelizable: i64 = row.get(10)?;
    let dependencies_json: String = row.get(11)?;
    let dependencies = serde_json::from_str(&dependencies_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(11, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let claude_suitable: i64 = row.get(14)?;
    Ok(ServiceTaskRecord {
        id: row.get(0)?,
        title: row.get(1)?,
        description: row.get(2)?,
        acceptance_criteria,
        source_prd_path: row.get(4)?,
        source_task_number: row.get(5)?,
        state: row.get(6)?,
        priority: row.get(7)?,
        preferred_model: row.get(8)?,
        role: row.get(9)?,
        parallelizable: parallelizable != 0,
        dependencies,
        scheduling_pool: row.get(12)?,
        estimated_size: row.get(13)?,
        claude_suitable: claude_suitable != 0,
        assigned_worker_id: row.get(15)?,
        eligible_providers: row.get(16)?,
        lease_owner: row.get(17)?,
        lease_expires_at: row.get(18)?,
        claimed_at: row.get(19)?,
        completed_worker_id: row.get(20)?,
        completed_worker_kind: row.get(21)?,
        claim_duration_secs: row.get(22)?,
        retry_count: row.get(23)?,
        failure_reason: row.get(24)?,
        attempt: row.get(25)?,
        max_attempts: row.get(26)?,
        acked_at: row.get(27)?,
        started_at: row.get(28)?,
        reported_at: row.get(29)?,
        verified_at: row.get(30)?,
        completed_at: row.get(31)?,
        last_error: row.get(32)?,
        retry_reason: row.get(33)?,
        report_hash: row.get(34)?,
        created_at: row.get(35)?,
        updated_at: row.get(36)?,
    })
}

fn map_service_task_report_row(row: &rusqlite::Row) -> rusqlite::Result<ServiceTaskReportRecord> {
    let files_inspected_json: String = row.get(2)?;
    let changed_files_json: String = row.get(3)?;
    let tests_run_json: String = row.get(4)?;
    let files_inspected = serde_json::from_str(&files_inspected_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let changed_files = serde_json::from_str(&changed_files_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let tests_run = serde_json::from_str(&tests_run_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(ServiceTaskReportRecord {
        task_id: row.get(0)?,
        summary: row.get(1)?,
        files_inspected,
        changed_files,
        tests_run,
        verification: row.get(5)?,
        risks: row.get(6)?,
        raw_report: row.get(7)?,
        created_at: row.get(8)?,
    })
}

fn map_service_prd_row(row: &rusqlite::Row) -> rusqlite::Result<ServicePrdRecord> {
    Ok(ServicePrdRecord {
        id: row.get(0)?,
        path: row.get(1)?,
        title: row.get(2)?,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
    })
}

fn map_service_event_row(row: &rusqlite::Row) -> rusqlite::Result<ServiceEventRecord> {
    Ok(ServiceEventRecord {
        id: row.get(0)?,
        event_type: row.get(1)?,
        worker_id: row.get(2)?,
        task_id: row.get(3)?,
        payload: row.get(4)?,
        created_at: row.get(5)?,
    })
}

fn service_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn service_task_select_clause() -> &'static str {
    "SELECT id, title, description, acceptance_criteria, source_prd_path,
            source_task_number, state, priority, preferred_model, role,
            parallelizable, dependencies, scheduling_pool, estimated_size, claude_suitable,
            assigned_worker_id, eligible_providers, lease_owner, lease_expires_at,
            claimed_at, completed_worker_id, completed_worker_kind, claim_duration_secs,
            retry_count, failure_reason,
            attempt, max_attempts, acked_at, started_at, reported_at, verified_at,
            completed_at, last_error, retry_reason, report_hash, created_at, updated_at"
}

fn bool_to_i64(value: bool) -> i64 {
    if value {
        1
    } else {
        0
    }
}

fn required_service_field(field: &str, value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        anyhow::bail!("service {field} cannot be empty");
    }
    Ok(value.to_string())
}

fn normalized_worker_kind(value: &str) -> Result<String> {
    let value = value.trim().to_ascii_lowercase();
    match value.as_str() {
        "codex" | "claude" | "openrouter" | "local" | "manual" => Ok(value),
        _ => anyhow::bail!(
            "invalid service worker kind '{value}'. Expected codex, claude, openrouter, local, or manual"
        ),
    }
}

fn normalized_worker_status(value: &str) -> Result<String> {
    let value = value.trim().to_ascii_lowercase();
    match value.as_str() {
        WORKER_STATE_STARTING
        | WORKER_STATE_READY
        | WORKER_STATE_ASSIGNED
        | WORKER_STATE_WORKING
        | WORKER_STATE_BLOCKED
        | WORKER_STATE_OFFLINE => Ok(value),
        _ => anyhow::bail!(
            "invalid service worker status '{value}'. Expected starting, ready, assigned, working, blocked, or offline"
        ),
    }
}

fn service_direct_claim_rank(worker: &ServiceWorkerRecord, task: &ServiceTaskRecord) -> i64 {
    if task.assigned_worker_id.as_deref() == Some(worker.id.as_str()) {
        0
    } else {
        1
    }
}

fn service_lane_claim_rank(
    worker: &ServiceWorkerRecord,
    task: &ServiceTaskRecord,
    lane_escape_secs: i64,
) -> i64 {
    if task.assigned_worker_id.as_deref() == Some(worker.id.as_str()) {
        return 0;
    }
    if service_lane_explicitly_matches_worker(worker, task) {
        return 1;
    }
    if task.eligible_providers.is_none() {
        return 2;
    }
    if service_task_lane_escaped(task, lane_escape_secs) {
        return 3;
    }
    9
}

fn service_worker_can_claim_task(
    worker: &ServiceWorkerRecord,
    task: &ServiceTaskRecord,
    lane_escape_secs: i64,
) -> bool {
    if task.assigned_worker_id.as_deref() == Some(worker.id.as_str()) {
        return worker
            .current_task_id
            .as_deref()
            .is_none_or(|id| id == task.id)
            && matches!(
                worker.status.as_str(),
                WORKER_STATE_READY | WORKER_STATE_ASSIGNED
            );
    }
    if worker.status != WORKER_STATE_READY || worker.current_task_id.is_some() {
        return false;
    }
    if !worker.role.eq_ignore_ascii_case(&task.role) && worker.role != "coding_worker" {
        return false;
    }
    service_lane_matches_worker(worker, task) || service_task_lane_escaped(task, lane_escape_secs)
}

fn service_lane_matches_worker(worker: &ServiceWorkerRecord, task: &ServiceTaskRecord) -> bool {
    task.eligible_providers.is_none() || service_lane_explicitly_matches_worker(worker, task)
}

fn service_lane_explicitly_matches_worker(
    worker: &ServiceWorkerRecord,
    task: &ServiceTaskRecord,
) -> bool {
    let Some(lane) = task.eligible_providers.as_deref() else {
        return false;
    };
    lane.split(',')
        .map(str::trim)
        .any(|provider| provider == worker.kind)
}

fn service_task_lane_escaped(task: &ServiceTaskRecord, lane_escape_secs: i64) -> bool {
    if task.eligible_providers.is_none() {
        return true;
    }
    let Ok(created_at) = chrono::DateTime::parse_from_rfc3339(&task.created_at) else {
        return false;
    };
    chrono::Utc::now()
        .signed_duration_since(created_at.with_timezone(&chrono::Utc))
        .num_seconds()
        >= lane_escape_secs.max(0)
}

fn service_time_after_secs(secs: i64) -> String {
    chrono::Utc::now()
        .checked_add_signed(chrono::Duration::seconds(secs.max(1)))
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn service_elapsed_secs_since(value: &str) -> Option<i64> {
    let started = chrono::DateTime::parse_from_rfc3339(value).ok()?;
    Some(
        chrono::Utc::now()
            .signed_duration_since(started.with_timezone(&chrono::Utc))
            .num_seconds()
            .max(0),
    )
}

fn service_pool_sizing_hint(providers: &[ServiceQueueProviderStat]) -> Option<String> {
    let mut rates = providers
        .iter()
        .filter(|provider| provider.completed_window > 0)
        .map(|provider| {
            (
                provider.worker_kind.as_str(),
                provider.completed_window as f64,
            )
        })
        .collect::<Vec<_>>();
    if rates.len() < 2 {
        return None;
    }
    rates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let (fast_kind, fast_count) = rates[0];
    let (slow_kind, slow_count) = rates[rates.len() - 1];
    if slow_count <= 0.0 {
        return None;
    }
    Some(format!(
        "{fast_kind} is completing {:.1}x more tasks than {slow_kind}; size worker pools accordingly.",
        fast_count / slow_count
    ))
}

fn service_worker_can_take_task(worker: &ServiceWorkerRecord, task: &ServiceTaskRecord) -> bool {
    if worker.status != WORKER_STATE_READY || worker.current_task_id.is_some() {
        return false;
    }
    if !worker.role.eq_ignore_ascii_case(&task.role) && worker.role != "coding_worker" {
        return false;
    }
    match task.scheduling_pool.as_str() {
        "manual" => worker.kind == "manual",
        "claude" => worker.kind == "claude" && task.claude_suitable,
        "codex" => worker.kind == "codex",
        "openrouter" => worker.kind == "openrouter",
        "local" => worker.kind == "local",
        preferred => worker.kind == preferred,
    }
}

struct ServiceTaskScheduling {
    pool: String,
    size: String,
    claude_suitable: bool,
}

fn service_task_scheduling_metadata(
    preferred_model: &str,
    description: &str,
    acceptance_criteria: &[String],
) -> ServiceTaskScheduling {
    let weighted_size =
        description.len() + acceptance_criteria.iter().map(String::len).sum::<usize>();
    let size = if weighted_size <= 800 && acceptance_criteria.len() <= 3 {
        "small"
    } else if weighted_size <= 1800 && acceptance_criteria.len() <= 5 {
        "medium"
    } else {
        "large"
    };
    let claude_suitable = preferred_model == "claude" && size == "small";
    let pool = match preferred_model {
        "claude" if claude_suitable => "claude",
        "claude" => "codex",
        other => other,
    };
    ServiceTaskScheduling {
        pool: pool.to_string(),
        size: size.to_string(),
        claude_suitable,
    }
}

fn ensure_service_updated(updated: usize, kind: &str, id: &str) -> Result<()> {
    if updated == 1 {
        Ok(())
    } else {
        anyhow::bail!("stale service {kind} state for {id}")
    }
}

fn redact_report_list(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| redact_report_text(&value))
        .collect()
}

fn redact_report_text(value: &str) -> String {
    let mut redacted = value.to_string();
    for pattern in [
        r#"(?i)(api[_-]?key|token|secret|password)\s*[:=]\s*['"]?[^'"\s]+"#,
        r"sk-[A-Za-z0-9_\-]{12,}",
        r"or-[A-Za-z0-9_\-]{12,}",
        r"/Users/[A-Za-z0-9._-]+",
    ] {
        let regex = regex::Regex::new(pattern).expect("valid redaction regex");
        redacted = regex.replace_all(&redacted, "[REDACTED]").to_string();
    }
    redacted
}

fn service_report_hash(report: &ServiceTaskReportInput) -> Result<String> {
    let canonical = serde_json::to_vec(report)?;
    let digest = Sha256::digest(canonical);
    Ok(format!("sha256:{:x}", digest))
}

fn parse_prd_checkbox_tasks(source_prd_path: &str, content: &str) -> Vec<ServiceTaskInput> {
    content
        .lines()
        .filter_map(|line| {
            let number = checkbox_task_number(line)?;
            let after_checkbox = line.split_once(']')?.1.trim();
            let title = after_checkbox
                .trim_start_matches(|ch: char| ch == '.' || ch == '-' || ch.is_ascii_digit())
                .trim()
                .trim_end_matches("- Parallel")
                .trim_end_matches("- Serial")
                .trim()
                .to_string();
            if title.is_empty() {
                return None;
            }
            Some(ServiceTaskInput {
                title: title.clone(),
                description: title,
                acceptance_criteria: vec!["Task is implemented, tested, and reported.".to_string()],
                source_prd_path: source_prd_path.to_string(),
                source_task_number: Some(number),
                priority: 0,
                preferred_model: "codex".to_string(),
                role: "coding_worker".to_string(),
                parallelizable: line.contains("Parallel"),
                max_attempts: None,
                dependencies: None,
            })
        })
        .collect()
}

fn checkbox_task_number(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    if !(trimmed.starts_with("- [ ]")
        || trimmed.starts_with("- [x]")
        || trimmed.starts_with("- [X]"))
    {
        return None;
    }
    let after_checkbox = trimmed.split_once(']')?.1.trim();
    let number: String = after_checkbox
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    if number.is_empty() {
        None
    } else {
        Some(number)
    }
}

fn map_autopilot_run_row(row: &rusqlite::Row) -> rusqlite::Result<AutopilotRunRecord> {
    Ok(AutopilotRunRecord {
        id: row.get(0)?,
        prd_path: row.get(1)?,
        status: row.get(2)?,
        created_at: row.get(3)?,
        completed_at: row.get(4)?,
    })
}

fn map_autopilot_agent_row(row: &rusqlite::Row) -> rusqlite::Result<AutopilotAgentRecord> {
    Ok(AutopilotAgentRecord {
        id: row.get(0)?,
        run_id: row.get(1)?,
        name: row.get(2)?,
        role: row.get(3)?,
        model_provider: row.get(4)?,
        skills_prompt: row.get(5)?,
        status: row.get(6)?,
    })
}

fn map_autopilot_task_row(row: &rusqlite::Row) -> rusqlite::Result<AutopilotTaskRecord> {
    let acceptance_criteria_json: String = row.get(9)?;
    let acceptance_criteria = serde_json::from_str(&acceptance_criteria_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(9, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(AutopilotTaskRecord {
        id: row.get(0)?,
        run_id: row.get(1)?,
        title: row.get(2)?,
        description: row.get(3)?,
        assigned_role: row.get(4)?,
        assigned_agent_id: row.get(5)?,
        status: row.get(6)?,
        priority: row.get(7)?,
        risk_level: row.get(8)?,
        acceptance_criteria,
        created_at: row.get(10)?,
        completed_at: row.get(11)?,
    })
}

fn map_autopilot_task_dependency_row(
    row: &rusqlite::Row,
) -> rusqlite::Result<AutopilotTaskDependencyRecord> {
    Ok(AutopilotTaskDependencyRecord {
        task_id: row.get(0)?,
        depends_on_task_id: row.get(1)?,
    })
}

fn map_autopilot_review_row(row: &rusqlite::Row) -> rusqlite::Result<AutopilotReviewRecord> {
    Ok(AutopilotReviewRecord {
        id: row.get(0)?,
        task_id: row.get(1)?,
        reviewer_agent_id: row.get(2)?,
        verdict: row.get(3)?,
        notes: row.get(4)?,
        created_at: row.get(5)?,
    })
}

fn map_autopilot_terminal_session_row(
    row: &rusqlite::Row,
) -> rusqlite::Result<AutopilotTerminalSessionRecord> {
    Ok(AutopilotTerminalSessionRecord {
        id: row.get(0)?,
        run_id: row.get(1)?,
        agent_id: row.get(2)?,
        terminal_kind: row.get(3)?,
        command: row.get(4)?,
        status: row.get(5)?,
    })
}

fn autopilot_terminal_sessions_match_plan(
    existing: &[AutopilotTerminalSessionRecord],
    planned: &[AutopilotTerminalSessionRecord],
) -> bool {
    existing.len() == planned.len()
        && existing.iter().zip(planned).all(|(existing, planned)| {
            existing.run_id == planned.run_id
                && existing.agent_id == planned.agent_id
                && existing.terminal_kind == planned.terminal_kind
                && existing.command == planned.command
                && existing.status == planned.status
        })
}

fn required_autopilot_agent_field(field: &str, value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        anyhow::bail!("autopilot agent {field} cannot be empty");
    }
    Ok(value.to_string())
}

fn required_autopilot_task_field(field: &str, value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        anyhow::bail!("autopilot task {field} cannot be empty");
    }
    Ok(value.to_string())
}

fn task_status_label(status: &TaskGraphStatus) -> &'static str {
    match status {
        TaskGraphStatus::ReadyParallel => "READY_PARALLEL",
        TaskGraphStatus::Blocked => "BLOCKED",
        TaskGraphStatus::Sequential => "SEQUENTIAL",
        TaskGraphStatus::ReviewRequired => "REVIEW_REQUIRED",
        TaskGraphStatus::Done => "DONE",
        TaskGraphStatus::Failed => "FAILED",
    }
}

fn risk_level_label(risk_level: &RiskLevel) -> &'static str {
    match risk_level {
        RiskLevel::Low => "low",
        RiskLevel::Medium => "medium",
        RiskLevel::High => "high",
    }
}

fn terminal_kind_label(terminal_kind: &TerminalKind) -> &'static str {
    match terminal_kind {
        TerminalKind::Tmux => "tmux",
    }
}

fn terminal_session_status_label(status: &TerminalSessionStatus) -> &'static str {
    match status {
        TerminalSessionStatus::Planned => "planned",
        TerminalSessionStatus::Running => "running",
        TerminalSessionStatus::Failed => "failed",
        TerminalSessionStatus::Closed => "closed",
    }
}

fn normalized_review_verdict(verdict: &str) -> Result<&'static str> {
    match verdict.trim().to_ascii_lowercase().as_str() {
        "accepted" | "accept" | "approved" | "approve" => Ok("accepted"),
        "rejected" | "reject" | "failed" | "fail" => Ok("rejected"),
        value if value.is_empty() => anyhow::bail!("autopilot review verdict cannot be empty"),
        value => anyhow::bail!(
            "invalid autopilot review verdict '{value}'. Expected accepted or rejected"
        ),
    }
}

fn model_provider_rank(provider: &str) -> i64 {
    match provider.trim().to_ascii_lowercase().as_str() {
        "local" => 1,
        "openrouter_free" | "openrouter-free" => 2,
        "openrouter_cheap" | "openrouter-cheap" | "gemini" | "opencode" => 3,
        "codex" => 4,
        "claude" => 5,
        _ => 0,
    }
}

fn autopilot_record_is_claude_eligible(
    task: &AutopilotTaskRecord,
    policy: &AdaptiveSchedulingConfig,
) -> bool {
    claude_task_eligibility(
        &TaskGraphTask {
            id: format!("task-{}", task.id),
            title: task.title.clone(),
            description: task.description.clone(),
            assigned_role: task.assigned_role.clone(),
            status: task_status_from_record(&task.status),
            priority: task.priority,
            risk_level: risk_level_from_record(task.risk_level.as_deref()),
            acceptance_criteria: task.acceptance_criteria.clone(),
            likely_files: Vec::new(),
            test_requirements: Vec::new(),
            depends_on: Vec::new(),
        },
        policy,
    )
    .eligible
}

fn task_status_from_record(status: &str) -> TaskGraphStatus {
    match status {
        "READY_PARALLEL" => TaskGraphStatus::ReadyParallel,
        "BLOCKED" => TaskGraphStatus::Blocked,
        "REVIEW_REQUIRED" => TaskGraphStatus::ReviewRequired,
        "DONE" => TaskGraphStatus::Done,
        "FAILED" => TaskGraphStatus::Failed,
        _ => TaskGraphStatus::Sequential,
    }
}

fn risk_level_from_record(risk_level: Option<&str>) -> RiskLevel {
    match risk_level {
        Some("low") => RiskLevel::Low,
        Some("high") => RiskLevel::High,
        _ => RiskLevel::Medium,
    }
}

fn ensure_autopilot_task_updated(updated: usize, task_id: i64) -> Result<()> {
    if updated == 1 {
        Ok(())
    } else {
        anyhow::bail!("stale autopilot task state for {task_id}")
    }
}

fn ensure_task_updated(updated: usize, task_id: &str) -> Result<()> {
    if updated == 1 {
        Ok(())
    } else {
        anyhow::bail!("stale task state for {task_id}")
    }
}
