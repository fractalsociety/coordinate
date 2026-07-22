# RLMF API/Dashboard Integration Task

## Assignment
Build or update Fractalwork API and dashboard integration for RLMF jobs, runs, judge servers, benchmark reports, promotion decisions, receipts, and status surfaces.

## Scope
- Fractalwork API controllers, services, route registry, and tests.
- Dashboard API routes and user-facing RLMF pages.
- Keep backend state and dashboard displays consistent with DataEvol and chain/indexer records.

## Acceptance
- API routes return sanitized, stable payloads with no secret values.
- Dashboard routes handle healthy, missing, and unreachable backend states.
- Tests cover create/list/get/status paths touched by the task.
- Worker report lists changed files, inspected files, tests run, and residual risks.

## Output
- Summary of API/dashboard behavior.
- Route list or screenshot path when UI changes.
- Test command output summary.
- Risks and follow-up tasks.

## Verification
- Run focused API or dashboard tests.
- Run `npm run build` when TypeScript surface changes.
