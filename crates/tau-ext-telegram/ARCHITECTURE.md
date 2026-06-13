# tau-ext-telegram architecture

`std-telegram` is a personal text bridge, not a generic chat abstraction. The
extension process starts to register tools, but it does not contact Telegram
until a Tau agent calls `telegram_register(enabled: true)`.

## State

Runtime state is intentionally in memory: registered agents, labels, selected
agent per chat, learned chat id, and update offset are forgotten when the
extension restarts. The first long poll after lazy startup drains Telegram's
existing backlog without routing it, so pre-registration messages are not
submitted as fresh prompts.

## Harness boundary

Incoming Telegram text is emitted as `extension.prompt_submit_request`. The
harness validates the target loaded agent and owns the resulting durable
`agent.prompt_submitted` fact. This extension must not publish transcript prompt
facts directly.

## Routing

Allowed users can use these commands:

- `/agents`
- `/select <agent-id-or-prefix>`
- `/to <agent-id-or-prefix> <message>`

Plain text routes when exactly one agent is registered or a selected agent exists.
Command designators always put the stable `agent_id` first, with display name
only as context in listings and selection confirmations (`agent_id (display
name)`). `/select` and `/to` resolve by full `agent_id` or unambiguous `agent_id`
prefix, not by display name. Agent replies sent with `telegram_send` are prefixed
with `[agent_id]` only. Ambiguous plain text receives a Telegram reply and is not
routed.
