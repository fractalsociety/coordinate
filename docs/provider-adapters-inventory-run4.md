# Provider Adapters — Inventory (Task 390, Run 4)

> **Autopilot-Task-ID:** 390 · **Autopilot-Run-ID:** 4
> **Owner:** scientific_planner-4 · **Role:** scientific_planner · **Client:** claude
> **Scope:** Identify the existing provider/model adapters in the `squad` codebase.
> **Provenance:** HEAD = `8146bcc`; working tree is **dirty AND still actively mutating** — other
> swarm agents are editing `src/autopilot.rs` live. Direct evidence: between two of this author's
> own reads, **9 lines were inserted above `provider_tool_command`**, shifting `provider_tool_command`
> `2838→2847`, `role_specific_injection_text` `2860→2869`, `CostRateLimitEstimate` `3284→3293`, and
> `estimate_cost_and_rate_limit` `3295→3304` (snapshot taken **2026-06-29**). Therefore:
> **cite symbol names as authoritative; line numbers are a point-in-time snapshot and WILL drift.**
> Re-resolve by symbol (`grep -n 'pub fn <name>'`) before relying on any line number.

This is a **fresh, independent re-derivation** read directly from source — not transcribed from
run-2 (`docs/provider-adapters-inventory.md`, task 134) or run-3 (`docs/provider-adapters-inventory-run3.md`,
task 262). It **confirms run-3's core findings and corrects one** (backoff logic now exists — see §7/§8).

## 1. Executive summary

One canonical provider abstraction — the `ModelProvider` enum (`src/autopilot.rs`,
`pub enum ModelProvider`) with **7 variants** — adapted to concrete behavior through **three
adapter layers**, plus a set of supporting adapters:

1. **Launch/CLI adapter** — `provider_tool_command()` maps a `ModelProvider` to a `(program, args)`
   shell command.
2. **Prompt-injection adapter** — `role_specific_injection_text()` maps a provider to the join
   instruction auto-typed into a spawned pane.
3. **Install/slash-command adapter** — `PLATFORMS` (`src/setup.rs`, `pub const PLATFORMS`) maps
   detected client binaries to in-repo slash-command template files.

Supporting adapters: a capability **rank** (`model_provider_rank`, `src/store.rs`), the `--client`
join **validator** (`src/main.rs`), **model-mix defaults + role overrides**, a per-provider
**launch delay**, a **cost/rate-limit estimator**, a **retry/backoff schedule**, plus **offline
local-model helpers** and online **router helpers**.

**Key distinction (unchanged across runs):** all 7 `ModelProvider` variants exist for **autopilot
routing/planning**, but only **4** (`claude`, `codex`, `gemini`, `opencode`) are valid interactive
join clients (`squad join --client`). `openrouter_free`, `openrouter_cheap`, and `local` are
**autopilot-only routing tiers** realized by reusing other binaries (opencode / zsh) — not
first-class clients.

## 2. The `ModelProvider` enum (the abstraction)

`src/autopilot.rs` — `#[serde(rename_all = "lowercase")]`, 7 variants:

| Variant | `as_str()` | Serde aliases | Notes |
|---|---|---|---|
| `Claude` | `claude` | — | frontier; default for manager/planner/critic roles |
| `Codex` | `codex` | — | frontier; default for coding/verification/router roles |
| `Gemini` | `gemini` | — | disabled in default mix (0.00) |
| `OpenCode` | `opencode` | — | reused as the proxy host for OpenRouter tiers + `Local` join |
| `OpenRouterFree` | `openrouter_free` | `openrouter-free`, `openrouterfree` | routing tier only; proxied via opencode |
| `OpenRouterCheap` | `openrouter_cheap` | `openrouter-cheap`, `openroutercheap` | routing tier only; proxied via opencode |
| `Local` | `local` | — | routing tier only; realized as a bare `zsh` shell |

Round-tripping: `FromStr` (`impl FromStr for ModelProvider`), `Display` (delegates to `as_str`),
and `as_str()` all live in the same `impl`/`enum` block. **Directly re-read this run:** all 7 arms
present in `as_str()` and `from_str()` (`claude`, `codex`, `gemini`, `opencode`, `openrouter_free`,
`openrouter_cheap`, `local`).

## 3. Adapter layer 1 — Launch/CLI command (`provider_tool_command`)

