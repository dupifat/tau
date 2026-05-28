# Shell directory locking

This note documents the move of filesystem update coordination out of `delegate` and harness scheduling and into `tau-ext-shell`.


## Current design

- `tau-ext-shell` owns directory update locking with an optional `dir_lock` tool.
- The tool name is `dir_lock`; Tau tool names do not allow hyphens.
- `dir_lock` is registered enabled by default. Setting ext-shell config `dir_lock.enable = false` disables the handler, re-registers the tool as disabled by default, and opts mutating ext-shell tools out of locking.
- `delegate` sub-agents are independent agents. A parent agent lock does not automatically cover a delegate.
- The harness no longer enforces tool or start-agent update/exclusive scheduling, and the protocol no longer carries scheduling metadata for tool specs, delegate progress, or start-agent requests.


## `dir_lock` semantics

Arguments:

- `command`: `update` or `unlock`
- `directory`: an existing directory

All `directory` values are canonicalized before use. Missing paths or non-directories are errors.

`update` acquires a manual update lock for the canonical directory and the owning `agent_id`. `unlock` releases one matching manual lock held by that same agent. A second `update` by the same agent for the same directory, an ancestor, or a child is an error; manual double-locking is treated as a likely forgotten unlock.

Conflicts are based on path ancestry: a lock conflicts when either directory contains the other. Reads do not participate.

The wait queue is FIFO. If the front waiter is blocked, later waiters do not jump ahead. Same-owner automatic reentry is allowed so an agent holding a manual lock can keep using mutating tools under that lock without deadlocking itself. A repeated manual `dir_lock update` still errors.

Manual locks are released when ext-shell observes `SessionAgentUnloaded` for the owning agent, and all manual locks are released on `SessionShutdown`. The extension also publishes a UI action `/shell-dir-force-unlock DIRECTORY` that canonicalizes an existing directory and force-releases all overlapping manual locks, regardless of owner. It does not cancel or release automatic locks held by currently running tools.


## Automatic locking for ext-shell tools

When `dir_lock.enable` is true (the default), these mutating tools acquire automatic update locks before running:

- `write`: locks the target file parent. If parents are missing, it locks the deepest existing ancestor so existing `write` behavior remains intact.
- `edit`: locks the canonical parent of the existing file, following a final symlink to the real edited file.
- `apply_patch`: parses the patch and locks all touched source and destination directories as one FIFO request.
- `shell` and `gpt_shell`: lock the canonical `cwd`, or the extension process cwd when `cwd` is omitted.

Automatic locks are held only for the tool invocation duration. They serialize with manual locks and with other automatic mutating calls. When the calling agent already owns a covering manual lock, automatic calls under that lock reenter the same writer section and do not wait on same-owner automatic calls; other agents remain blocked until the manual lock is released and all active automatic calls finish. Lock waiters do not consume the ext-shell worker semaphore; the semaphore is acquired only after the lock is granted.

`read`, `grep`, `find`, and `ls` remain free to run while update locks are held. User `!` shell commands are UI commands, not agent tool calls, and are intentionally excluded.


## UI behavior

Blocked ext-shell tool calls emit `ToolProgress` with a live `ToolDisplay` update that shows the directory or directories being waited on. `dir_lock` terminal success and failure displays also include the relevant directory when it is known. Those displayed directories are valid inputs to `/shell-dir-force-unlock`; the action releases overlapping manual locks, so either an ancestor or child lock can be cleared from the waiting path. Normal foreground and auto-background behavior still applies because the harness sees the call as running until the extension sends a terminal event.


## Caveats

- Shell locking is advisory. A shell command can mutate paths outside its `cwd` using absolute paths or command-specific flags.
- `write` to missing parents is safe but less precise because the exact final parent does not exist yet.
- Same-owner reentry can keep other agents waiting for as long as the owner keeps a manual lock. That is intentional manual-lock behavior, not starvation inside the FIFO queue.
- Out-of-tree non-shell tools no longer get harness update/exclusive serialization. They need their own coordination if they mutate shared state.
