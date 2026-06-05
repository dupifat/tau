# Agent roles

Agent roles are named aliases for the model and model-behavior settings Tau should use for agent turns.

A role can set:

- `description`: short free-form summary shown in `/role ...` completions
- `model`: qualified model id, in `provider/model` form
- `effort`: `off`, `minimal`, `low`, `medium`, `high`, or `xhigh`
- `verbosity`: `low`, `medium`, or `high`
- `thinking_summary`: `off`, `auto`, `concise`, or `detailed`
- `service_tier`: `fast` or `flex`
- `compaction`: provider-side automatic compaction policy: `provider_default`, `disabled`, or `{ threshold: 200000 }`
- `prompt_fragments`: role-specific prompt fragments
- `prompt_override`: system prompt template name
- `tools`: explicit internal tools enabled for this role
- `enable_tools`: internal tools added to the selected/default set
- `disable_tools`: internal tools removed from the selected/default set

Top-level `prompt_fragments` in `harness.yaml` apply to every role. Use them for global style or policy instructions:

```yaml
prompt_fragments:
  - name: user.short-plain-style
    priority: 65
    text: Keep answers short and plain, using only simple words.
```

Roles live in `harness.yaml` under globally unique `role_groups`. Each group has a `roles` map, plus optional role fields such as `prompt_fragments` that apply as defaults to every role in the group. `default_role` selects the startup role; if omitted, Tau starts on the first role in `role_groups` order.

```json5
{
  default_role: "senior-engineer",
  role_groups: {
    engineer: {
      prompt_fragments: [
        { name: "engineer.workflow", priority: 66, text: "Focus on implementation details." },
      ],
      roles: {
        "junior-engineer": {
          description: "Lower-reasoning engineer",
          effort: "low",
        },
        "senior-engineer": {
          description: "Balanced coding engineer",
          model: "chatgpt/gpt-5.3-codex",
          effort: "medium",
          compaction: { threshold: 200000 },
          tools: ["read", "grep"],
          enable_tools: ["web_search"],
        },
        "staff-engineer": {
          description: "Maximum-reasoning engineer",
          effort: "xhigh",
        },
        "old-role": {
          enable: false,
        },
      },
    },
    manager: {
      roles: {
        manager: {
          prompt_fragments: [
            { name: "manager.workflow", priority: 66, text: "Delegate non-trivial work." },
          ],
        },
      },
    },
  },
}
```

Missing fields use group defaults first, then provider-published fallback knobs for the role's resolved model. Tool filtering starts with `tools` when set, otherwise with each tool's default enablement; then `enable_tools` adds tools, and `disable_tools` removes tools. When `compaction` is omitted, Tau asks supported providers to use their model-specific compaction default. Set `enable: false` on a role in a higher-precedence config layer to remove it from the effective role list and role-group cycling after all layers merge.

Tau ships built-in `junior-engineer`, `senior-engineer`, `staff-engineer`, and `manager` roles, with `default_role: senior-engineer`. `junior-engineer` uses lower reasoning for straightforward engineering work, `senior-engineer` uses balanced individual-contributor defaults, and `staff-engineer` is the maximum-reasoning engineering variant. `manager` is an orchestration role with a built-in delegation prompt. For non-trivial work, the built-in `manager` prompt tells the model to use `delegate` by default for research/scoping, implementation, and review/validation sub-agent steps, then synthesize the results; tiny or purely clerical work may still be handled directly.


## Selecting a role

Use `/model <role>` or `/role <role>`.

`/model` and `/role` completion list roles, not raw models. Each completion description shows the currently resolved model and role settings. `/role` completions also append the configured role `description` when present.


## Editing roles

Use:

```text
/role <role> <delete|model|effort|verbosity|thinking-summary|service-tier|compaction-threshold|tools|enable-tools|disable-tools> [value]
```

Examples:

```text
/role engineer model chatgpt/gpt-5.3-codex
/role manager effort xhigh
/role engineer enable-tools web_search
/role engineer disable-tools shell
/role temporary model anthropic/claude-sonnet-4-20250514
/role temporary delete
```

Use `reset` as the value to clear a field and return to model/provider fallback behavior (`off` is still the explicit off value for `effort` and `thinking-summary`).

The convenience command `/fast` mutates the currently selected role using the same role-update path.

The `<role>` argument completes existing roles, but any new name can be used to create a role for the current run. Add it to `role_groups` if it should be available after restart.

`/role <role> delete` removes the runtime role override. It does not edit `role_groups` from configuration; built-in or configured roles come back on the next harness start.

Runtime role changes are not persisted. Startup is controlled by `default_role` and `role_groups` order, and durable role changes should be made in `harness.yaml`.

Prompt fragment priorities sort ascending. Use priorities below `100` for role/persona instructions that should appear before generated context sections such as skills and AGENTS.md. Use high priorities for epilogue context; Tau's built-in current-working-directory fragment uses `900` so it stays at the end of the prompt.
