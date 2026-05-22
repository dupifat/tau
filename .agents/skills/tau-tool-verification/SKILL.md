---
name: tau-tool-verification
description: >
  Use this skill when asked to verify Tau harness tools or tool output behavior,
  especially read, write, edit, shell, line-oriented output, truncation,
  metadata headers, UTF-8 handling, diffs, timeouts, or skill/tool conformance.
---

# Tau Tool Verification

Use when asked to verify Tau skills.

If not explicitly stated, assume the user means `read`, `write`, `edit` and `shell` tools.

## Goal

Your goal is to verify if basic Tau harness tools still work as expected,
and conform to our standards and guidelines.

## Guidelines

### Tool result output structure
All tools should return a HTTP-protocol-like structure:

```
header-1: value-1
header-2: value-2
...
header-n: value-n

multi-line-payload
```

With a single empty line separating headers from the main payload.

`multi-line-payload` can be arbitrary, but line-oriented output typically uses
`<prefix>(optional-per-line-flags) <line-content>` structure. If that's the case
the tool description should mention it.

Many headers are optional, and skipped for their default most natural values
for token efficiency.

### Common patterns

Range operations should use `<start-line>` and `<line-number>` (optional)
approach to range selection.

Newlines are assumed to be `\n`, but other styles are supported
and displayed as `crlf` (`\r\n`), `cr` (`\r`) or `no_nl` (missing trailing newline).

Lines containing invalid UTF-8 characters are skipped, and a `invalid-utf8` is displayed,
and line content is skipped to avoid mistakes and force fallback to more appropriate tools.
In similar way, lines which are too long show `truncated` flag and have content skipped.

Total outputs that are too long are truncated; `truncated: true`,
`total_lines: {lines}` and `total_bytes: {bytes}` headers are added.
These total headers are omitted when output is not truncated.

When output is truncated due to line number limit, first and last 1000 lines
should be shown with `...` line separating them, instead of usual line prefix.
If a single line would exceed the byte budget (currently 50 KB for
`read`/`shell`), show only the line prefix plus `(truncated)` rather than
partial content.


### Tool descriptions

Tool description should be short but informative. They should mention the line prefix meaning, if used in the tool. They should mention line and byte limits.


### Tool-specific guidelines

The output of `read` and `shell` is intentionally similar, and should support
the same semantics. The meaning of the line prefix is different: line number vs stdout/stderr information

`shell` tool will add `duration_seconds: {number}` header for commands that took longer
than 5s to execute. Whole-second precision is acceptable; finer precision is
not needed. Reported durations are approximate, and can include overheads and
latencies of internal components.

`shell` tool should return non-zero exits and timeouts as structured command
results with output details, not as tool invocation errors. It should reliably
timeout operations that take longer than timeout argument, but currently 100%
reliable child process termination is not implemented and will require advanced
techniques to implement in the future (e.g. cgroups).

`edit` tool produces unified-diff like output for edits made in the payload, and
allows at most 100 replacements per call, to limit amount of output it produces.
Requests for more than 100 replacements must error out immediately before making
any changes. If an edit finds no matches, the tool error should include structured
details with `changed: false` and `replacements: 0`. Hunks that would be too large
than some sanity threshold (both lines and bytes) or with invalid characters, will
be replaced with:

```
@@ -1,8 +1,8 @@
<marker>
```

Where marker is similar to ones used in tools like `read`, `shell` output.

Other commands should adhere to pre-existing conventions and naming used in
standard tools.


### Background tools and `wait`

Some tools can run in the background. The agent first receives a synthetic tool result with `kind: background_placeholder` saying:

```
tau_internal: true

Tool call `<tool_call_id>` is running in the background.
```

When the real tool finishes, Tau injects an internal, UI-hidden prompt saying:

```
[tau-internal] Tool call `<tool_call_id>` is complete.
```

The agent can then call `wait({"tool_call_id": "..."})` to collect that specific real result, or `wait({})` to wait for the first background completion in the current conversation. The no-arg form is conversation-scoped: it must not consume completions from parent, child, or sibling conversations. The tool description shown to agents often says not to call `wait` until they know the tool call has completed. This is an optimization to avoid wasting tokens: for foreground calls, the normal tool call result will arrive without an extra `wait`, and for background calls Tau will wake the agent when the tool finishes. It is not a technical requirement. The `wait` tool must work well when called for tool calls that are still running, and it must have reasonable semantics in all cases. If `wait` is used for a backgrounded call before completion, Tau suppresses that internal completion prompt while still emitting the real background result/error event. If `wait({})` consumes a completion, it suppresses the normal `[tau-internal] Tool call ... is complete.` prompt for that completion and returns an `original_tool_call_id: <tool_call_id>` provider-visible header so the agent knows which background call was collected.

