use anyhow::{Context, Result};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use crate::autopilot::{
    RiskLevel, TaskGraphStatus, TaskGraphTask, TerminalKind, TerminalSessionPlan,
    TerminalSessionStatus,
};
use crate::tasks::TaskRecord;

const DEFAULT_MESSAGE_KIND: &str = "note";
const TASK_ASSIGNED_KIND: &str = "task_assigned";
const TASK_STATUS_QUEUED: &str = "queued";
const TASK_STATUS_ACKED: &str = "acked";
const TASK_STATUS_COMPLETED: &str = "completed";
const TASK_LEASE_SECS: i64 = 15 * 60;

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
        let mut assigned = Vec::with_capacity(ready_tasks.len());
        let mut next_worker_index = 0usize;
        for task in ready_tasks {
            let worker = if let Some(role) = task.assigned_role.as_deref() {
                let worker = workers_by_role.get(role).with_context(|| {
                    format!(
                        "ready autopilot task '{}' has no worker for role '{role}'",
                        task.id
                    )
                })?;
                if let Some(worker_index) = workers
                    .iter()
                    .position(|candidate| candidate.id == worker.id)
                {
                    next_worker_index = worker_index + 1;
                }
                worker
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
