---
name: tau-self-knowledge-ext-shell
description: Use this extension skill when the user asks about Tau's core-shell extension, filesystem tools, shell command execution, file editing, directory locks, AGENTS.md discovery, shell configuration, or read-only tool isolation.
advertise: false
---

# Tau core-shell extension self-knowledge

`core-shell` is Tau's built-in shell and filesystem extension. It runs `tau-ext-shell`, is enabled by default, and registers the everyday project-inspection and mutation tools used by agents.


## Tools and behavior

Model-visible tools:

- `read` — reads UTF-8 and non-UTF-8 files with line numbers, line-ending markers, Unicode replacement for invalid bytes plus `invalid-utf8` flags, range/ranges support, and line/byte truncation metadata.
- `edit` — applies guarded line-oriented replacements. `newText` fully replaces the inclusive selected line range; the first line after `end_line` is preserved unchanged and should not be included unless the range includes it. The agent-visible result is minimal status only; the UI receives a separate structured diff payload for changed UTF-8 files, including inline changed-token segments. Non-empty replacements stay whole-line; if a missing line ending is added, the result includes `newline_added: true`.
- `apply_patch` — applies patch-style file edits and also sends structured UI-only diffs for changed UTF-8 files. It is registered but disabled by default.
- `shell` — runs `sh -c`-style commands with `mode: "ro"` or `mode: "rw"`, optional `cwd`, timeout, stdout/stderr capture, Unicode replacement for invalid output bytes plus `invalid-utf8` flags, truncation, and tool cancellation support.
- `gpt_shell` — shell-like execution surface advertised as model-visible `shell_command` for GPT-style tool compatibility. It is registered but disabled by default.
- `grep` — ripgrep-backed literal or regex search with context, glob filtering, and truncation.
- `find` — ignore-aware glob file search.
- `ls` — sorted directory listing with 1-based entry prefixes, escaped control characters/backslashes, Unicode replacement for invalid filename bytes plus `invalid-utf8` flags, and standard truncation metadata.
- `dir_lock` — manual directory update lock/unlock for coordinating mutating agents.

Test builds or the `echo-agent` cargo feature also register `echo` for harness tests.

For Tau VCR runs, `ls`, `read`, `edit`, `apply_patch`, `shell`, and `gpt_shell` use the shared ext-shell world-operation recorder. Replay substitutes recorded filesystem effects such as directory listing, file reads, parent-path checks, directory creation, and asserted writes/removes while still running normal tool argument handling, guard validation, patch application, diff generation, escaping, invalid-UTF-8 handling, and truncation logic. Shell terminal outcomes are recorded as world operations: finished results replay at 100x recorded speed, while recorded cancellations require a matching replay cancellation request.


## Directory locks and mutation safety

When `config.dir_lock.enable` is true, `dir_lock` is available and mutating calls automatically acquire matching directory locks: `edit`, `apply_patch`, and `shell`/`gpt_shell` with `mode: "rw"`. Read-only calls and shell calls with `mode: "ro"` do not wait on update locks. The extension publishes a `/shell-dir-force-unlock DIRECTORY` user action when a manual lock blocks work long enough to matter.

Read-only shell mode is advisory unless `config.enforce_ro_mode: true` is set. Enforced mode uses a read-only bind mount of the tool cwd when supported, but it is opt-in because tools such as `jj` and `nix-direnv` can break under that namespace setup.


## Agent context discovery

`core-shell` discovers and publishes project/user instructions and skills:

- `$HOME/.agents/AGENTS.md` and `$HOME/.agents/AGENTS.*.md`
- `AGENTS.md` and `AGENTS.*.md` in current-working-directory ancestors
- matching `.agents.local/AGENTS.md` and `.agents.local/AGENTS.*.md` files
- skills under `.agents/skills`, `.agents.local/skills`, `$HOME/.agents*/skills`, and `$HOME/.config/agents*/skills`

`.local` locations are intended for machine- or user-specific instructions and are usually gitignored.


## Configuration

Configured under `extensions.core-shell.config`:

```json5
extensions: {
  "core-shell": {
    config: {
      working_directory: "/srv/project",
      enforce_ro_mode: false,
      shell: {
        command: "bash",
        prefix: ["nix", "develop", "-c"],
        user_command_timeout_secs: 3600,
        extra_env: { PAGER: "cat" },
      },
      dir_lock: { enable: true },
    },
  },
}
```

`working_directory` changes the extension process cwd after startup config. `shell.command` is invoked as `<command> -c <user command>` after `shell.prefix`. `shell.extra_env` is applied to shell-tool and user `!`/`!!` child processes after the inherited environment. `user_command_timeout_secs` affects UI-initiated shell commands; agent tool calls use their own `timeout` argument.