`pub fn provider_tool_command(provider) -> ProviderToolCommand` (snapshot `src/autopilot.rs:~2847`).
Returns `ProviderToolCommand { program, args }` (struct `pub struct ProviderToolCommand`) with
builder `shell_command()`.

| `ModelProvider` | program | args | Notes |
|---|---|---|---|
| `Claude` | `claude` | `--dangerously-skip-permissions` | full autonomy flag |
| `Codex` | `codex` | `--yolo` | full autonomy flag |
| `Gemini` | `gemini` | *(none)* | |
| `OpenCode` | `opencode` | *(none)* | |
| `OpenRouterFree` | `opencode` | `--model openrouter/free` | **proxied via opencode**; no native OpenRouter client |
| `OpenRouterCheap` | `opencode` | `--model openrouter/cheap` | **proxied via opencode** |
| `Local` | `zsh` | *(none)* | a raw local shell, not an AI client |

Consumed by `terminal_session_plan_for_role()` (populates `TerminalSessionPlan.command` and
`.provider_tool`). **Confirmed verbatim this run.**

## 4. Adapter layer 2 — Prompt injection syntax (`role_specific_injection_text`)

`pub fn role_specific_injection_text(provider, role_id, agent_id)` (snapshot `src/autopilot.rs:~2869`).

| Provider | Injection text |
|---|---|
| `Codex` | `$squad {role_id} {agent_id}` (Codex Skills trigger) |
| `Local` | `squad join {agent_id} --role {role_id} --client opencode --protocol-version 2 && squad receive {agent_id} --wait` |
| all others (`Claude`, `Gemini`, `OpenCode`, `OpenRouter*`) | `/squad {role_id} {agent_id}` (slash command) |

`Local` joins using `--client opencode`, so the local tier reuses the opencode client. **Confirmed
verbatim this run.**

## 5. Adapter layer 3 — Install/slash-command templates (`PLATFORMS`)

`pub const PLATFORMS: &[Platform]` in `src/setup.rs` (snapshot `:14`). Installs one squad-join
template per *detected* client binary. Four entries:

| client | install path (under `$HOME`) | template content const | format |
|---|---|---|---|
| `claude` | `.claude/commands/squad.md` | `SQUAD_MD_CONTENT` | Markdown, `$ARGUMENTS` |
| `gemini` | `.gemini/commands/squad.toml` | `SQUAD_TOML_CONTENT` | TOML, `{{args}}` |
| `codex` | `.codex/skills/squad/SKILL.md` | `SQUAD_CODEX_CONTENT` | Skills MD, `$ARGUMENTS` |
| `opencode` | `.config/opencode/commands/squad.md` | `SQUAD_MD_CONTENT` | Markdown (reuses Claude template) |

Lifecycle helpers in `src/setup.rs`: `run_setup`, `check_and_update_commands`, `cleanup_commands`,
`detect_platforms`, `diagnose_templates_for_platforms`. **Confirmed this run** (`PLATFORMS` at `:14`,
templates at `:42`/`:108`/`:173`).

## 6. Supporting adapters

- **Capability rank** — `fn model_provider_rank(provider: &str) -> i64` (`src/store.rs`, snapshot
  `~1935`): `local=1`, `openrouter_free=2`, `openrouter_cheap|gemini|opencode=3`, `codex=4`,
  `claude=5`, unknown=`0`. Drives model-mix selection / escalation ordering. **Confirmed verbatim.**
- **`--client` validator** (`src/main.rs`, snapshot `~164`): `squad join --client` accepts only
  `claude | gemini | codex | opencode`; any other value bails (`invalid --client value`).
  Confirms `openrouter_free` / `openrouter_cheap` / `local` are **not** join clients. **Confirmed
  verbatim.**
- **Model-mix defaults** — `ModelMix::default` (`src/autopilot.rs`, snapshot `~117`) realized via
  `default_model_mix_*` fns (snapshot `~135-148`) =
  **`claude=0.50, codex=0.50, gemini=0.00, openrouter_free=0.00, openrouter_cheap=0.00, local=0.00`.**
  **Only Claude and Codex are active by default.** Confirmed this run by reading every
  `default_model_mix_*` body.
