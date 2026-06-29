# AMC Test PRD

## Product Goals
- Validate an autopilot multi-coding run that can ingest a PRD, create a task graph, generate agents, and plan terminal sessions.
- Use an even Claude/Codex model mix for generated worker roles.

## Milestones
- MVP 1: PRD ingestion and task graph validation.
- MVP 2: Agent generation and session planning.
- MVP 3: Acceptance reporting.

## Implementation Task Checklist
[x] 1. Confirm the autopilot config loads a 50/50 Claude/Codex model mix - Parallel
[x] 2. Parse this PRD into exactly ten implementation tasks - Parallel
[x] 3. Preserve product goals, milestones, acceptance criteria, tests, and risks - Parallel
[x] 4. Generate specialized worker roles from the PRD context - Parallel
[x] 5. Apply the model mix to generated worker roles - Parallel
[x] 6. Persist the autopilot run, agents, tasks, and terminal sessions - Sequential
[x] 7. Assign ready parallel tasks to available generated agents - Sequential
[x] 8. Produce a dry launch plan without opening terminal windows - Sequential
[x] 9. Record the initialized test checkpoint and changed-file summary - Sequential
[x] 10. Write the final autopilot report and release readiness notes with the model mix and unresolved risks - Sequential

## Completion Notes
- Run 2 validated 10 tasks, 10 generated agents, and a 5 Claude / 5 Codex session split.
- macOS Terminal wet launch opened real Terminal windows.
- Ready autopilot assignments were bridged into normal squad task messages so workers had work in their inboxes.
- Codex startup handling was increased to avoid missed `$squad` command injection.

## Acceptance Criteria
- `squad autopilot plan ./amc-test-prd.md` reports exactly 10 tasks.
- `squad autopilot run ./amc-test-prd.md` creates one run with generated agents and tasks.
- `squad autopilot launch --run-id <id>` prints a dry launch plan.
- The final report includes Claude 50% and Codex 50%, with Gemini and local disabled.
- The launch plan contains exactly 10 sessions: 5 Claude and 5 Codex.
- On macOS, autopilot launch defaults to Terminal.app unless a tmux option/backend is explicitly provided.

## Test Requirements
- Run the autopilot plan command against this PRD.
- Run the autopilot run command against this PRD.
- Run the autopilot launch command without `--execute`.

## Risky Areas
- Terminal spawning should remain dry-run unless explicitly executed.
- Existing uncommitted user changes must not be reverted.
- The generated task graph must not collapse or duplicate checklist items.

## Dependencies
6 depends on 1, 2, 3, 4, 5
7 depends on 6
8 depends on 6
9 depends on 6
10 depends on 7, 8, 9
