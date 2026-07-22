#!/usr/bin/env bash
set -euo pipefail

COORDINATE_URL="${COORDINATE_URL:-http://127.0.0.1:8787}"
OUTPUT="${1:-host-bridge-coordinate-export.jsonl}"

python3 - "$COORDINATE_URL" "$OUTPUT" <<'PY'
import hashlib
import json
import sys
import urllib.error
import urllib.request

base, output = sys.argv[1], sys.argv[2]

def get(path):
    with urllib.request.urlopen(base.rstrip("/") + path, timeout=15) as res:
        return json.loads(res.read().decode())

def stable(value):
    if isinstance(value, list):
        return "[" + ",".join(stable(v) for v in value) + "]"
    if isinstance(value, dict):
        return "{" + ",".join(json.dumps(k) + ":" + stable(value[k]) for k in sorted(value) if value[k] is not None) + "}"
    return json.dumps(value, separators=(",", ":"))

def hash_obj(value):
    return hashlib.sha256(stable(value).encode()).hexdigest()

rows = []
events_by_task = {}
try:
    for event in get("/events?limit=1000"):
        task_id = event.get("taskId")
        if not task_id:
            continue
        events_by_task.setdefault(task_id, []).append(event)
except urllib.error.HTTPError:
    pass

for task in get("/tasks"):
    report = None
    try:
        report = get(f"/tasks/{task['id']}/report")
    except urllib.error.HTTPError:
        pass
    task_events = events_by_task.get(task["id"], [])
    retry_reap_events = [
        {
            "type": event.get("eventType") or event.get("type"),
            "workerId": event.get("workerId"),
            "createdAt": event.get("createdAt"),
            "payload": event.get("payload"),
        }
        for event in task_events
        if any(token in str(event.get("eventType") or event.get("type") or "").lower() for token in ("retry", "lease", "reap", "assignment_released", "offline"))
    ]
    payload = {
        "schema": "dataevol.coordinate_task_trace.v1",
        "taskId": task["id"],
        "state": task.get("state"),
        "workerId": task.get("assignedWorkerId"),
        "completedWorkerId": task.get("completedWorkerId"),
        "completedWorkerKind": task.get("completedWorkerKind"),
        "lane": task.get("schedulingPool") or task.get("preferredModel"),
        "eligibleProviders": task.get("eligibleProviders"),
        "leaseOwner": task.get("leaseOwner"),
        "leaseExpiresAt": task.get("leaseExpiresAt"),
        "claimDurationSecs": task.get("claimDurationSecs"),
        "retryCount": task.get("retryCount"),
        "retryReason": task.get("retryReason"),
        "reapEvents": retry_reap_events,
        "acceptanceCriteria": task.get("acceptanceCriteria", []),
        "acceptanceOutcome": "accepted" if task.get("state") == "complete" and report else "pending",
        "reportSummary": (report or {}).get("summary"),
        "verification": (report or {}).get("verification"),
        "testsRun": (report or {}).get("testsRun", []),
        "traceHash": hash_obj({"task": task, "report": report}),
    }
    rows.append(payload)

with open(output, "w", encoding="utf-8") as fh:
    for row in rows:
        fh.write(json.dumps(row, sort_keys=True) + "\n")
print(f"wrote {len(rows)} Coordinate task trace rows to {output}")
PY