Current background timing: most tools background after about 5 seconds, `delegate` backgrounds instantly, and `wait` itself never backgrounds. This may change; when verifying, report if observed behavior differs.

Slow `delegate` calls should include the same `duration_seconds` header semantics as `shell`: omit fast calls, include approximate whole seconds for calls that took longer than about 5 seconds, and allow internal overheads and jitter. Verify this both for direct background delegate results and for delegate results collected through `wait`.

A completed background result is consumed by the first successful `wait`. Later waits for the same id should fail with an already-consumed error. Parallel duplicate waits on the same id race; at most one should receive the result, and the rest should fail. Parallel duplicate no-arg waits in the same conversation should also fail clearly because only one waiter can consume the next completion. The exact error depends on timing: an in-progress duplicate-wait error, an already-consumed error, or another clear race-related error can be acceptable if only one wait receives the result.

### Background tool `cancel`

`cancel` requires `tool_call_id` and never backgrounds. It currently supports only running `delegate` tool calls. A successful cancel request returns `Tool cancelation sent`, emits a harness info event containing `tool call cancelation request`, and targets only the sub-agent spawned by that delegate call. The canceled delegate should complete as a background error so `wait` can observe the cancellation instead of hanging.

Calling `cancel` for an unknown, completed, or unsupported tool call should return a tool error. Calling it twice for the same target should return a tool error like `Tool call already canceled`.

When verifying this behavior, check that the synthetic foreground result is visible to the model, the completion notification is delivered to the model but hidden from UI unless `wait` suppressed it, and `wait` returns a completed result once and only once.


### Message tool verification plan

Use this plan when asked to verify the `message` tool, especially in multi-agent scenarios. The goal is to prove that messages are routed correctly among the main agent, sub-agents, sibling sub-agents, the special `user` recipient, and completed or invalid recipients. Also verify timing, sender IDs, exact payload preservation, and error behavior.

Do not rely on memory. Give every sub-agent a self-contained prompt. A delegated agent starts with a clean context and does not know this skill, the parent conversation, or the IDs of other agents unless you include them in its prompt or later messages.

#### What to verify

Record all of these observations:

* Main agent to sub-agent delivery.
* Multiple messages to the same live sub-agent.
* Sub-agent to sibling sub-agent delivery.
* Sub-agent to the main agent using the main agent recipient ID.
* Sub-agent to `user` delivery, noting that this may be visible in the UI but may not appear as a model-visible inbound message to the main agent.
* Main agent to itself, after the main agent recipient ID is known.
* Delivery while a sub-agent is sleeping or otherwise between model turns.
* Delivery order, or any reorderings, especially for parallel `message` calls.
* Sender IDs visible to recipients.
* Message payload preservation for multiline content, blank lines, unicode, JSON-like text, backticks, and literal `<message>` tags inside the payload.
* Error for an unknown recipient ID.
* Error for a completed sub-agent recipient ID.
* Error for an empty message.
* `delegate` and `wait` behavior around long-running sub-agents, including `duration_seconds` headers for slow delegates.

#### Phase 1: spawn two peer agents and use `user` for live reports

Start with two shared delegates. Name them Agent A and Agent B. They should stay alive long enough to receive delayed messages, log every inbound message, execute peer-send commands, and report to `user`. Reporting to `user` avoids needing the main agent recipient ID in the first phase.

Use this prompt for Agent A, replacing only the agent name where needed for Agent B:

