# Session Rework

This document sketches a historical proposed architecture change for Tau's
session model. It is superseded for transcript persistence by
[`agents-rework.md`](agents-rework.md): sessions are now membership containers,
while durable conversation transcripts live in global agent logs.

## Problem

Today, `session` mixes several concepts:

- persisted conversation history
- currently bound harness state
- extension lifecycle/init boundary
- cwd/project context
- prompt/runtime routing

That makes multi-session harness support difficult and makes it unclear which events are durable transcript facts versus runtime status.

## Target Meaning

A **session** should mean one independent conversation lane.

A harness may host many sessions at once.

A session is **not** the harness lifetime. There should be no global `current_session_id` in the harness.

The client/UI owns its current session selection. Every UI event that targets a session should carry `session_id` explicitly.

## Initial Multi-Session Shape

Do **not** remove `Conversation` in the first refactor. Keep it as internal runtime plumbing for side prompts, delegates, prompt queues, and branch cursors.

However, conversations should live inside session runtime state rather than in one global harness map.

```text
Harness
  sessions: HashMap<SessionId, SessionRuntime>

SessionRuntime
  session_id
  cwd / execution root
  selected_role
  selected_model
  role overrides / effective role state
  context usage and token accounting
  compaction state
  extension/context snapshot
  default_conversation_id
  conversations: HashMap<ConversationId, Conversation>
```

This gives multi-session support without rewriting the side-conversation/subagent machinery at the same time.

A later refactor can decide whether to remove `Conversation`, turn side conversations into child sessions, or keep conversations as an internal implementation detail.

## Client Session Selection

The harness should not maintain a conceptual `active_session_by_client` map.

The CLI or other UI client should track its current session locally and include `session_id` on session-targeted events, for example:

- `ui.prompt_submitted`
- `ui.prompt_draft`
- `ui.tree_request`
- `ui.navigate_tree`
- `ui.compact_request`
- `ui.cancel_prompt`
- `ui.recall_queued_prompt`
- eventually role selection/update events

This keeps focus/attachment as UI state rather than harness architecture.

## Persistence

The simplest first step is to keep the current persisted-event log shape, but shrink which runtime events are allowed into it.

The session log should contain durable conversation facts, not lifecycle/status events. For example, `extension.context_ready` should be runtime-only even though it is session-related.

Persisted events should be limited to things needed to reconstruct the conversation transcript and its durable metadata, such as:

- user messages
- assistant responses
- completed tool calls/results
- compaction facts
- context injections that affected model input
- model or role changes, if they are part of conversation history
- branch/head movement
- session metadata

This avoids introducing a second persistence record format immediately. A dedicated record format may still be useful later, but shrinking the persisted event set is likely a lower-risk migration path.

`meta.json` remains useful as sidecar metadata for listing/resume: cwd, created time, last touched time, and latest user prompt preview.

## Runtime Events

Runtime events that affect a session should carry `session_id` explicitly.

Events must not rely on an ambient current session. For example, extension context events should identify which session they answer for.

```text
session.started { session_id, cwd, role, forks_from? }
extension.context_ready { session_id, ... }
provider.response_finished { session_id, prompt_id, ... }
tool.result { session_id, call_id, ... }
```

In the initial refactor, provider/tool events may still route through prompt/call id maps, but those maps must resolve unambiguously to a session runtime.

## Forking and Side Sessions

Forking should eventually create a new independent session.

```text
session.started {
  session_id: new_session,
  cwd,
  role,
  forks_from: {
    session_id: parent_session,
    node_id: starting_node,
  },
}
```

The new session builds its initial transcript/context from the parent session up to `node_id`, then diverges independently.

Subagents and side conversations can eventually use the same mechanism:

```text
session.started {
  session_id: child_session,
  kind: "subagent",
  parent: {
    session_id: parent_session,
    tool_call_id,
  },
  forks_from: {
    session_id: parent_session,
    node_id: starting_node,
  },
}
```

For the first multi-session refactor, keep current `Conversation`-based side conversations. Moving subagents to child sessions is a separate later change.

## Consequences

The public model becomes:

```text
Harness
  many Sessions
```

Each session is independently hostable and owns the runtime state needed to advance its conversation(s).

Open design questions:

- Should forks copy parent records or reference them?
- What happens if a parent session is deleted or compacted?
- How should hidden subagent sessions appear in listing/export once subagents become sessions?
- Which context records must be snapshotted for reproducibility?
- How should existing mixed event logs migrate to a smaller persisted event set?
- Should `ConversationId` remain globally unique during the intermediate state, with a `conversation_id -> session_id` index, or become session-local?

## Smaller Preliminary Refactor

Before the full session model rework, simplify role/model knob state.

Today the harness keeps both:

- `selected_role`
- separate mutable `selected_params`

This duplicates state. The selected role plus role overrides should be enough to derive effective model parameters.

Mini-refactor:

- remove `selected_params` as independent harness state
- represent effort, service tier, verbosity, and thinking-summary changes as role override updates
- stop emitting separate knob events:
  - `harness.effort_changed`
  - `harness.service_tier_changed`
  - `harness.verbosity_changed`
  - `harness.thinking_summary_changed`
- have the UI update from role selection / role update state instead

This reduces global harness state before making sessions independently hostable.
