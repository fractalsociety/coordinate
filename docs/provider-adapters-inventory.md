# Provider Adapters — Inventory (Task 134)

> **Autopilot-Task-ID:** 134 · **Autopilot-Run-ID:** 2
> **Owner:** scientific_planner-2 · **Role:** scientific_planner
> **Scope:** Identify the existing provider/model adapters in the `squad` codebase.
> **Provenance:** All claims cite `file:line` against the working tree at commit `8146bcc` (HEAD) on `2026-06-29`.

## 1. Executive summary

The codebase has **one canonical provider abstraction** — the `ModelProvider` enum
(`src/autopilot.rs:13`) with **7 variants** — that is adapted to concrete CLI behavior through
**three distinct adapter layers**:

1. **Launch/CLI adapter** — `provider_tool_command()` maps a `ModelProvider` to a
   `(program, args)` shell command (`src/autopilot.rs:2758`).
2. **Prompt-injection adapter** — `role_specific_injection_text()` maps a provider to the join
   instruction syntax a spawned agent must run (`src/autopilot.rs:2780`).
3. **Install/slash-command adapter** — `PLATFORMS` in `src/setup.rs:14` maps provider/clients to
   the in-repo slash-command template files installed into each tool's config dir.

A supporting **rank/scoring adapter** (`model_provider_rank`, `src/store.rs:1938`) orders
providers by capability for escalation, and a **`--client` validator** (`src/main.rs:164`)
restricts which providers may actually *join* a session.

**Key distinction:** all 7 `ModelProvider` variants exist for **autopilot routing/planning**, but
only **4** (`claude`, `codex`, `gemini`, `opencode`) are valid interactive join clients
(`squad join --client`). `openrouter_free`, `openrouter_cheap`, and `local` are **autopilot-only
routing tiers**, not first-class join clients — they are realized by reusing other binaries.

## 2. The `ModelProvider` enum (the abstraction)

`src/autopilot.rs:11-31` — `#[serde(rename_all = "lowercase")]`, 7 variants:

| Variant | `as_str()` | Serde aliases | Source |
|---|---|---|---|
| `Claude` | `claude` | — | `src/autopilot.rs:36` |
| `Codex` | `codex` | — | `src/autopilot.rs:37` |
| `Gemini` | `gemini` | — | `src/autopilot.rs:38` |
| `OpenCode` | `opencode` | — | `src/autopilot.rs:39` |
| `OpenRouterFree` | `openrouter_free` | `openrouter-free`, `openrouterfree` | `src/autopilot.rs:18-23,40` |
| `OpenRouterCheap` | `openrouter_cheap` | `openrouter-cheap`, `openroutercheap` | `src/autopilot.rs:24-29,41` |
| `Local` | `local` | — | `src/autopilot.rs:42` |

Round-tripping: `FromStr` at `src/autopilot.rs:53-70`; `Display` at `src/autopilot.rs:47-51`.

## 3. Adapter layer 1 — Launch/CLI command (`provider_tool_command`)

`src/autopilot.rs:2758-2778`, returns `ProviderToolCommand { program, args }`
(struct `src/autopilot.rs:2407`; `shell_command()` builder at `src/autopilot.rs:2413`).

| `ModelProvider` | program | args | Notes |
|---|---|---|---|
| `Claude` | `claude` | `--dangerously-skip-permissions` | full autonomy flag |
| `Codex` | `codex` | `--yolo` | full autonomy flag |
| `Gemini` | `gemini` | *(none)* | |
| `OpenCode` | `opencode` | *(none)* | |
| `OpenRouterFree` | `opencode` | `--model openrouter/free` | **proxied via opencode binary**, no native OpenRouter client |
| `OpenRouterCheap` | `opencode` | `--model openrouter/cheap` | **proxied via opencode binary** |
| `Local` | `zsh` | *(none)* | a raw local shell, not an AI client |

Consumed by `terminal_session_plan_for_role()` (`src/autopilot.rs:2717-2737`), which populates
`TerminalSessionPlan.command` and `.provider_tool` (`src/autopilot.rs:2392-2404`).

## 4. Adapter layer 2 — Prompt injection syntax (`role_specific_injection_text`)

`src/autopilot.rs:2780-2790`. The text auto-typed into a spawned pane to make the agent join:

| Provider | Injection text |
|---|---|
| `Codex` | `$squad {role_id} {agent_id}` (Codex Skills trigger) |
| `Local` | `squad join {agent_id} --role {role_id} --client opencode --protocol-version 2 && squad receive {agent_id} --wait` |
| all others (`Claude`, `Gemini`, `OpenCode`, `OpenRouter*`) | `/squad {role_id} {agent_id}` (slash command) |

Note: `Local` joins using `--client opencode` (so a local tier reuses the opencode client).

## 5. Adapter layer 3 — Install/slash-command templates (`PLATFORMS`)

`src/setup.rs:14-39`. Installs a squad join template per *detected* client binary. Four templates:

