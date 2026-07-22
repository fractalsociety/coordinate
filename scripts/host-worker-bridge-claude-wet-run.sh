#!/usr/bin/env bash
set -euo pipefail

SERVICE_URL="${COORDINATE_URL:-http://127.0.0.1:8787}"
SESSION="${COORDINATE_CLAUDE_TMUX_SESSION:-coordinate-claude-wet-run}"
TIMEOUT_SECS="${COORDINATE_WET_RUN_TIMEOUT_SECS:-300}"
READY_TIMEOUT_SECS="${COORDINATE_CLAUDE_READY_TIMEOUT_SECS:-120}"
CLAUDE_COMMAND="${COORDINATE_CLAUDE_COMMAND:-claude --dangerously-skip-permissions}"
WORKER_ID="${COORDINATE_CLAUDE_WORKER_ID:-claude-wet-run-$(date +%s)}"
RESET_SESSION="${COORDINATE_CLAUDE_RESET_SESSION:-1}"
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
if [ "${RESET_SESSION}" != "0" ] && [ "${RESET_SESSION}" != "false" ]; then
  tmux kill-session -t "${SESSION}" >/dev/null 2>&1 || true
fi
tmux has-session -t "${SESSION}" >/dev/null 2>&1 || tmux new-session -d -s "${SESSION}" "${CLAUDE_COMMAND}"

ready_deadline=$((SECONDS + READY_TIMEOUT_SECS))
while (( SECONDS < ready_deadline )); do
  pane="$(tmux capture-pane -p -J -S -80 -t "${SESSION}" 2>/dev/null | tail -n 30 || true)"
  if [ -n "${pane}" ] && ! printf '%s' "${pane}" | grep -Eq 'Loading|Starting'; then
    break
  fi
  sleep 2
done
if (( SECONDS >= ready_deadline )); then
  echo "Claude pane did not become ready within ${READY_TIMEOUT_SECS}s" >&2
  tmux capture-pane -p -J -S -120 -t "${SESSION}" >&2 || true
  exit 1
fi

task_json="$(curl -fsS -H 'Content-Type: application/json' -d '{
  "title":"claude wet-run tiny task",
  "description":"Acknowledge the task and return the required Coordinate report marker. This is intentionally tiny.",
  "acceptanceCriteria":["prints COORDINATE_ACK","prints COORDINATE_REPORT_JSON"],
  "sourcePrdPath":"fractalmaster/HOST_WORKER_BRIDGE_PRD.md",
  "sourceTaskNumber":"34",
  "priority":1,
  "preferredModel":"claude",
  "role":"coding_worker",
  "parallelizable":true
}' "${SERVICE_URL}/tasks")"
task_id="$(printf '%s' "${task_json}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"

echo "Created Claude wet-run task: ${task_id}"
echo "Attach if needed: tmux attach -t ${SESSION}"
echo "Starting pull-claim bridge and waiting up to ${TIMEOUT_SECS}s for completion."
cargo run --quiet -- host-bridge \
  --service-url "${SERVICE_URL}" \
  --worker-id "${WORKER_ID}" \
  --kind claude \
  --role coding_worker \
  --tmux-target "${SESSION}" &
BRIDGE_PID=$!

deadline=$((SECONDS + TIMEOUT_SECS))
while (( SECONDS < deadline )); do
  state="$(curl -fsS "${SERVICE_URL}/tasks/${task_id}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["state"])')"
  if [ "${state}" = "complete" ]; then
    curl -fsS "${SERVICE_URL}/tasks/${task_id}/report" >/dev/null
    echo "Claude wet-run PASS: ${task_id}"
    exit 0
  fi
  if [ "${state}" = "failed" ] || [ "${state}" = "blocked" ]; then
    echo "Claude wet-run failed with state ${state}: ${task_id}" >&2
    tmux capture-pane -p -J -S -200 -t "${SESSION}" >&2 || true
    exit 1
  fi
  sleep 2
done

echo "Claude wet-run timed out waiting for ${task_id}" >&2
tmux capture-pane -p -J -S -200 -t "${SESSION}" >&2 || true
exit 1
