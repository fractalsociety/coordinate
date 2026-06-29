# Autopilot tmux Launcher Review

Task 25 review of the optional launcher at `scripts/squad-tmux-launch.sh` and helper library at `scripts/lib/squad-tmux-launcher-helpers.sh`.

## Current Flow

1. Reads `.squad/launcher.yaml` with Ruby `YAML.safe_load`.
2. Resolves project metadata, role names, worker count, Claude command/args, focus files, constraints, and optional worktree settings.
3. Optionally creates or reuses an isolated git worktree.
4. Writes dry-run artifacts under `.squad/quickstart/`:
   - `generated-manager.prompt.md`
   - `generated-inspector.prompt.md`
   - `generated-run-summary.md`
   - `generated-terminal-map.md`
5. In non-dry-run mode, runs `squad setup claude`, initializes the workspace, creates a tiled tmux session, starts each pane with the same Claude launch command, injects `/squad` role commands, waits for agents, then sends manager and inspector prompts.

## Reusable Pieces for Autopilot

- Worktree planning and safety checks are already separated in helper functions.
- `--dry-run` gives Autopilot a low-risk planning path with generated prompt and terminal-map artifacts.
- Prompt generation already records config root, workspace root, task brief, focus files, constraints, worker count, and worktree details.
- Pane labels and commands are assembled as arrays before tmux execution, which maps well to an Autopilot terminal session model.
- Existing shell tests cover command detection, worktree path handling, dry-run prompt generation, terminal map generation, and tilde expansion.

## Gaps Before Autopilot Launch

- Runtime is Claude-only. Autopilot needs per-agent provider commands for `claude`, `codex`, `gemini`, and `opencode`.
- `squad setup claude` is hard-coded. Multi-provider launch needs setup per detected provider or a shared setup phase.
- Pane commands assume `/squad <role>` syntax. Codex currently uses `$squad <role>`, and other providers may require provider-specific injection text.
- The terminal session model only exists as generated Markdown. Autopilot needs persisted `autopilot_terminal_sessions` rows with run ID, agent ID, terminal kind, command, and status.
- Worker topology is count-based rather than generated-agent-based. Autopilot should launch the synthesized team exactly, including role-specific prompts from `.squad/roles/generated/`.
- Join readiness is counted with `squad agents --json`; Autopilot will need to associate joined agent IDs with persisted generated agents.

## Recommended Integration Path

1. Keep the existing script as optional manual automation.
2. Extract the reusable launch plan concept into Rust first: labels, provider commands, injected squad command, workspace, and prompt path.
3. Generate the same terminal map in dry-run mode from the Rust launch plan.
4. Add provider-specific command rendering before invoking tmux.
5. Persist planned and launched panes to `autopilot_terminal_sessions`.
6. Only after the Rust model is covered by tests, decide whether the shell launcher should call the Rust planner or remain a separate manual tool.

## Existing Test Coverage

- `tests/squad_tmux_launcher_helpers_test.sh`
- `tests/squad_tmux_launcher_smoke.sh`

These should continue to run after Autopilot terminal-launch changes because they protect current manual launcher behavior.
