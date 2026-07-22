# RLMF Dataset Builder Task

## Assignment
Build or update DataEvol RLMF dataset preparation work for trace-to-training conversion, privacy gating, manifest hashing, and deterministic fixture export.

## Scope
- FractalDataevol RLMF dataset modules and tests.
- Synthetic fixture traces, privacy/export guards, manifest hashes, and dataset receipts.
- Keep raw secrets, API keys, and private user paths out of outputs.

## Acceptance
- Dataset output is deterministic for the same input traces.
- Privacy/export rules are checked before any training manifest can be emitted.
- Manifest hashes are stable and covered by tests.
- Worker report lists changed files, inspected files, tests run, and residual risks.

## Output
- Summary of implementation.
- Dataset/hash examples or fixture paths.
- Test command output summary.
- Risks and follow-up tasks.

## Verification
- Run the narrow DataEvol test for the changed dataset path.
- Run full relevant pytest when the task touches shared RLMF code.