```text
You are Agent A in a Tau message-tool verification test. Goal: verify cross-agent messaging behavior. You have a clean context; follow these instructions exactly.

Important:
- Incoming messages from the Tau `message` tool may appear as hidden prompts in your conversation. Treat every new prompt/message you see after starting as an inbound test message.
- Keep a full log of every inbound message you receive after this initial task prompt. Include exact text, apparent sender/recipient if visible, and when you noticed it.
- You may use only safe commands. Use short `sleep` commands only to stay alive and give the parent/peer time to send messages.
- If you receive a message containing `COMMAND: SEND_PEER`, parse `recipient_id={id}` and `text={text}`, then call the `message` tool to send exactly `{text}` to that recipient. Log the tool result.
- If you receive a message containing `COMMAND: REPORT`, send a `message` to `user` with your current full log.
- Do not finish early. Run four observation rounds.

Procedure:
1. Immediately send a message to `user` with exactly: `READY Agent A: started message-tool test`.
2. For rounds 1 through 4:
   a. Run `sleep 3` using the shell tool.
   b. After the sleep result, inspect any new inbound messages/prompts you have received.
   c. Execute any `COMMAND: SEND_PEER` instructions you have newly received.
   d. Send a message to `user` starting with `REPORT Agent A round {n}:` and include all newly observed inbound messages since the previous report and any message-tool actions/results. If none, say `none`.
3. Final answer: return `FINAL Agent A` plus your complete inbound-message log and all message-tool actions/results.

You are expected to receive messages from the parent and possibly from Agent B. Be precise and do not invent messages.
```

After the `delegate` placeholder results return, note both `agent_id` values and both delegate tool call IDs. Send the first batch of messages in parallel:

```text
To Agent A:
- MAIN to A direct message 1. nonce=main-a-001. Please log exact text.
- MAIN to A direct message 2. nonce=main-a-002. Please log exact text.
- COMMAND: SEND_PEER recipient_id={agent_b_id} text=PEER A to B message from Agent A. nonce=peer-a-b-001. Please log exact text.
- COMMAND: REPORT from main to Agent A after initial sends. nonce=report-a-001.

To Agent B:
- MAIN to B direct message 1. nonce=main-b-001. Please log exact text.
- MAIN to B direct message 2. nonce=main-b-002. Please log exact text.
- COMMAND: SEND_PEER recipient_id={agent_a_id} text=PEER B to A message from Agent B. nonce=peer-b-a-001. Please log exact text.
- COMMAND: REPORT from main to Agent B after initial sends. nonce=report-b-001.
```

Sleep for about four seconds in the main agent, then send a delayed batch in parallel:

```text
To Agent A:
- MAIN to A delayed direct message 3. nonce=main-a-003. Please log exact text.
- COMMAND: SEND_PEER recipient_id={agent_b_id} text=PEER A to B delayed message from Agent A. nonce=peer-a-b-002. Please log exact text.
- COMMAND: REPORT from main to Agent A after delayed sends. nonce=report-a-002.

To Agent B:
- MAIN to B delayed direct message 3. nonce=main-b-003. Please log exact text.
- COMMAND: SEND_PEER recipient_id={agent_a_id} text=PEER B to A delayed message from Agent B. nonce=peer-b-a-002. Please log exact text.
- COMMAND: REPORT from main to Agent B after delayed sends. nonce=report-b-002.
```

Also send one message to a clearly invalid recipient such as `engineer_does_not_exist_message_validation`; expect a tool error with the unknown recipient ID and echoed message fields.

Wait for both delegates. In their final logs, verify that:

* Each agent saw the direct main-agent messages addressed to it.
* Each agent saw the peer message from the other agent.
* Each `COMMAND: SEND_PEER` caused exactly one peer `message` call with result `Message sent`.
* Delayed messages arrived even though the agents were already running.
* The visible sender ID for messages from the main agent is present. Save that sender ID; it is the main agent recipient ID for the next phase.

After both delegates complete, try to send a post-completion message to each old `agent_id`. Expect an error. Current behavior may report this the same way as an unknown recipient.

#### Phase 2: verify sub-agent to main-agent routing

Use the main agent recipient ID learned from Phase 1. Spawn two fresh shared delegates, Agent C and Agent D. These agents should report back to the main agent recipient ID, not to `user`. This proves that parent-directed messages are delivered as model-visible `[tau-internal]` inbound messages to the main agent.

Use this prompt for Agent C, replacing only the agent name where needed for Agent D and filling `{main_agent_id}` with the ID learned in Phase 1:

