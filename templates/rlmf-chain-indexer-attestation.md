# RLMF Chain/Indexer Attestation Task

## Assignment
Build or update RLMF chain commitment, attestation submission, indexer projection, lineage, and replay verification work.

## Scope
- Fractalwork commitment/projection/replay helpers.
- FractalChain/fractalchain2 attestation stubs or adapters.
- Chain/indexer records for dataset, job, judge report, benchmark report, model artifact, promotion decision, proof-of-payment, and provenance hashes.

## Acceptance
- Commitment and attestation hashes are deterministic.
- Indexed rows can be replay-verified from local artifacts.
- Chain stubs validate bad hashes and return stable local references.
- Worker report lists changed files, inspected files, tests run, and residual risks.

## Output
- Summary of commitment/indexer behavior.
- Example commitment hash and chain reference.
- Test command output summary.
- Risks and follow-up tasks.

## Verification
- Run focused Fractalwork projection/replay tests.
- Run focused FractalChain/fractalchain2 attestation tests when Rust chain code changes.
