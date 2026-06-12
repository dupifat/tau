# tau-harness architecture

`tau-harness` owns the daemon-side control plane for Tau sessions. It connects
clients and extensions, sequences events, applies interception, persists durable
session/agent facts, and delivers committed events to subscribers.

## Event sequencing, interception, and persistence

All ordinary event publication should flow through the central publish path:
`enqueue_publish` runs interceptors in priority order, `commit_event` stamps a
single runtime sequence/timestamp, writes debug/event-log records, persists
eligible semantic facts, and broadcasts delivery frames. Direct calls to
`commit_event` are reserved for code that has already resolved interception.

Interceptors are local privileged extensions. They can inspect, modify, or drop
most matching events before commit. The harness protects selected facts as
must-pass and immutable because live state, durable resume state, and transcript
routing must agree. Fully immutable facts include session lifecycle facts,
session membership facts, `agent.started`, harness-owned agent message
projections, terminal tool completion facts (`tool.result`, `tool.error`,
`provider.tool_result`, `provider.tool_error`, `tool.cancelled`,
`tool.background_result`, and `tool.background_error`), and selected response
closure facts such as `provider.response_finished`. Prompt text facts are
must-pass, but only their routing keys are immutable: interceptors may rewrite
text on the sanctioned prompt-text events without changing agent id, message
class, or originator. Important `harness.info` diagnostics are also published
with a call-site `must_pass` override.

## Session and agent stores

The session store owns durable membership facts such as
`session.agent_loaded` and `session.agent_unloaded`. `session.started` and
`session.shutdown` are must-pass, immutable runtime/current-session snapshot
facts, but they are not folded into the durable session membership store. Agent
stores own durable transcript facts, including `agent.started`, prompt facts,
provider/tool results, and harness-owned inter-agent message projections. Tests
should assert durable stores, not only runtime delivery, when changing durable
facts.

## Extension boundary

Extensions are less-trusted peers connected over the Tau protocol. They may
publish ordinary events through `emit`, subscribe to committed events, register
interceptors, provide tools/actions/context, and request extension-data file
operations. The harness validates source ownership for harness-owned or
provider-owned facts and rejects peer-authored lifecycle, membership,
transcript, prompt, and harness-status facts unless they arrive through the
specific API path that owns them. Interceptor replacement is intentionally
conservative: protected facts may be observed, but drops and forbidden rewrites
publish the original event so routing identities and durable folds stay aligned.
Mutable prompt-text events may be rewritten only without changing their routing
identity.

The harness also tracks loaded session membership in runtime state before the
corresponding must-pass `session.agent_loaded` publish commits. That keeps
idempotency stable while an interceptor parks publication and prevents duplicate
membership/start facts from being queued for the same live agent.

## Extension data

Extension-data RPCs confine paths to per-extension state roots, reject traversal
and symlink escapes, write private files/directories where supported, and enforce
per-file/per-directory-list quotas. Quota failures are reported as
`quota_exceeded`. These limits bound individual harness operations, not aggregate
extension disk usage across many files.

## Lifecycle events

Harness lifecycle events such as session start/shutdown and extension status are
normal events unless specifically marked must-pass/immutable. Session lifecycle
facts are protected because extensions and context providers use them to set up
or tear down per-session state. Extension lifecycle/status events are runtime
observability facts and may be intercepted like other non-protected events unless
call-site policy says otherwise.
