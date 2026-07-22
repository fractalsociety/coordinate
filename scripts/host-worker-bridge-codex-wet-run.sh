#!/usr/bin/env bash
set -euo pipefail

SERVICE_URL="${COORDINATE_URL:-http://127.0.0.1:8787}"
SESSION="${COORDINATE_CODEX_TMUX_SESSION:-coordinate-codex-wet-run}"
TIMEOUT_SECS="${COORDINATE_WET_RUN_TIMEOUT_SECS:-300}"
READY_TIMEOUT_SECS="${COORDINATE_CODEX_READY_TIMEOUT_SECS:-180}"
CODEX_COMMAND="${COORDINATE_CODEX_COMMAND:-codex --dangerously-bypass-approvals-and-sandbox --no-alt-screen}"
WORKER_ID="${COORDINATE_CODEX_WORKER_ID:-codex-wet-run-$(date +%s)}"
BRIDGE_PID=""

cd "$(dirname "$0")/.."

cleanup() {
  if [ -n "${BRIDGE_PID}" ]; then
    kill "${BRIDGE_PID}" >/dev/null 2>&1 || true
    wait "${BRIDGE_PID}" 2>/dev/null || true
  fi
}
trap cleanup EXIT

curl -fsS "${SERVICE_URL}/health" >/dev/null
tmux has-session -t "${SESSION}" >/dev/null 2>&1 || tmux new-session -d -s "${SESSION}" "${CODEX_COMMAND}"

ready_deadline=$((SECONDS + READY_TIMEOUT_SECS))
while (( SECONDS < ready_deadline )); do
  pane="$(tmux capture-pane -p -J -S -80 -t "${SESSION}" 2>/dev/null | tail -n 30 || true)"
  if [ -n "${pane}" ] && ! printf '%s' "${pane}" | grep -Eq 'model:[[:space:]]*loading|Loading|Starting'; then
    break
  fi
  sleep 2
done
if (( SECONDS >= ready_deadline )); then
  echo "Codex pane did not become ready within ${READY_TIMEOUT_SECS}s" >&2
  tmux capture-pane -p -J -S -120 -t "${SESSION}" >&2 || true
  exit 1
fi

task_json="$(curl -fsS -H 'Content-Type: application/json' -d '{
  "title":"codex wet-run tiny task",
  "description":"Acknowledge the task, inspect no files, and return the required Coordinate report marker.",
  "acceptanceCriteria":["prints COORDINATE_ACK","prints COORDINATE_REPORT_JSON"],
  "sourcePrdPath":"fractalmaster/HOST_WORKER_BRIDGE_PRD.md",
  "sourceTaskNumber":"33",
  "priority":1,
  "preferredModel":"codex",
  "role":"coding_worker",
  "parallelizable":true
}' "${SERVICE_URL}/tasks")"
task_id="$(printf '%s' "${task_json}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"

echo "Created Codex wet-run task: ${task_id}"
echo "Attach if needed: tmux attach -t ${SESSION}"
echo "Starting pull-claim bridge and waiting up to ${TIMEOUT_SECS}s for completion."
cargo run --quiet -- host-bridge \
  --service-url "${SERVICE_URL}" \
  --worker-id "${WORKER_ID}" \
  --kind codex \
  --role coding_worker \
  --tmux-target "${SESSION}" &
BRIDGE_PID=$!

deadline=$((SECONDS + TIMEOUT_SECS))
while (( SECONDS < deadline )); do
  state="$(curl -fsS "${SERVICE_URL}/tasks/${task_id}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["state"])')"
  if [ "${state}" = "complete" ]; then
    curl -fsS "${SERVICE_URL}/tasks/${task_id}/report" >/dev/null
    echo "Codex wet-run PASS: ${task_id}"
    exit 0
  fi
  if [ "${state}" = "failed" ] || [ "${state}" = "blocked" ]; then
    echo "Codex wet-run failed with state ${state}: ${task_id}" >&2
    tmux capture-pane -p -J -S -200 -t "${SESSION}" >&2 || true
    exit 1
  fi
  sleep 2
done

echo "Codex wet-run timed out waiting for ${task_id}" >&2
tmux capture-pane -p -J -S -200 -t "${SESSION}" >&2 || true
exit 1