- **Role→provider overrides** — emitted by `default_autopilot_config_content()` as a full
  `[role_overrides]` map: `claude` for manager / scientific_planner / protocol_designer /
  literature_worker / hypothesis_worker / adversarial_critic / safety_gatekeeper / inspector /
  architect / security_reviewer / docs; `codex` for tool_mapper / coding_worker / verification_worker
  / trace_collector / router / compressor / rust_backend / sqlite_engineer / terminal_tmux /
  test_engineer / test_worker / release_engineer. Applied via
  `AutopilotConfig::provider_for_role` (`self.role_overrides.get(role).unwrap_or(default_provider)`).
  **Confirmed this run.**
- **Per-provider launch delay** — `fn provider_prompt_delay_seconds(provider)` (snapshot
  `~2784`): `Codex=45s`, `Claude=8s`, else `1s`; overridable via `SQUAD_AUTOPILOT_PROMPT_DELAY_SECS`.
  Used by both the tmux injector and the macOS Terminal path. **Confirmed verbatim.**
- **Cost / rate-limit estimator** — `pub struct CostRateLimitEstimate` +
  `pub fn estimate_cost_and_rate_limit(provider)` (snapshot `~3293`/`~3304`). Static per-provider
  estimate, **read directly this run**:
  - `Local` → tier `Free`, relative_cost `1`, rate `None`
  - `OpenRouterFree` → tier `Free`, relative_cost `1`, rate **`Some(20)` /min**
  - `OpenRouterCheap` → tier `Cheap`, relative_cost `3`, rate **`Some(60)` /min**
  - `Codex` → tier `Cheap`, relative_cost `5`, rate `None`
  - `Gemini`/`OpenCode` → tier `Premium`, relative_cost `7`, rate `None`
  - `Claude` → tier `Premium`, relative_cost `8`, rate `None`
- **Retry / backoff schedule (task 74)** — `pub fn retry_backoff_delays_seconds(max_attempts,
  base_seconds, cap_seconds) -> Vec<u64>` + `next_retry_delay_seconds(...)` (snapshot `~3355/3367`,
  under `// ---------- Retry / backoff (task 74) ----------`). Generates an exponential, capped
  backoff schedule. *(Run-3 listed retry/backoff as "still absent" — **corrected here**: the
  schedule helper now exists.)*
- **Offline local-model helpers (tasks 82–84)** — under `// ---------- Local-model helpers (tasks
  82, 83, 84) ----------` (snapshot `~3377`): `detect_duplicate_work(tasks, threshold)` (task 82)
  + an offline task-difficulty classifier (task 83) + a memory curator (task 84). Described as the
  only work a local tier may do ("must not finalize scientific claims").
- **Online router helpers** — `estimate_task_difficulty()` (task 35, snapshot `~3170`) and
  `recommend_provider_tier(band)` (snapshot `~3234`).

## 7. Observations / gaps (current)

1. **Still no native OpenRouter adapter.** `OpenRouterFree`/`OpenRouterCheap` remain thin shims over
   the `opencode` binary with a `--model` flag (`provider_tool_command`). A *static* rate-limit
   estimate exists (`estimate_cost_and_rate_limit`: 20/min free, 60/min cheap). What is **still
   absent** is any **dynamic** per-model rate-limit **tracker**, a provider **pool**, and **wiring**
   of the backoff schedule into an actual retry loop (the `retry_backoff_delays_seconds` helper
   exists but no consumer was found in this pass — grep showed only the comment + the fn).
2. **`Local` is still a bare `zsh` shell**, not an AI model. Offline local helpers exist (tasks
   82–84). **Still absent:** a local-model **install** path (no `ollama`/`llama` symbols anywhere in
   `src/`) and wiring a real local LLM into the router. So "local model support" is partially
   scaffolded (deterministic helpers) but not realized as an actual local LLM.
3. **Default mix is Claude/Codex 50/50**; `gemini`, `openrouter_free`, `openrouter_cheap`, `local`
   are all `0.00` by default.
4. **Template reuse:** `opencode` reuses the Claude markdown template (`SQUAD_MD_CONTENT`), so the
   two are not independently customizable.
5. **Flat enum, no trait abstraction.** `ProviderToolCommand` is a plain struct; adding a new
   provider still requires editing multiple `match` arms (`FromStr`, `as_str`,
   `provider_tool_command`, `role_specific_injection_text`, `model_provider_rank`,
   `estimate_cost_and_rate_limit`) plus the join validator if it should be a first-class client.
   Risk of drift across arms.

