# RLMF vLLM/Qwen Judge Task

## Assignment
Build or update vLLM and Qwen judge integration for RLMF scoring, structured judge decisions, calibration reports, and deterministic fixture parsing.

## Scope
- Judge request and response schemas.
- vLLM server profile/health integration.
- Qwen structured output validation.
- Judge report hashes and calibration benchmark outputs.

## Acceptance
- Request and response parsing handles valid responses, malformed responses, and missing fields.
- Judge decisions include stable hashes and no raw private trace content.
- Fixture mode is deterministic and does not require a live judge server.
- Worker report lists changed files, inspected files, tests run, and residual risks.

## Output
- Summary of judge behavior.
- Example decision/report hash.
- Test command output summary.
- Risks and follow-up tasks.

## Verification
- Run focused judge parser and structured-output tests.
- Run API integration tests if Fractalwork routes change.
