---
name: tau-self-knowledge-config
description: >
  Use this skill when the user asks how to configure Tau, where Tau stores config,
  state, sessions, runtime files, policies, credentials, or provider setup, or how
  to use tau init and tau provider commands.
advertise: false
---

# Tau configuration

Tau follows the XDG directory layout on Linux:

- Config: `~/.config/tau/`
  - `cli.yaml`, `cli.d/*.yaml` — CLI display preferences and key bindings.
  - `harness.yaml`, `harness.d/*.yaml` — harness roles/defaults, extensions, tools, and session retention.
- State: `~/.local/state/tau/` or the platform/user state directory.
  - `sessions/<session_id>/` — durable session membership, metadata, logs, and debug captures.
  - `agents/<agent_id>/` — durable agent transcripts and metadata.
  - `cli.json` — persisted CLI runtime toggles.
  - `policy.cbor` — persisted socket-client policy decisions.
  - `auth.d/<provider>.json` — provider credentials; `auth.json` may exist as legacy credentials.
- Runtime: `${XDG_RUNTIME_DIR}/tau/<pid>/` or `/tmp/tau-$USER/<pid>/`.
  - `tau.sock`, `tau.pid`, `tau.session_id`, `tau.dir` — daemon socket and discovery markers.

Use `tau init` to create starter `cli.yaml` and `harness.yaml` files.

## Built-in defaults

Tau layers these defaults underneath user config and `*.d/*.yaml` drop-ins.

### Harness defaults

```yaml
{harness_config}
```

### CLI UI defaults

```yaml
{ui_config}
```

## Providers

Use `tau provider add` for the interactive provider setup wizard. It prompts for provider kind, provider namespace, auth, and model details as needed.

Other provider commands:

- `tau provider list` — show configured provider profiles.
- `tau provider remove <name>` — remove a provider profile.

Models are published by provider extensions at runtime; start Tau and use `/model` to inspect the current model list.
