# Providers

A provider is a normal Tau extension that exposes models and executes prompts.
The harness does not own provider-specific LLM execution, and `tau-agent` should not remain as a central executor in the final architecture.

## Core meaning

- **provider**: a configured runtime instance that can expose and execute one or more models
- **model**: a selectable model exposed by a provider
- **role**: a harness-owned named default that points at a model plus optional model parameters

## Core responsibilities

Provider extensions own provider-specific work:

- auth and runtime state
- model availability snapshots
- request execution
- response streaming
- provider protocol details

The harness owns orchestration:

- sessions and prompt assembly
- model and role selection
- mapping `ModelId` to the provider extension that published it
- direct prompt routing
- Tau tool routing and the tool-call follow-up loop
- harness/UI state such as selected model and available roles

The UI should stay dumb: it consumes harness/provider events and asks the harness to change model or role state.

## Model publication and routing

One extension may publish multiple models.
One model carries provider identity in its `ModelId`.

```rust
extension -> models
```

Example:

```rust
ModelId::new("openai", "gpt-5.5")
ModelId::new("chatgpt", "gpt-5.3-codex")
```

The provider extension publishes `provider.models_updated` with the models it can currently serve.
This snapshot carries model metadata, not just IDs:

```rust
struct ProviderModelInfo {
    id: ModelId,
    display_name: Option<String>,
    context_window: u64,
    efforts: Vec<Effort>,
    verbosities: Vec<Verbosity>,
    thinking_summaries: Vec<ThinkingSummary>,
}
```

`context_window` is required for every published model.
Publishing a model means it is available; no separate `enabled` flag is needed initially.

The harness records which extension sent the snapshot and uses that as routing state.
It also uses the metadata for model UI state: context window, effort choices, verbosity choices, thinking-summary choices, and role descriptions.

Prompt execution should be directed to the extension that owns the selected `ModelId`; it should not be broadcast to every provider.
This mirrors Tau's tool routing model.

## Execution events

Provider execution should use provider-named events, not `agent.*` events:

- `provider.prompt_submitted`
- `provider.response_updated`
- `provider.response_finished`

These should keep the semantics of the current agent execution events as much as possible:

- submitted = the provider accepted the prompt and started work
- updated = accumulated streamed response text/thinking so far
- finished = final response, tool calls, usage, stop reason, backend metadata

Provider final responses may contain tool calls, but providers do not execute Tau tools.
The harness routes tools and sends follow-up prompts back to the selected provider when needed.

## Roles

Roles are harness-owned.
A role points at a model and may include model parameters.

```rust
Role {
    name: "smart".into(),
    model: ModelId::new("chatgpt", "gpt-5.3-codex"),
}
```

The harness owns role resolution and first-model selection.
The UI displays and edits resolved harness state; it should not do provider resolution itself.

## State

Provider-specific config and runtime state should live with the provider extension / provider storage.
There should be no global `models.json5` that describes every provider runtime.

A provider owns its own:

- auth state
- cached tokens
- endpoint/runtime settings, if any are needed later
- transport caches or pools
- internal metadata

For the first OpenAI Responses provider, auth presence is enough to enable a provider namespace:

- `openai/*` is available when OpenAI API-key state exists
- `chatgpt/*` is available when ChatGPT OAuth state exists

No separate enable flag is needed initially.

## Initial first-party provider

The first provider extension should cover only the Responses backend:

- `openai/*` for the public OpenAI Responses API
- `chatgpt/*` for the ChatGPT / Codex Responses backend

Initial model lists and metadata should be hardcoded, including required context windows.
Do not add upstream model discovery, compat matrices, custom base URLs, or chat-completions support in the first cut.

## Summary

- providers are normal Tau extensions
- provider extensions publish models and execute prompts
- the harness routes prompts directly to the selected model owner
- execution events should be `provider.*`, not `agent.*`
- the harness owns roles, selection, sessions, and tool routing
- provider state belongs to providers
- the UI should not resolve providers itself
