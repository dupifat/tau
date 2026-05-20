---
name: tau-self-knowledge-config
description: >
  Use this skill when the user asks how to configure Tau, where Tau stores config,
  state, sessions, runtime files, policies, credentials, or provider setup, or how
  to use tau init and tau provider commands.
advertise: false
---

# Tau configuration

Tau follows the XDG directory layout on normal Linux installs:

- Config: `~/.config/tau/`
  - `cli.yaml`, `cli.d/*.yaml` — CLI display preferences and key bindings.
  - `harness.yaml`, `harness.d/*.yaml` — harness roles/defaults, extensions, tools, and session retention.
- State: `~/.local/state/tau/` (or the platform/user state directory)
  - `sessions/<session_id>/` — durable session events, metadata, logs, and debug captures.
  - `cli.json` — persisted CLI runtime toggles.
  - `policy.cbor` — persisted socket-client policy decisions.
  - `auth.d/<provider>.json` — provider credentials; `auth.json` may exist as legacy credentials.
- Runtime: `${XDG_RUNTIME_DIR}/tau/<pid>/` or `/tmp/tau-$USER/<pid>/`
  - `tau.sock`, `tau.pid`, `tau.session_id`, `tau.dir` — daemon socket and discovery markers.

Use `tau init` to create starter `cli.yaml` and `harness.yaml` files.

## Providers

Use `tau provider add` for the interactive provider setup wizard. It configures API-key providers, local providers such as Ollama, and OAuth-backed providers.

Other provider subcommands:

- `tau provider list` — show configured provider credentials.
- `tau provider login [name]` — log in or refresh OAuth credentials; `tau provider login chatgpt` enables the built-in ChatGPT/Codex provider.
- `tau provider remove [name]` — remove credentials.
- `tau provider list-models [name]` — explains that models are published by provider extensions at runtime; start Tau and use `/model` to inspect the current model list.
