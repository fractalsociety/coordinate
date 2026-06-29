# Session Token — Agent Identity Protection

**Goal:** Prevent silent identity hijacking when two agents join with the same ID, by using session tokens to detect and notify displaced agents.

**Problem:** When Terminal 2 runs `squad join worker` while Terminal 1 is already `worker`, the `INSERT OR REPLACE` silently overwrites Terminal 1's registration. Terminal 1 continues operating unaware that its messages are now being consumed by Terminal 2.

**Solution:** Last-writer-wins with notification. Each `squad join` generates a unique session token. Every subsequent squad command validates the token. A mismatch means the agent was displaced — the CLI returns a clear error with instructions to re-join under a new ID.

## Scenarios

| Scenario | Behavior |
|----------|----------|
| Two terminals `/squad worker` simultaneously | Second overwrites first. First gets "session replaced" error on next command, re-joins as `worker-2`. |
| Reconnect next day (stale agent) | New join overwrites stale token. No one holds the old token. Clean takeover. |
| Terminal crash and reconnect | Same as stale. Old token is abandoned. New join takes over. |
| Explicit different IDs (`worker-1`, `worker-2`) | No conflict. Tokens are per-ID. |
| Long coding session without squad commands | Token is not time-dependent. Still valid when agent runs next command. |

## Schema Change

```sql
ALTER TABLE agents ADD COLUMN session_token TEXT;
```

## Token Lifecycle

1. `squad join worker` → generate UUID → store in DB (`agents.session_token`) + file (`.squad/sessions/worker`)
2. Every command that acts as an agent (send, receive) → read token from file → compare with DB
3. Mismatch → error: `"Session replaced. Another terminal joined as worker. Re-join with: squad join worker-2 --role worker"`
4. `squad leave worker` → delete session file
5. `squad clean` → delete all session files

## Slash Command Update

Add one line to slash command instructions:
```
If any squad command returns "Session replaced", re-join with a suffixed ID (e.g. worker-2) and continue.
```

## Out of Scope

- Preventing the overwrite (last-writer-wins is intentional)
- Heartbeat / TTL mechanisms
- PID tracking
- Daemon processes