```text
You are Agent C in a second Tau message-tool verification test. Parent/main agent recipient_id is `{main_agent_id}`. Goal: verify messages among parent, Agent C, and Agent D.

Rules:
- Incoming `message` tool messages may appear as hidden prompts. Log every inbound message you receive after this initial task prompt, with exact text and visible sender id.
- For every report, use the `message` tool to send to `recipient_id={main_agent_id}` (the parent/main agent), not `user`, unless the parent message fails. If it fails, log the failure and continue.
- If an inbound message contains `COMMAND: SEND_PEER recipient_id={id} text={text}`, send exactly `{text}` to `{id}` using the `message` tool and log the result.
- If an inbound message contains `COMMAND: REPORT_PARENT`, immediately message your current log to `{main_agent_id}`.
- Stay alive for three observation rounds using `sleep 2` each round. Do not finish early.

Procedure:
1. Send to `{main_agent_id}`: `READY Agent C to parent. nonce=ready-c-parent-001`.
2. Repeat three rounds: sleep 2 seconds; inspect new inbound messages; execute any SEND_PEER commands; message the parent with `REPORT Agent C round {n}:` plus new inbound messages and actions since previous report, or `none`.
3. Final answer: `FINAL Agent C` plus complete inbound log and all message-tool actions/results.
```

After the `delegate` placeholders return, send this batch in parallel:

```text
To Agent C:
- MAIN to C direct message. nonce=main-c-001. Please log exact text and sender id.
- COMMAND: SEND_PEER recipient_id={agent_d_id} text=PEER C to D from Agent C. nonce=peer-c-d-001. Please log exact text.
- COMMAND: REPORT_PARENT nonce=report-c-parent-001.

To Agent D:
- MAIN to D direct message. nonce=main-d-001. Please log exact text and sender id.
- COMMAND: SEND_PEER recipient_id={agent_c_id} text=PEER D to C from Agent D. nonce=peer-d-c-001. Please log exact text.
- COMMAND: REPORT_PARENT nonce=report-d-parent-001.
```

The main agent should receive `[tau-internal]` inbound messages from each sub-agent. Record whether the sender ID in those inbound messages matches the sub-agent `agent_id`. Sleep for about three seconds, then send one delayed direct message to each agent:

```text
To Agent C:
- MAIN to C delayed message. nonce=main-c-002. Please log exact text and sender id.

To Agent D:
- MAIN to D delayed message. nonce=main-d-002. Please log exact text and sender id.
```

Wait for both delegates. Verify that their final logs match the parent-visible reports already received by the main agent.

After both complete, again send post-completion messages to both old `agent_id` values and expect errors.

#### Phase 3: verify self, content, and simple validation errors

After the main agent recipient ID is known, send a message from the main agent to itself. Expect a model-visible `[tau-internal]` inbound message whose sender is the same main agent ID and whose payload is exact.

Then send a multiline self-message like this:

```text
MULTILINE self content probe. nonce=self-main-002
line 2 unicode: café 🚀

line 4 xml-ish: <message>inner</message> & chars
line 5 code-ish: `backticks` and {"json":true}
```

Verify that blank lines, unicode, backticks, JSON-like text, ampersands, and literal inner `<message>` tags are preserved. Note whether the wrapper around delivered messages makes inner tags confusing.

Finally, call `message` with an empty string to a valid recipient. Expect a tool error such as `` `message` must not be empty ``. Also verify an unknown recipient error if it was not already checked in Phase 1.

#### Reporting format for `message` verification

Report concise but complete findings:

* List each tested route and whether it passed: main to child, child to child, child to parent, child to `user`, main to self, invalid recipient, completed recipient, empty payload, rich content payload.
* Include exact unexpected errors or output.
* Mention any timing surprises, missed messages, duplicate messages, or ordering uncertainty.
* Include whether `message` success output had enough metadata. Current expected success output is only `Message sent`.
* Include whether errors distinguish completed recipients from unknown recipients. Current behavior may use the same unknown-recipient error for both.
* Include whether parent recipient ID discovery was clear or had to be inferred from sub-agent logs.
* Include whether the delivered wrapper preserved but visually confused payloads containing literal `<message>` tags.


### Verification procedure

Create a scratch directory in `/tmp` for your experiments and always avoid dangerous or disruptive actions during testing.

For every tool thoroughly consider all corner cases, including ones which are not covered
in this document.

Report back:

* discrepancies between this document and actual usage,
* things that are wrong, confusing, inconsistent or unclear in both this document and actual tool output
* ideas for improvements both in the tool behavior and this document