## 8. Delta vs prior inventories

| Item | Run-2 (task 134) | Run-3 (task 262) | **Run-4 (task 390, this)** |
|---|---|---|---|
| Default model mix | claude=.15, codex=.15, or_free=.50, or_cheap=.10, local=.10 | claude=.50, codex=.50, rest .00 | **claude=.50, codex=.50, rest .00** (re-confirmed) |
| Cost/rate-limit estimator | "does not exist" | exists (static) | **exists (static); values re-read**: or_free=20/min, or_cheap=60/min |
| Retry/backoff (task 74) | n/a | **"still absent"** | **EXISTS** — `retry_backoff_delays_seconds` (+`next_retry_delay_seconds`); no consumer found yet |
| Local helpers (82–84) | "not implemented" | exist | **exist** (re-confirmed) |
| Launch-delay fn name | `macos_terminal_inject_delay_seconds` | `provider_prompt_delay_seconds` (+env override) | **same as run-3** (re-confirmed) |
| Line numbers | accurate for old tree | snapshot | **snapshot; drifted +9 during this very investigation** |

Net since run-3: the only substantive change observed is the **appearance of the backoff schedule
helper (task 74)**; everything else is stable. The repo posture remains **shipped-default
Claude/Codex 50/50** with OpenRouter/local features statically scaffolded but not dynamically wired.

## 9. Independent verification requirement

Per the task's acceptance criteria, **this inventory must be independently verified before it is
relied upon.** Recommended independent checks — assignable to `inspector` / `verification_worker`
(an agent **other than this author**):

- **V1 (mechanical):** For each of the 7 `ModelProvider` variants, confirm the `program`/`args` in
  `provider_tool_command` matches §3 — by reading the function directly, *not* this report.
- **V2 (behavioral):** Confirm `provider_tool_command(...).shell_command()` and
  `role_specific_injection_text(...)` via the existing tests
  `test_provider_tool_command_maps_supported_provider_tools` and
  `test_role_specific_injection_text_uses_provider_prompt_style` in
  `tests/autopilot_terminal_session_test.rs` (locate by test name — line numbers drift). Run
  `cargo test --test autopilot_terminal_session_test`.
- **V3 (default mix):** Confirm `default_model_mix_*` values are `claude=0.50 / codex=0.50 /
  rest=0.00`, and that `test_default_autopilot_config_uses_claude_codex_50_50_mix`
  (`tests/autopilot_config_test.rs`) and `fresh_init_autopilot_pipeline_meets_release_block_criteria`
  (`tests/autopilot_release_block_test.rs`) agree. Run
  `cargo test --test autopilot_config_test --test autopilot_release_block_test`.
- **V4 (cost + backoff, NEW vs run-3):** Confirm `estimate_cost_and_rate_limit` returns
  `OpenRouterFree=20/min`, `OpenRouterCheap=60/min`, `Local=None`; and that
  `retry_backoff_delays_seconds` exists (task 74). Grep for a **dynamic** rate-limit **tracker** /
  provider **pool** / a **consumer** of the backoff schedule (expected: absent — only the static
  estimator and the schedule helper exist).
- **V5 (rank + join gate):** Confirm `model_provider_rank` ordering (`src/store.rs`) and that the
  `--client` validator (`src/main.rs`) restricts joins to `claude|gemini|codex|opencode`.

## 10. Acceptance-criteria status

- ☑ **Task output recorded with provenance** — this file (`docs/provider-adapters-inventory-run4.md`);
  every claim cites a symbol + snapshot line + HEAD `8146bcc`; the live-tree caveat (+9 drift during
  the investigation) is stated up front.
- ☐ **Task result has an independent verification requirement** — **defined** in §9 (V1–V5);
  **not executed** by this agent, because the author must not be the verifier. Requires an
  independent agent (`inspector` / `verification_worker`) to run V1–V5 against the then-current tree.

## 11. Changed files

- `docs/provider-adapters-inventory-run4.md` (new — this file).

## 12. Tests run

None. This is an identification/investigation task; no production code changed. Mechanical and
behavioral confirmation is deferred to the independent verifier (§9). The author intentionally did
not execute the test suite, to preserve the independence of §9 (per acceptance criteria).
