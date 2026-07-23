# Host Worker Bridge Inventory

This inventory backs `fractalmaster/HOST_WORKER_BRIDGE_TASK_LIST.md` task 1.

## Coordinate HTTP Service

Implemented in `src/service.rs`.

- Health: `GET /health`
- Workers: `GET /workers`, `POST /workers/register`, `POST /workers/:worker_id/heartbeat`, `POST /workers/:worker_id/ready`, `POST /workers/:worker_id/offline`, `POST /workers/expire-stale`
- Tasks: `GET /tasks`, `POST /tasks`, `GET /tasks/:task_id`, `POST /tasks/:task_id/assign`, `POST /tasks/:task_id/ack`, `POST /tasks/:task_id/start`, `POST /tasks/:task_id/progress`, `GET|POST /tasks/:task_id/report`, `POST /tasks/:task_id/verify`, `POST /tasks/:task_id/retry`, `POST /tasks/:task_id/complete`, `POST /tasks/:task_id/fail`, `POST /tasks/requeue-stale`
- Events: `GET /events`
- PRDs: `GET /prds`, `POST /prds/import`, `POST /prds/sync`, `GET /prds/:prd_path/tasks`

## Coordinate Store

Implemented in `src/store.rs`.

- Worker table: `service_workers` stores kind, role, status, capacity, current task, heartbeat timestamp, metadata, and timestamps.
- Task table: `service_tasks` stores lifecycle state, assignment, retry count, attempt count, max attempts, ack/start/report/verify/complete timestamps, report hash, retry reason, and last error.
- Report table: `service_task_reports` stores redacted summaries, files, tests, verification, risks, and raw report text.
- Event table: `service_events` stores append-only worker/task lifecycle events.
- PRD table: `service_prds` stores imported PRD paths and titles.

## Canonical Lifecycles

- Worker states: `starting`, `ready`, `assigned`, `working`, `blocked`, `offline`
- Task states: `queued`, `assigned`, `acked`, `working`, `reported`, `verified`, `complete`, `failed`, `blocked`

## Host Worker Contracts

The host macOS bridge should:

- Register each tmux-backed worker with `POST /workers/register`.
- Mark it ready after its terminal session is loaded with `POST /workers/:worker_id/ready`.
- Send heartbeats with `POST /workers/:worker_id/heartbeat`.
- Accept task assignment, then call task `ack`, `start`, `progress`, `report`, and `verify` or `fail`.
- Mark a worker offline before shutting down or after terminal failure.

## Fractalwork Client Contract

Fractalwork can continue using the existing task/report routes and can consume the richer lifecycle fields without breaking current callers. The next integration step is to update its Coordinate client to read worker status, task timestamps, attempt metadata, report hashes, and stale/offline events.

## tmux / macOS Scripts

Host terminal execution remains outside Docker. The future host bridge should own tmux session discovery/spawn, readiness probes for Codex and Claude, guaranteed Enter/send-key delivery, and structured report parsing.

## Host Bridge Config

`squad host-bridge --config host-bridge.toml` expands one or more host workers into tmux-backed bridge loops:

```toml
serviceUrl = "http://127.0.0.1:8787"
tmuxSessionPrefix = "coordinate"
startupTimeoutSecs = 90
readinessPollSecs = 2
intervalSecs = 5
executionGraphUrl = "http://127.0.0.1:8091"

[nodeVerifier]
program = "dataevol-node-verifier"
args = []
timeoutSecs = 300
minVerifiers = 1
requireHiddenRegression = true

[[workers]]
id = "codex-1"
kind = "codex"
role = "coding_worker"

[[workers]]
id = "claude-1"
kind = "claude"
role = "coding_worker"
readinessTimeoutSecs = 45
```

Codex workers default to `codex --yolo`; Claude workers default to `claude --dangerously-skip-permissions`. The bridge attaches to an existing target when present, otherwise it spawns a tmux session unless `createSession = false`.

Compiled graph tasks fail closed unless `nodeVerifier` is configured. The verifier
receives the node, acceptance criteria, and worker report as JSON on stdin. It must
run DataEvol public and isolated hidden checks and return JSON containing
`publicCheck`, `hiddenRegression`, and `verifierVerdicts`, with every evidence item
carrying a `sha256:<64 hex>` hash. Coordinate independently enforces the configured
evidence floor, persists the bundle and its aggregate hash, and then completes,
retries, or escalates the node while updating its execution-board checkout.
