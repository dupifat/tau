# Security policy

Tau is still early-stage software, but security issues are important. Please
report suspected vulnerabilities through GitHub private vulnerability reporting
for `dpc/tau` (`https://github.com/dpc/tau/security/advisories/new`) when
available. If that path is unavailable, contact the maintainer privately first
and avoid filing a public issue with exploit details.

## Harness and extension boundaries

The harness treats extensions as less-trusted peers connected over the Tau
protocol. For extension-owned persistent data, the harness confines paths to
per-extension state roots, rejects path traversal and symlink escapes, uses
private file and directory permissions where supported, and enforces per-file and
per-directory-list quotas. Quota failures are returned to extensions as
`quota_exceeded` extension-data errors.

These quotas bound individual file writes, file reads, and directory listing
work performed by the harness. They do not bound aggregate per-extension disk
usage across many files, sandbox arbitrary extension code, or prevent protocol
payloads from being deserialized before the harness validates an operation. Run
only extensions you trust to execute on your machine.

## Telegram extension

`std-telegram` / `tau-ext-telegram` is disabled by default because it bridges
untrusted external Telegram text into Tau prompts. When enabled, it requires an
explicit bot-token secret and non-empty allowlist of Telegram user ids. The model
cannot provide arbitrary chat ids: outgoing messages use only the configured
chat or an allowlisted user's linked private chat. Unconfigured group/supergroup
chats are refused, and configured groups should be treated as shared output
channels visible to everyone in that chat. Runtime registrations, selected
agents, learned chat id, and update offsets are in-memory only. Avoid logs that
include bot tokens, Bot API URLs, or unexpected private Telegram content.

## Rhai scripting extension

`std-rhai` / `tau-ext-rhai` scripts are trusted local code. A Rhai script can
register agent-invokable tools, handle model-originated tool calls, emit raw Tau
events, and execute host shell commands directly through the Rhai extension.
These shell commands intentionally do not route through `tau-ext-shell` and do
not participate in ext-shell directory-update locks; only enable scripts you
would be comfortable running as local programs.

## Interception boundary

Interceptors are privileged local extensions. They can see, modify, or drop most
events they subscribe to before those events commit. Must-pass and immutable
checks protect selected harness-owned facts from integrity loss, but they are not
confidentiality boundaries: do not expose sensitive event streams to interceptors
you do not trust.

## CLI terminal UI

The terminal UI executes trusted local configuration and environment-derived
commands, including key-binding shell snippets, completion commands, `$EDITOR`,
and `$VISUAL`. Treat `cli.yaml`, inherited environment variables, and PATH as
local code execution inputs rather than untrusted data.

Prompt completion may read the local filesystem and query `git` for tracked and
unignored files. These operations should stay bounded and best-effort: failures
or quota/size limits should disable the completion source or surface a local
notice, not wedge the prompt.

Raw terminal mode is a process-local ownership boundary. Before spawning editors
or pickers, Tau must pause redraws, release raw-mode features, and always clear
that paused state when setup or resume fails so the UI cannot remain permanently
muted. Abort paths for terminal-releasing shell actions should terminate the
owned process group before Tau resumes raw-mode/redraw ownership. Redraw and
input coordination assumes a single foreground reader thread; background
renderer threads must not write while the terminal is released to an external
program.

## Reporting guidance

When reporting a vulnerability, include:

- affected Tau version or commit;
- operating system and relevant configuration;
- minimal reproduction steps;
- whether an extension, provider, UI client, or daemon boundary is involved;
- any logs that do not contain secrets.

Avoid sharing API keys, OAuth tokens, email contents, or other private data in
reports.
