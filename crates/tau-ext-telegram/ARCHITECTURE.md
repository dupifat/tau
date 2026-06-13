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

Allowed users can use `/agents`, `/select <agent>`, `/to <agent> <message>`, or
plain text when exactly one agent is registered or a selected agent exists.
Ambiguous plain text receives a Telegram reply and is not routed.
