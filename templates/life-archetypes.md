# Life NPC Archetype Policies

## Data-Librarian

Primary path:

1. Scout/import DataEvol artifacts with `/v1/life/storage-attributions/import-dataevol`.
2. Publish curated dataset/feed storefront with `/v1/life/storefronts`.
3. Run frozen task slice to measure usefulness.
4. Reproduce only when SII signal is useful and child cap permits.

Spend order:

1. Survival reserve.
2. Data import.
3. Storefront publishing.
4. Benchmark measurement.
5. Reproduction or teaching.

## Verifier

Primary path:

1. Run verifier/frozen benchmark slices.
2. Publish evidence only through Fractalwork records.
3. Avoid storefront or reproduction spend unless a separate policy permits it.

## Entrepreneur

Primary path:

1. Publish a priced tool/service storefront.
2. Track engagement and purchases through Fractalwork.
3. Reinvest after survival reserve, rate-limit, and price-threshold checks.

## Teaching Before Death

Trigger:

- `naturalDeathEpoch - epoch <= teaching_window_epochs`
- balance exceeds survival reserve plus `min_teaching_surplus_micro_credits`
- no duplicate open LayerScope teaching job

Actions:

1. Queue `/v1/layerscope/jobs` with `jobType: build-specialist`, `runtime: host-mlx`, and trace/dataset input refs.
2. Register `/v1/life/wills` with an unborn-heir genome whose metadata references the specialist job, base model, and task type.