| client | install path (under `$HOME`) | template content | format |
|---|---|---|---|
| `claude` | `.claude/commands/squad.md` | `SQUAD_MD_CONTENT` | Markdown, `$ARGUMENTS` |
| `gemini` | `.gemini/commands/squad.toml` | `SQUAD_TOML_CONTENT` | TOML, `{{args}}` |
| `codex` | `.codex/skills/squad/SKILL.md` | `SQUAD_CODEX_CONTENT` | Skills MD, `$ARGUMENTS` |
| `opencode` | `.config/opencode/commands/squad.md` | `SQUAD_MD_CONTENT` | Markdown (reuses Claude template) |

Content defined at `src/setup.rs:42` (Codex), `:108` (MD), `:173` (TOML).
Lifecycle helpers: `run_setup` `src/setup.rs:480`, `check_and_update_commands` `:307`,
`cleanup_commands` `:339`, `detect_platforms` `:446`, `diagnose_templates_for_platforms` `:251`.

## 6. Supporting adapters

- **Capability rank** — `model_provider_rank()` `src/store.rs:1938-1946`:
  `local=1`, `openrouter_free=2`, `openrouter_cheap|gemini|opencode=3`, `codex=4`, `claude=5`,
  unknown=`0`. Drives model-mix selection / escalation ordering.
- **`--client` validator** — `src/main.rs:164-169`: `squad join --client` accepts only
  `claude | gemini | codex | opencode`. Confirms that `openrouter_free` / `openrouter_cheap` /
  `local` are **not** join clients.
- **Model-mix defaults** — `ModelMix::default` `src/autopilot.rs:115-126`:
  `claude=0.15, codex=0.15, gemini=0.00 (off by default), openrouter_free=0.50,
  openrouter_cheap=0.10, local=0.10`.
- **Role→provider overrides** — `AutopilotConfig::provider_for_role` `src/autopilot.rs:90-97`
  (per-role override, else default provider).
- **Per-provider spawn delay** (macOS Terminal) — `macos_terminal_inject_delay_seconds`
  `src/autopilot.rs:2709-2715`: `Codex=45s`, `Claude=8s`, else `1s`.

## 7. Observations / gaps (for follow-up)

1. **No native OpenRouter adapter.** `OpenRouterFree`/`OpenRouterCheap` are thin shims over the
   `opencode` binary with a `--model` flag (`src/autopilot.rs:2764-2771`). A real OpenRouter
   provider pool / rate-limit tracker (planned tasks 70-73) does **not** exist yet.
2. **`Local` is a bare `zsh` shell** (`src/autopilot.rs:2772`) — not an AI model. The "local model"
   support (planned tasks 79-86) is **not** implemented.
3. **`Gemini` is disabled by default** in the model mix (`gemini: 0.00`, `src/autopilot.rs:120`).
4. **Template/markdown adapter re-use:** `opencode` reuses the Claude markdown template
   (`src/setup.rs:37`), so the two are not independently customizable.
5. **`ProviderToolCommand` is a flat struct** with no trait/abstraction — adding a new provider
   requires editing 5 match arms (FromStr `:57`, as_str `:34`, `provider_tool_command` `:2759`,
   `role_specific_injection_text` `:2785`, `model_provider_rank` `:1939`) plus the join validator
   if it should be a first-class client. Risk of drift.

## 8. Independent verification requirement

Per the task's acceptance criteria, **this inventory must be independently verified before it is
relied upon**. Recommended independent verification (assignable to `inspector` /
`verification_worker`):

- **V1 (mechanical):** For each of the 7 `ModelProvider` variants, confirm the `program`/`args`
  in `provider_tool_command` (`src/autopilot.rs:2758`) matches the table in §3 — by reading the
  function directly, *not* this report.
- **V2 (behavioral):** Confirm `provider_tool_command(&ModelProvider::X).shell_command()` returns
  the expected string via the existing test
  `test_provider_tool_command_maps_supported_provider_tools` (`tests/autopilot_terminal_session_test.rs:294`),
  and that injection text is covered by `test_role_specific_injection_text_uses_provider_prompt_style`
  (`tests/autopilot_terminal_session_test.rs:314`).
- **V3 (gap check):** Confirm claims in §7 by grepping for `OpenRouter`/rate-limit/local-model
  symbols (expected: absent).

**Status of acceptance criteria:**
- ☑ Task output recorded with provenance — this file (`docs/provider-adapters-inventory.md`),
  every claim cites `file:line`.
- ☐ Task result has an independent verification requirement — defined in §8 above; **not yet
  executed** (this agent is the planner, not the verifier). Requires an independent agent to run
  V1–V3.

## 9. Changed files

- `docs/provider-adapters-inventory.md` (new — this file).

## 10. Tests run

None. This is an investigation/identification task; no code changed. Compilation/behavioral
confirmation is deferred to the independent verifier (§8).
