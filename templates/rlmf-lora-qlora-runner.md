# RLMF LoRA/QLoRA Runner Task

## Assignment
Build or update RLMF LoRA/QLoRA runner support, including training manifests, memory profile selection, resumable job state, checkpoint hashes, and deterministic dry-run behavior.

## Scope
- DataEvol local training and RLMF runner code.
- MLX LoRA and QLoRA manifest generation.
- Job status fields: job id, process id, checkpoint path, last metric, logs, failure reason, and artifact hashes.

## Acceptance
- Dry-run mode works without downloading models or requiring GPUs.
- Manifest generation is deterministic and includes dataset, runtime, checkpoint, and replay hashes.
- Memory profile selection is tested for at least low, medium, and high profile inputs.
- Worker report lists changed files, inspected files, tests run, and residual risks.

## Output
- Summary of runner behavior.
- Example manifest or fixture hash.
- Test command output summary.
- Risks and follow-up tasks.

## Verification
- Run focused runner/manifest tests.
- Run full relevant pytest when shared training code changes.
