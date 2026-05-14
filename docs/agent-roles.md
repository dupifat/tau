# Agent roles

Agent roles are named aliases for the model and model-behavior settings Tau should use for agent turns.

A role can set:

- `model`: qualified model id, in `provider/model` form
- `effort`: `off`, `minimal`, `low`, `medium`, `high`, or `xhigh`
- `verbosity`: `low`, `medium`, or `high`
- `thinkingSummary`: `off`, `auto`, `concise`, or `detailed`
- `fastMode`: `true` or `false`

Roles live in `models.json5` under `defaultRoles`:

```json5
{
  defaultRoles: {
    default: {
      model: "openai/gpt-5.3-codex",
      effort: "medium",
    },
    smart: {},
    deep: {
      effort: "xhigh",
      verbosity: "high",
      thinkingSummary: "detailed",
    },
    rush: {
      effort: "low",
      verbosity: "low",
      thinkingSummary: "off",
    },
  },
}
```

Missing fields inherit from the `default` role. Fields still missing after that use Tau's hardcoded defaults for the selected model.

Tau ships built-in `default`, `smart`, `deep`, and `rush` roles. `smart` inherits `default`; `deep` asks for higher reasoning/verbosity; `rush` asks for lower reasoning/verbosity and Fast mode.


## Selecting a role

Use `/model <role>`.

`/model` completion lists roles, not raw models. Each completion description shows the currently resolved model and role settings.


## Editing roles

Use:

```text
/role <role> <delete|model|effort|verbosity|thinking-summary|fast-mode> [value]
```

Examples:

```text
/role default model openai/gpt-5.3-codex
/role deep effort xhigh
/role rush fast-mode on
/role temporary model anthropic/claude-sonnet-4-20250514
/role temporary delete
```

The `<role>` argument completes existing roles, but any new name can be used to create a role.

`/role <role> delete` removes the runtime/persisted role override. It does not edit `defaultRoles` from configuration; built-in or configured roles come back on the next harness start.

Runtime changes are persisted in `~/.local/state/tau/harness.json5` together with the last selected role.
