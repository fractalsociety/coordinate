# Non-blocking Receive — Fix AI Tool Compatibility

**Goal:** Eliminate the double-consumer race condition caused by AI tools backgrounding `squad receive --wait`.

**Problem:** `squad receive --wait` blocks for up to 3600s. AI tools (Claude Code, Gemini, Codex) have bash timeouts (120-300s) that background the command. The AI agent then starts a second receive, creating two consumers competing for the same messages. The loser never sees the message.

**Root cause:** The slash command templates and role files instruct AI agents to use `--wait`. Remove that instruction and the problem disappears.

## Solution

**Template-only fix.** No new CLI flags needed.

`squad receive <id>` (without `--wait`) already exists — it checks once and returns immediately. This naturally fits AI tools' "execute → get result → think → execute" model.

### Changes

1. **Update slash command templates** — replace `squad receive <id> --wait` with `squad receive <id>` in all instructions. Remove the "retry on timeout" instruction.

2. **Update role templates** — worker.md, manager.md, inspector.md: replace `--wait` with plain receive.

3. **Update freeform role fallback** — main.rs `cmd_join` prints `--wait` for roles without a template file.

4. **Update README and help text** — remove "The --wait Pattern" section, update examples.

5. **Harden receive transaction** — add `id <= max_id` fence to the UPDATE in `receive_messages` to prevent phantom read (a message arriving between SELECT and UPDATE getting silently marked as read).

### What we're NOT doing

- ~~`--poll N` flag~~ — unnecessary. `squad receive` already does a one-shot check. AI agents naturally retry on their own.
- ~~Split transaction~~ — the DEFERRED transaction provides value (groups SELECT+UPDATE). Splitting it introduces a worse phantom read bug.
- ~~Deprecation warning on --wait~~ — keep it working for manual/debug use, just don't recommend it.

## Behavior after fix

```
AI agent completes task
  → squad send worker manager "done"
  → squad receive worker              ← instant check, returns immediately
  → no messages? continue other work
  → check again when ready
```

No blocking, no backgrounding, no competing consumers.

## Files Modified

| File | Changes |
|------|---------|
| `src/store.rs` | Add `id <= max_id` fence to UPDATE |
| `src/setup.rs` | Remove --wait from both slash command templates |
| `src/roles/*.md` | Remove --wait from 3 role templates |
| `src/main.rs` | Fix freeform role message + HELP_TEXT |
| `README.md` | Remove --wait Pattern section, update examples |
| `README.zh-CN.md` | Same as above |
