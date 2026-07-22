#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
REPO_ROOT="$(pwd -P)"

PORT="${COORDINATE_SMOKE_PORT:-18787}"
SERVICE_URL="http://127.0.0.1:${PORT}"
SESSION="${COORDINATE_SMOKE_TMUX_SESSION:-coordinate-sim-worker-smoke}"
DB_DIR="$(mktemp -d -t coordinate-bridge-smoke.XXXXXX)"
LOG_FILE="${DB_DIR}/coordinate.log"

cleanup() {
  tmux kill-session -t "${SESSION}" >/dev/null 2>&1 || true
  if [ -n "${SERVICE_PID:-}" ]; then
    kill "${SERVICE_PID}" >/dev/null 2>&1 || true
    wait "${SERVICE_PID}" 2>/dev/null || true
  fi
}
trap cleanup EXIT

wait_http() {
  local url="$1"
  local deadline=$((SECONDS + 30))
  until curl -fsS "$url" >/dev/null 2>&1; do
    if (( SECONDS >= deadline )); then
      echo "smoke: timed out waiting for ${url}" >&2
      tail -n 80 "${LOG_FILE}" >&2 || true
      exit 1
    fi
    sleep 1
  done
}

echo "smoke: starting Coordinate service on ${SERVICE_URL}"
mkdir -p "${DB_DIR}/.squad"
(
  cd "${DB_DIR}"
  cargo run --quiet --manifest-path "${REPO_ROOT}/Cargo.toml" -- serve --bind "127.0.0.1:${PORT}"
) >"${LOG_FILE}" 2>&1 &
SERVICE_PID=$!
wait_http "${SERVICE_URL}/health"

echo "smoke: starting simulated tmux worker ${SESSION}"
tmux kill-session -t "${SESSION}" >/dev/null 2>&1 || true
tmux new-session -d -s "${SESSION}" "cd ${REPO_ROOT} && cargo run --quiet -- host-bridge-sim-worker"

echo "smoke: creating task"
task_json="$(curl -fsS -H 'Content-Type: application/json' -d '{
  "title":"simulated host bridge smoke",
  "description":"Verify host bridge ack/report/complete flow using simulated worker.",
  "acceptanceCriteria":["worker ack is recorded","report is persisted","task completes"],
  "sourcePrdPath":"fractalmaster/HOST_WORKER_BRIDGE_PRD.md",
  "sourceTaskNumber":"32",
  "priority":1,
  "preferredModel":"codex",
  "role":"coding_worker",
  "parallelizable":true
}' "${SERVICE_URL}/tasks")"
task_id="$(printf '%s' "${task_json}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"

echo "smoke: running pull-claim bridge ticks for ${task_id}"
for _ in 1 2 3 4 5 6; do
  cargo run --quiet -- host-bridge \
    --service-url "${SERVICE_URL}" \
    --worker-id sim-codex-1 \
    --kind codex \
    --role coding_worker \
    --tmux-target "${SESSION}" \
    --command "cargo run --quiet -- host-bridge-sim-worker" \
    --no-create-session \
    --once >/dev/null
  state="$(curl -fsS "${SERVICE_URL}/tasks/${task_id}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["state"])')"
  if [ "${state}" = "complete" ]; then
    break
  fi
  sleep 1
done

curl -fsS "${SERVICE_URL}/tasks/${task_id}/report" >/dev/null
final_state="$(curl -fsS "${SERVICE_URL}/tasks/${task_id}" | python3 -c 'import json,sys; body=json.load(sys.stdin); print(body["state"])')"
if [ "${final_state}" != "complete" ]; then
  echo "smoke: expected complete task, got ${final_state}" >&2
  tmux capture-pane -p -J -S -200 -t "${SESSION}" >&2 || true
  exit 1
fi

echo "smoke: PASS ${task_id}"
