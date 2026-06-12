# tau-socket agent notes

Before changing this crate, read:

- `ARCHITECTURE.md` for listener/client directionality and ownership boundaries.
- `SECURITY.md` for local IPC and socket path cleanup invariants.

Keep changes focused on Unix socket transport behavior. Preserve explicit receive
outcomes, partial-frame decode errors, safe stale-socket cleanup, and background
reader shutdown behavior. Update focused regression tests when these contracts change.
