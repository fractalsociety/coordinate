# CLI Command Patterns Inventory - Run 5

Task: Autopilot task 516, "Identify existing CLI command patterns"

## Summary

The `squad` binary uses a hand-rolled Rust CLI parser in `src/main.rs`, not `clap` or another command framework. Top-level dispatch reads `std::env::args().skip(1)`, defaults to `help`, and matches the first token to command handlers. Unknown top-level commands intentionally fall through to a role-based join pattern, so `squad cto` is treated as `squad join cto --role cto`.

## Implemented Command Families

Provenance: `src/main.rs:21-127`

- Workspace/state commands: `init`, `clean`, `cleanup`, `doctor`.
- Agent commands: `join`, `leave`, `agents`.
- Messaging commands: `send`, `receive`, `pending`, `history`.
- Structured task commands: `task create`, `task ack`, `task complete`, `task requeue`, `task list`.
- Template commands: `roles`, `teams`, `team <name>`.
- Setup/help commands: `setup`, `help`, `--help`, `-h`, `--version`, `-V`.
- Autopilot commands: `autopilot init`, `autopilot plan`, `autopilot launch`, `autopilot run`; `swarm` is an alias for `autopilot`.

## Parsing Patterns

Provenance: `src/main.rs:142-188`, `src/main.rs:191-201`, `src/main.rs:204-240`, `src/main.rs:617-642`, `src/main.rs:675-710`, `src/main.rs:1886-1921`, `src/main.rs:1924-1975`

- Commands parse flags with small command-specific helper functions and explicit `while`/`match` loops.
- Missing required positional arguments produce `Usage: ...` errors at the dispatch point.
- Flag value validation usually rejects missing values and values that look like another flag, for example `join --role --client codex`.
- Some commands support JSON output via `--json`: `agents` and `receive`.
- `receive --timeout <secs>` is valid only with `--wait`.
- `send` accepts metadata flags before positionals: `--task-id`, `--reply-to`, and `--file <path-or->`.
- `task` and `autopilot` each have a second-level subcommand dispatcher.
- `autopilot launch` uses named flags for all options and requires `--run-id <id>`.

## Documented Surface

Provenance: `README.md:124-147`, `src/main.rs:2310-2353`

- The README commands table documents the main user-facing commands through `clean`.
- The built-in help additionally documents `cleanup`, `doctor`, the `autopilot` subcommands, structured task quick start, and examples.
- README currently documents `receive <id> [--wait] [--json]` but not the implemented `--timeout <secs>` option.
- README currently documents `clean` but not `cleanup`, `doctor`, or `autopilot` in the command table.

## Test Coverage Evidence

Provenance: `tests/cli_test.rs`

- Basic workspace and command smoke tests: `test_init`, `test_join_and_agents`, `test_send_and_receive`, `test_leave`.
- Input validation tests cover join flag value errors and receive timeout/unknown-flag errors.
- Autopilot tests cover `init`, `plan`, `launch`, `run`, and unknown autopilot subcommands.
- Structured task tests cover `create`, `ack`, `complete`, `requeue`, `list`, text receive formatting, JSON receive formatting, task metadata on send, and empty JSON inbox behavior.
- Help/docs alignment tests exist for leave/archive wording, receive timeout/debug wording, role prompts, and doctor help mention.

## Independent Verification Requirement

To independently verify this inventory, run:

```sh
cargo test --test cli_test
```

Verification should confirm the command patterns from executable behavior, not just source inspection. A stronger follow-up check is to compare `squad help` output against `README.md` and decide whether README should add `doctor`, `cleanup`, `autopilot`, and `receive --timeout`.

## Risks And Gaps

- Documentation and help are not identical; README omits several implemented commands/options listed above.
- Because unknown top-level commands become role joins, typo detection for top-level commands is intentionally weak.
- The parser is decentralized; adding a command requires updates in dispatch, usage/help text, README, and tests.
