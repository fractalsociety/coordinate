# Coordinate HTTP Service

Run service mode from a Squad workspace:

```bash
squad init
squad serve --bind 127.0.0.1:8787
```

Health:

```bash
curl http://127.0.0.1:8787/health
```

Create a task:

```bash
curl -s http://127.0.0.1:8787/tasks \
  -H 'Content-Type: application/json' \
  -d '{
    "title": "Implement task",
    "description": "Small coding task",
    "acceptanceCriteria": ["Tests pass"],
    "sourcePrdPath": "PRD.md",
    "priority": 1,
    "preferredModel": "codex",
    "role": "coding_worker",
    "parallelizable": true
  }'
```

Docker service mode is the default:

```bash
docker run --rm -p 8787:8787 fractal-ecosystem-coordinate
```

CLI debugging override:

```bash
docker run --rm -it --entrypoint sh fractal-ecosystem-coordinate
squad init
squad agents
```

Notes:

- The HTTP service coordinates tasks and worker state; it does not run Claude or Codex inside the Linux container.
- Host tmux/macOS worker execution is bridged by `squad host-bridge`, which runs on the macOS host and talks to this service over HTTP.
- Reports are redacted before storage for obvious tokens, API keys, secrets, and private macOS user paths.

## Host tmux Worker Bridge

The service container should own task state. The macOS host should own real terminal execution. Start the HTTP service first, then run one bridge process per real tmux worker pane:

```bash
squad host-bridge \
  --service-url http://127.0.0.1:8787 \
  --worker-id codex-1 \
  --kind codex \
  --role coding_worker \
  --tmux-target coordinate:0.1
```

What the bridge does:

- Registers the worker with `/workers/register`.
- Sends `/workers/{id}/heartbeat` on every interval.
- Pulls one queued task when the worker is idle, assigns it, acknowledges it, and injects a task brief into the tmux pane.
- Captures pane output and posts `/tasks/{id}/report` plus `/tasks/{id}/complete` when the worker prints a structured completion marker.
- Posts `/tasks/{id}/fail` when the pane prints a blocked or failed marker.

Completion marker contract:

```text
COORDINATE_REPORT_JSON: {"summary":"done","filesInspected":["src/service.rs"],"changedFiles":["src/service.rs"],"testsRun":["cargo test"],"verification":"passed","risks":"none","rawReport":"details"}
```

Failure markers:

```text
COORDINATE_BLOCKED: waiting for credentials
COORDINATE_FAILED: tests failed
```

Useful options:

- `--once` runs one bridge tick and exits. Use it for smoke tests.
- `--no-auto-assign` only heartbeats and reports for already assigned work.
- `--interval-secs <n>` changes the heartbeat/poll interval.
