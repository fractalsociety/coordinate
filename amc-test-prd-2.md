# AMC Test PRD 2

## Product Goals
- Validate a second macOS Terminal autopilot run using a fresh PRD and a strict Claude/Codex split.
- Confirm launched agents receive normal squad tasks after Terminal.app opens.

## Milestones
- MVP 1: Parse a fresh 10-task PRD.
- MVP 2: Generate a 10-agent team with 5 Claude and 5 Codex sessions.
- MVP 3: Deliver ready work into the regular squad task queue.

## Implementation Task Checklist
[x] 1. Confirm the second PRD is parsed into exactly ten tasks - Parallel
[x] 2. Verify the active autopilot config still disables Gemini and local providers - Parallel
[x] 3. Validate generated roles include manager, inspector, architect, backend, data, terminal, test, docs, release, and quality coverage - Parallel
[x] 4. Check that macOS Terminal session planning keeps exactly five Claude commands and five Codex commands - Parallel
[x] 5. Verify launch-time task delivery creates normal squad tasks for ready assignments - Parallel
[x] 6. Persist the run, generated agents, tasks, sessions, and report artifacts - Sequential
[x] 7. Confirm ready assignments are visible through `squad task list` - Sequential
[x] 8. Confirm worker inboxes can receive task assignment messages - Sequential
[x] 9. Review terminal-spawn safety and command-injection timing for Codex startup - Sequential
[x] 10. Prepare release readiness notes for the second AMC macOS test - Sequential

## Completion Notes
- Run 12 validated 10 tasks, 10 generated agents, and a 5 Claude / 5 Codex session split.
- macOS Terminal wet launch opened 10 Terminal windows.
- Launch-time task delivery created normal squad tasks for ready assignments; the slow Codex role exposed a startup race that was fixed.
- Codex Terminal injection delay is now 20 seconds and the launch task-delivery wait is now 180 seconds.
- On macOS, autopilot launch defaults to Terminal.app; tmux remains available via `--terminal-backend tmux` or `--tmux-session`.

## Acceptance Criteria
- `squad autopilot plan ./amc-test-prd-2.md` reports exactly 10 tasks.
- `squad autopilot run ./amc-test-prd-2.md` creates a new run with 10 generated agents and 10 tasks.
- `squad autopilot launch --run-id <id> --terminal-backend macos-terminal` reports 10 sessions.
- The wet launch opens macOS Terminal windows and reports autopilot task delivery.
- The launch plan contains exactly 5 Claude sessions and 5 Codex sessions.
- The default macOS launch backend is Terminal.app.

## Test Requirements
- Run the autopilot plan command against this PRD.
- Run the autopilot run command against this PRD.
- Run the macOS Terminal launch command with `--execute`.
- Run `squad task list` after launch to confirm visible queued work.

## Risky Areas
- Terminal spawning must require `--execute`.
- Codex startup may be slower than command injection.
- Repeated runs may create suffixed agent IDs that task delivery must resolve.
- Existing uncommitted changes must remain intact.

## Dependencies
6 depends on 1, 2, 3, 4, 5
7 depends on 6
8 depends on 7
9 depends on 8
10 depends on 7, 8, 9
