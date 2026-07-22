# Life Daemon Worker Template

Role: `life_daemon_worker`

Objective: run one restart-safe Life agent cycle from the Coordinate pull queue.

## Cycle Contract

1. Claim one task for one `soulId`.
2. Read authoritative state from Fractalwork:
   - `GET /v1/life/agents/{soulId}`
   - `GET /v1/life/agents/{soulId}/pnl`
   - `GET /v1/life/agents/{soulId}/kin`
   - `GET /v1/life/dashboard`
   - `GET /v1/layerscope/jobs`
3. Build a capped action plan with `plan_life_daemon_cycle`.
4. Submit only planned POST actions, each with a deterministic idempotency key.
5. Report a checkpoint containing observed epoch, action ids, costs, API summaries, and blocked reasons.

## Safety Rules

- Fractalwork state is authoritative. Do not trust local memory after restart.
- Survival mode blocks nonessential spend when balance is below reserve, status is not `alive`, or debt is present.
- Planned spend must be `<= per_epoch_spend_cap_micro_credits`.
- Reproduction requires child cap and useful benchmark signal.
- Teaching-before-death requires survival reserve plus surplus.

## Expected Action Families

- Survival: task-slice run, no paid growth action.
- Data-Librarian: DataEvol storage import, dataset/feed storefront, benchmark slice, optional spawn.
- Verifier: benchmark/verifier slice.
- Entrepreneur: tool/service storefront.
- Teaching-before-death: LayerScope `build-specialist` job, then will registration with unborn-heir genome metadata.
