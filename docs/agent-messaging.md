# Agent messaging tool

The harness-owned `message` tool lets an agent send a short text note to the user or to another agent. Every sent message is recorded as an `agent.message` event and shown in the UI as:

```text
Messages from <sender> to <recipient>:
<message>
```

## Send to the user

Use the special recipient id `user`:

```text
message({"recipient_id":"user","message":"I found the root cause and am checking the fix now."})
```

On success the tool result is:

```text
Message sent
```

## Send to another agent

Start the other agent with `delegate`. The instant background placeholder includes an `agent_id` header, and the final delegate result also carries the same `agent_id` alongside the sub-agent `output`:

```text
tau_internal: true
agent_id: engineer_ab12cd34

Tool call `call_123` is running in the background.
```

Use that id as `recipient_id`:

```text
message({"recipient_id":"engineer_ab12cd34","message":"Please also inspect crates/tau-cli/src/event_renderer.rs."})
```

The UI still displays the message. The recipient agent also receives a hidden internal prompt with the message body.

## Invalid recipients and arguments

A non-`user` recipient must be a live or pending `agent_id`. Otherwise the tool fails and no `agent.message` event is emitted:

```text
message({"recipient_id":"engineer_missing","message":"hello"})
```

```text
unknown message recipient: `engineer_missing`
```

Tool arguments are schema-validated before dispatch. Unknown extra fields are rejected before any logical tool invocation is logged.
