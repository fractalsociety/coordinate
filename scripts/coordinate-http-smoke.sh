#!/usr/bin/env bash
set -euo pipefail

BASE_URL="${COORDINATE_API_URL:-http://127.0.0.1:8787}"

curl_json() {
  curl -fsS "$@"
}

curl_json "$BASE_URL/health" >/dev/null

curl_json "$BASE_URL/workers/register" \
  -H 'Content-Type: application/json' \
  -d '{"id":"codex-smoke","kind":"codex","role":"coding_worker","capacity":1}' >/dev/null

TASK_ID="$(curl_json "$BASE_URL/tasks" \
  -H 'Content-Type: application/json' \
  -d '{"title":"Smoke task","description":"Verify Coordinate HTTP lifecycle","acceptanceCriteria":["Lifecycle completes"],"sourcePrdPath":"smoke.md","sourceTaskNumber":"1","priority":1,"preferredModel":"codex","role":"coding_worker","parallelizable":true}' \
  | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')"

test -n "$TASK_ID"

curl_json "$BASE_URL/tasks/$TASK_ID/assign" \
  -H 'Content-Type: application/json' \
  -d '{"workerId":"codex-smoke"}' >/dev/null
curl_json -X POST "$BASE_URL/tasks/$TASK_ID/ack" >/dev/null
curl_json "$BASE_URL/tasks/$TASK_ID/report" \
  -H 'Content-Type: application/json' \
  -d '{"summary":"done","filesInspected":[],"changedFiles":[],"testsRun":["smoke"],"verification":"passed","risks":"none","rawReport":"passed"}' >/dev/null
curl_json -X POST "$BASE_URL/tasks/$TASK_ID/complete" >/dev/null

curl_json "$BASE_URL/tasks/$TASK_ID" | grep -q '"state":"complete"'
echo "Coordinate HTTP smoke passed: $TASK_ID"
