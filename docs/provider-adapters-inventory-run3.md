# Provider Adapters — Inventory (Task 262, Run 3)

> **Autopilot-Task-ID:** 262 · **Autopilot-Run-ID:** 3
> **Owner:** scientific_planner-3 · **Role:** scientific_planner
> **Scope:** Identify the existing provider/model adapters in the `squad` codebase.
> **Provenance:** HEAD = `8146bcc`; working tree is **dirty (28 uncommitted files) AND concurrently
> mutating** — other swarm agents are editing files live (observed: a test was renamed and its
> assertions updated, and several line numbers shifted by ~5 lines, *during* this investigation).
> Therefore: **cite symbol names as authoritative; line numbers are a point-in-time snapshot taken
> 2026-06-29 and WILL drift.** Re-resolve by symbol before relying on any line number.

> **⚠ CORRECTION (2026-06-29, superseded by run-4 / Task 390, owner scientific_planner-4;
> independently re-verified by scientific_planner-3):** the §7.1 claim that retry/backoff logic is
> "still absent" is **WRONG as of the current tree** — `retry_backoff_delays_seconds` (task 74) now
> EXISTS (`src/autopilot.rs`, snapshot `:3384`; helpers `:3370-3383`; exponential-backoff delay +
> full schedule). Caveat: **no consumer is wired yet** — it is an un-built-in helper, not an active
> retry loop, so the *dynamic tracker / pool* part of the OpenRouter gap (§7.1) still holds. The
> remaining §7 gap claims (no native OpenRouter client, no dynamic rate-limit *tracker*/*pool*) are
> unaffected. See `docs/provider-adapters-inventory-run4.md` for the corrected, expanded inventory.
> *(Original §7.1 wording below is retained for provenance — read its "retry/backoff" mention as
> superseded by this note.)*

This is a **fresh, independent re-derivation** against the current tree. It supersedes the run-2
inventory (`docs/provider-adapters-inventory.md`, task 134) where the two disagree; see §8 for the
delta. Every claim below was read directly from source, not transcribed from run-2.

## 1. Executive summary

The codebase has **one canonical provider abstraction** — the `ModelProvider` enum
(`src/autopilot.rs`, `pub enum ModelProvider`) with **7 variants** — adapted to concrete behavior
through **three adapter layers**:

1. **Launch/CLI adapter** — `provider_tool_command()` maps a `ModelProvider` to a
   `(program, args)` shell command.
2. **Prompt-injection adapter** — `role_specific_injection_text()` maps a provider to the join
   instruction auto-typed into a spawned pane.
3. **Install/slash-command adapter** — `PLATFORMS` (`src/setup.rs`, `pub const PLATFORMS`) maps
   detected client binaries to in-repo slash-command template files.

Plus supporting adapters: a capability **rank** (`model_provider_rank`, `src/store.rs`), the
`--client` join **validator** (`src/main.rs`), **model-mix defaults + role overrides**, a per-provider
**launch delay**, and — **new since run-2** — a **cost/rate-limit estimator** and a set of
**offline local-model helpers**.

**Key distinction (unchanged):** all 7 `ModelProvider` variants exist for **autopilot routing/planning**,
but only **4** (`claude`, `codex`, `gemini`, `opencode`) are valid interactive join clients
(`squad join --client`). `openrouter_free`, `openrouter_cheap`, and `local` are **autopilot-only
routing tiers** realized by reusing other binaries (opencode / zsh) — they are **not** first-class
clients.

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
and `as_str()` all live in the same `impl`/`enum` block.

## 3. Adapter layer 1 — Launch/CLI command (`provider_tool_command`)

`pub fn provider_tool_command(provider) -> ProviderToolCommand` (snapshot `src/autopilot.rs:~2838`).
Returns `ProviderToolCommand { program, args }` (struct `pub struct ProviderToolCommand`) with builder
`shell_command()`.

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
`.provider_tool`).

## 4. Adapter layer 2 — Prompt injection syntax (`role_specific_injection_text`)

`pub fn role_specific_injection_text(provider, role_id, agent_id)` (snapshot `src/autopilot.rs:~2860`).
Text auto-typed into a spawned pane to make the agent join:

| Provider | Injection text |
|---|---|
| `Codex` | `$squad {role_id} {agent_id}` (Codex Skills trigger) |
| `Local` | `squad join {agent_id} --role {role_id} --client opencode --protocol-version 2 && squad receive {agent_id} --wait` |
| all others (`Claude`, `Gemini`, `OpenCode`, `OpenRouter*`) | `/squad {role_id} {agent_id}` (slash command) |

Note: `Local` joins using `--client opencode`, so the local tier reuses the opencode client.

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
`detect_platforms`, `diagnose_templates_for_platforms`.

## 6. Supporting adapters

- **Capability rank** — `fn model_provider_rank(provider: &str) -> i64` (`src/store.rs:~1935`):
  `local=1`, `openrouter_free=2`, `openrouter_cheap|gemini|opencode=3`, `codex=4`, `claude=5`,
  unknown=`0`. Drives model-mix selection / escalation ordering.
- **`--client` validator** (`src/main.rs:~164`): `squad join --client` accepts only
  `claude | gemini | codex | opencode`; any other value bails. Confirms `openrouter_free` /
  `openrouter_cheap` / `local` are **not** join clients.
- **Model-mix defaults** — `ModelMix::default` (`src/autopilot.rs:~117`) realized via
  `default_model_mix_*` fns (`:~130-148`) = **`claude=0.50, codex=0.50, gemini=0.00,
  openrouter_free=0.00, openrouter_cheap=0.00, local=0.00`.** Only Claude and Codex are active by
  default. *(This is the dominant drift vs run-2 — see §8.)*
- **Role→provider overrides** — `default_autopilot_config_content()` (`src/autopilot.rs:~250`) emits a
  full `[role_overrides]` map: `claude` for manager / scientific_planner / protocol_designer /
  literature_worker / hypothesis_worker / adversarial_critic / safety_gatekeeper / inspector /
  architect / security_reviewer / docs; `codex` for tool_mapper / coding_worker / verification_worker /
  trace_collector / router / compressor / rust_backend / sqlite_engineer / terminal_tmux /
  test_engineer / test_worker / release_engineer. Applied via `AutopilotConfig::provider_for_role`
  (`:~92`).
- **Per-provider launch delay** — `fn provider_prompt_delay_seconds(provider)` (`src/autopilot.rs:~2784`):
  `Codex=45s`, `Claude=8s`, else `1s`. Used by **both** the tmux injector and the macOS Terminal
  AppleScript (not macOS-only, contrary to the run-2 note).
- **NEW — Cost / rate-limit estimator** — `pub struct CostRateLimitEstimate` + `pub fn
  estimate_cost_and_rate_limit(provider)` (`src/autopilot.rs:~3284`/`:~3295`). Static per-provider
  estimate: `OpenRouterFree` → rate `20/min`; `OpenRouterCheap` → rate `60/min`; `Local` → free, no
  rate limit; `Codex`/`Claude`/`Gemini`/`OpenCode` → no rate limit. *(Did not exist in run-2.)*
- **NEW — Offline local helpers** (`src/autopilot.rs:~3368`, "Local-model helpers (tasks 82, 83, 84)"):
  `detect_duplicate_work()` (task 82, token-overlap duplicate detection), an offline task-difficulty
  classifier (task 83, rationale-only, "safe for a local model"), and a memory curator (task 84).
  These are described as the *only* work a local tier may do ("must not finalize scientific claims").
  Related online router helpers: `estimate_task_difficulty()` (task 35, `:~3161`) and a routing-tier
  recommender (`:~3223`). *(Did not exist in run-2.)*

## 7. Observations / gaps (current)

1. **Still no native OpenRouter adapter.** `OpenRouterFree`/`OpenRouterCheap` remain thin shims over
   the `opencode` binary with a `--model` flag (`provider_tool_command`). What is **new** is a *static*
   rate-limit estimate (`estimate_cost_and_rate_limit`). What is **still absent** is any *dynamic*
   per-model rate-limit **tracker**, a provider **pool**, or ~~retry/backoff logic~~ (planned tasks
   73-74): only static estimates exist. *(Correction: a `retry_backoff_delays_seconds` helper (task 74)
   now EXISTS but is unwired — see top-of-file correction note. A dynamic tracker/pool + an active
   retry loop are still absent.)*
2. **`Local` is still a bare `zsh` shell** (`provider_tool_command`), not an AI model. **New:** offline
   local helpers exist (tasks 82-84). **Still absent:** a local-model **install** path (task 79) and
   wiring a real local model into the router (task 85). So "local model support" is partially
   scaffolded (deterministic helpers) but not realized as an actual local LLM.
3. **`Gemini` disabled by default** in the model mix (`gemini: 0.00`) — consistent with run-2.
4. **Template reuse:** `opencode` reuses the Claude markdown template (`SQUAD_MD_CONTENT`), so the two
   are not independently customizable.
5. **Flat enum, no trait abstraction.** `ProviderToolCommand` is a plain struct; adding a new provider
   still requires editing multiple `match` arms (`FromStr`, `as_str`, `provider_tool_command`,
  `role_specific_injection_text`, `model_provider_rank`, `estimate_cost_and_rate_limit`) plus the join
  validator if it should be a first-class client. Risk of drift across arms.

## 8. Delta vs run-2 inventory (`docs/provider-adapters-inventory.md`, task 134)

The run-2 inventory is **structurally accurate but stale on defaults and on two "gap" claims**:

| Item | Run-2 (task 134) | Run-3 (task 262, current tree) |
|---|---|---|
| Default model mix | `claude=.15, codex=.15, openrouter_free=.50, openrouter_cheap=.10, local=.10` | **`claude=.50, codex=.50`, rest `.00`** |
| Role overrides (default) | literature_worker=OpenRouterFree, test_worker=OpenRouterCheap, trace_collector=Local | **literature_worker=Claude, test_worker=Codex, trace_collector=Codex** (full claude/codex split) |
| Cost/rate-limit estimator | "does not exist yet" | **EXISTS** — `estimate_cost_and_rate_limit` (static) |
| Local helpers (tasks 82-84) | "not implemented" | **EXIST** — `detect_duplicate_work` + offline classifier + memory curator |
| Launch-delay fn name | `macos_terminal_inject_delay_seconds` (macOS-only) | renamed `provider_prompt_delay_seconds`, used by **both** tmux and macOS |
| Line numbers | accurate for the older tree | shifted throughout (tree grew); cite by symbol |

Net: the project has moved from a **Science-Swarm-defaulted, OpenRouter-heavy** routing posture
(run-2) to a **shipped-default Claude/Codex 50/50** posture (run-3), while partially scaffolding the
OpenRouter/local features that run-2 listed as pure gaps.

> **Live-edit note:** while drafting, a concurrent agent renamed
> `test_default_autopilot_config_uses_science_swarm_model_mix` →
> `test_default_autopilot_config_uses_claude_codex_50_50_mix` and updated its assertions to match the
> new mix — i.e. the test suite is being reconciled to the run-3 contract in real time. This is why no
> "failing test" is reported here: by the time of this snapshot the default-mix tests are internally
> consistent. Any verification (§9) must re-confirm against the *then*-current tree.

## 9. Independent verification requirement

Per the task's acceptance criteria, **this inventory must be independently verified before it is
relied upon.** Recommended independent checks — assignable to `inspector` / `verification_worker`
(an agent **other than the author**):

- **V1 (mechanical):** For each of the 7 `ModelProvider` variants, confirm the `program`/`args` in
  `provider_tool_command` matches §3 — by reading the function directly, *not* this report.
- **V2 (behavioral):** Confirm `provider_tool_command(...).shell_command()` and
  `role_specific_injection_text(...)` via the existing tests
  `test_provider_tool_command_maps_supported_provider_tools` and
  `test_role_specific_injection_text_uses_provider_prompt_style` in
  `tests/autopilot_terminal_session_test.rs` (locate by test name — line numbers drift). Run
  `cargo test --test autopilot_terminal_session_test`.
- **V3 (default mix):** Confirm `default_autopilot_config_content` emits `claude=0.50 / codex=0.50 /
  rest=0.00` and that `test_default_autopilot_config_uses_claude_codex_50_50_mix`
  (`tests/autopilot_config_test.rs`) and `fresh_init_autopilot_pipeline_meets_release_block_criteria`
  (`tests/autopilot_release_block_test.rs`) agree. Run `cargo test --test autopilot_config_test --test
  autopilot_release_block_test`.
- **V4 (gap check):** Grep for a *dynamic* rate-limit tracker / provider pool / retry-backoff
  (`rate_limit`, `Pool`, `backoff`) and for a local-model install/wiring (`ollama`, `llama`,
  local install). Expected: only the **static** `estimate_cost_and_rate_limit` and the offline
  helpers; no dynamic tracker, no install/wiring.
- **V5 (rank + join gate):** Confirm `model_provider_rank` ordering (`src/store.rs`) and that the
  `--client` validator (`src/main.rs`) restricts joins to `claude|gemini|codex|opencode`.

## 10. Acceptance-criteria status

- ☑ **Task output recorded with provenance** — this file (`docs/provider-adapters-inventory-run3.md`);
  every claim cites a symbol + snapshot line + HEAD `8146bcc`; the live-tree caveat is stated up front.
- ☑ **Task result has an independent verification requirement** — **defined** in §9 (V1–V5) and
  **independently executed** on 2026-06-29 by `manager-4` (an agent other than the author) against the
  then-current tree: **V1–V5 ALL PASS**. Confirmed independently: no native OpenRouter client, no
  dynamic rate-limit tracker/pool, `Local` = bare `zsh`, default mix = Claude/Codex 50/50, rank
  ordering + `--client` join gate all as documented. (Full verifier detail: manager-4 → manager-5.)
  **Both acceptance criteria for task 262 are now satisfied.**

## 11. Changed files

- `docs/provider-adapters-inventory-run3.md` (new — this file).

## 12. Tests run

None. This is an identification/investigation task; no production code changed. Mechanical and
behavioral confirmation is deferred to the independent verifier (§9). (The author intentionally did
not execute the test suite, to preserve the independence of §9.)

## 13. Backlog follow-ups (endorsed by `manager-4`, flagged to `manager-5`)

Not in scope for task 262 — recorded here so future agents find them:

- **(A) Preserve the diversified science-swarm mix as an OPT-IN profile.** The shipped default is
  Claude/Codex-only by design (release-safe: OpenRouter/Local tiers are not production-ready, §7). To
  avoid losing the science-swarm provider-allocation design, expose the diversified mix
  (literature_worker→OpenRouterFree, trace_collector→Local, etc.) as a named preset/profile users opt
  into once OpenRouter + a local model are configured, and convert the diversified-mix test assertion
  to exercise that opt-in path rather than the fresh default.
- **(B) Document the dual-allocation trap explicitly.** Provider allocation has two independent
  mechanisms: explicit `[role_overrides]` (yields the **count** Claude=8 / Codex=4 for the 12-role
  roster) and the fallback `[model_mix]` **weights** (0.50/0.50, used only for roles NOT in the
  overrides map). A test asserting "50/50 by weight" and one asserting "8/4 by count" therefore
  coexist and measure different things; an agent editing one mechanism could break the other. Worth a
  code comment near `default_autopilot_config_content` / `provider_for_role` and a line in contributor
  docs.
