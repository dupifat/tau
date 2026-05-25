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

Tool outputs with non-trivial fields encoded into line-oriented
payloads should include a `format` header describing field order and names.
For example, an email listing can use:

```text
format: uid date from flags access attachments subject...

6212 2016-04-23T17:32:52Z builds@travis-ci.org seen,redacted preview 0 Hi there, from us
```

The `...` suffic on last field in the format is used to indicate it's a multi-word field that will extend till the end of the line.

Tool implementation must take care ensuring newlines and special characters are stripped from field values, and empty values use some placeholders (e.g. `-`) to avoid breaking the meaning of each line.

Many headers are optional, and skipped for their default most natural values
for token efficiency. Keep tool output compact: include only non-default,
non-redundant values that help the agent decide what to do next. Do not emit
aliases or duplicate fields that carry the same information.

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

`cancel` requires `tool_call_id` and never backgrounds. It supports running `delegate` calls and should support running `shell` calls. A successful cancel request returns `Tool cancellation requested`, emits a harness info event containing `tool call cancellation request`, and targets only the requested tool call. Cancellation is async and best effort: the success result only means Tau accepted the request, not that the child process or agent has already stopped. A canceled delegate should complete as a background error so `wait` can observe the cancellation instead of hanging. A canceled shell call should also complete through `wait`, include timing headers if it ran longer than about 5 seconds, and must not keep running to normal `status: 0` completion.

Calling `cancel` for an unknown, completed, or unsupported tool call should return a tool error. Unknown ids should be distinguished from already-completed ids. Calling it twice for the same target should return a tool error like `Tool call already canceled`.

When verifying this behavior, check that the synthetic foreground result is visible to the model, the completion notification is delivered to the model when no wait is active, and `wait` returns a completed result once and only once. Completion prompt suppression is only expected when a matching `wait` is already active before the background call finishes. If the tool finishes first and Tau shows `[tau-internal] Tool call ... is complete.`, a later `wait` can still consume the result and that earlier prompt is not a bug.


### Cancel tool verification plan

Use this plan when asked to verify the `cancel` tool, especially around background `delegate` calls, `wait`, duplicate requests, and leaked work from a canceled sub-agent. The goal is to prove that cancel targets exactly one running delegate call, reports success or errors clearly, and leaves no orphaned tool completions behind.

Do not rely on memory. Give every sub-agent a self-contained prompt. A delegated agent starts with a clean context and does not know this skill, the parent conversation, or the IDs of other agents unless you include them in its prompt or later messages.

Create a scratch directory in `/tmp`, such as `/tmp/tau-cancel-verification.*`, before running shell probes. Keep all sleeps short except where a background transition or leak check requires a longer wait.

#### What to verify

Record all of these observations:

* The delegate placeholder includes `tau_internal: true`, `self_agent_id`, `sub_agent_id`, and the background delegate tool call ID.
* `cancel` must be called with the delegate `tool_call_id`, not the `sub_agent_id`.
* A successful cancel returns exactly `Tool cancellation requested` and does not background.
* The harness emits a `HarnessInfo` event containing `tool call cancellation request` if event logs are available.
* The canceled delegate produces a background error that `wait` can collect.
* `wait({"tool_call_id": id})` returns the canceled result once and only once.
* `wait({})` can collect a canceled completion and includes `original_tool_call_id`.
* Waiting before the delegate has completed suppresses the later model-visible completion prompt. Waiting after a completion prompt was already delivered is still valid, but does not retroactively suppress that prompt.
* Duplicate cancel requests race cleanly: one succeeds, later or parallel ones fail with `Tool call already canceled` or another clear duplicate error.
* Canceling an unknown id, completed delegate id, unsupported running tool id, empty id, or `sub_agent_id` returns a tool error.
* Canceling one delegate does not cancel a sibling delegate.
* Canceling a long-running shell call works and does not let the command complete normally.
* Slow canceled delegates and shell calls include `duration_seconds` after about 5 seconds. A few seconds of timing overhead is normal and not worth reporting by itself.
* A canceled delegate does not leak completions from its own in-flight or backgrounded inner tool calls into the parent conversation.
* The user-visible UI does not show hidden internal completion prompts unless the current UI settings intentionally expose them.

#### Phase 1: running delegate happy path

Start a shared delegate with this prompt:

```text
You are a Tau cancel-tool verification sub-agent. Goal: stay alive until the parent cancels this delegate call.

Procedure:
1. Immediately send a message to `user` exactly: `READY cancel-ready-probe: entering long sleep`.
2. Run `sleep 60` using the shell tool.
3. If you are not canceled, final answer exactly: `UNEXPECTED cancel-ready-probe completed without cancellation`.

Do not do anything else.
```

After the placeholder result returns, record `self_agent_id`, `sub_agent_id`, and the delegate tool call ID. Call `cancel` with that delegate tool call ID. Expect the foreground result to be exactly:

```text
Tool cancellation requested
```

Then wait for the same tool call ID. Expect a background tool error like:

```text
error: Tool call canceled
self_agent_id: ...
sub_agent_id: ...
```

Call `wait` for the same ID again. Expect an already-consumed error. Call `cancel` for the same ID again. Expect `Tool call already canceled`.

#### Phase 2: no-arg wait and wait suppression

Start another long-sleeping delegate. Cancel it, then call `wait({})`. Expect the canceled error and an `original_tool_call_id` header matching the delegate call ID.

Start a third long-sleeping delegate. Call `cancel` and `wait({"tool_call_id": id})` in parallel or as close together as possible. Expect `wait` to return the canceled result. The later `[tau-internal] Tool call ... is complete.` prompt for that same call should be suppressed. If the prompt still appears after `wait` was already active for that call, record it as a discrepancy. If the completion prompt appears before the wait call is active, do not count it as a suppression failure.

#### Phase 3: invalid targets and duplicate requests

Verify each error case independently:

* `cancel({"tool_call_id": ""})` returns `` `tool_call_id` must not be empty ``.
* A clearly unknown call ID returns `Unknown tool call id` and echoes `tool_call_id`.
* A completed delegate ID returns `Tool call is already done`.
* A `sub_agent_id` returns `Unknown tool call id`; this proves the tool wants the delegate call ID.
* Two parallel `cancel` calls for the same live delegate produce one success and one duplicate-cancel error.

For the completed-delegate case, spawn a delegate that immediately returns:

```text
You are a Tau cancel-tool verification sub-agent. Return immediately with exactly: `FINAL cancel-completed-probe normal completion`.
```

Wait until the completion prompt arrives, then try to cancel it. After that, call `wait` and verify the normal final answer is still available once.

#### Phase 4: running shell cancellation

Start a shell command long enough to background, such as `sleep 20`. When the shell placeholder gives a tool call ID, call `cancel` for that ID. Expect the foreground result to be exactly `Tool cancellation requested`.

Then call `wait` for the shell call. Expect a canceled or terminated result, not a normal `status: 0` completion. If the command ran longer than about 5 seconds, verify the result includes a `duration_seconds` header. If `cancel` rejects the shell call as not cancellable, or if `wait` later returns normal `status: 0`, record this as a discrepancy because shell cancellation is expected to work.

#### Phase 5: target isolation

Start two delegates in parallel. The target should sleep for a long time. The survivor should sleep briefly and return `FINAL cancel-survivor unaffected`.

Cancel only the target delegate. Then wait for both IDs. Expect:

* Target: `error: Tool call canceled`.
* Survivor: normal final answer.

Any sibling cancellation, missing survivor result, or cross-talk between IDs is a bug.

#### Phase 6: slow cancellation and duration

Start a long-sleeping delegate. Let it run long enough to cross the delegate duration threshold, usually about 6 seconds. Cancel it and wait for the result. Expect the canceled delegate result to include `duration_seconds` with an approximate whole-second value.

Do not require an exact duration. Internal overhead and scheduling can add a few seconds of jitter; do not report small delays by themselves.

#### Phase 7: nested and inner-tool leak check

This phase is important. A canceled delegate can have its own foreground or background tool call in flight. Canceling the delegate must not leave an orphaned inner tool completion that later wakes the parent conversation.

Start a shared delegate with this prompt:

```text
You are a Tau cancel-tool verification sub-agent for inner-tool leak testing. Goal: start an inner tool call, then be canceled by the parent.

Procedure:
1. Run `sleep 12` using the shell tool.
2. If you are not canceled, final answer exactly: `UNEXPECTED cancel-inner-tool-leak completed without cancellation`.

Do not send messages. Do not do anything else.
```

Let the delegate run long enough for the inner shell call to background, usually about 6 seconds. Then cancel the delegate and wait for the delegate result. Expect `error: Tool call canceled`.

After the delegate cancel result is consumed, watch for stray completion prompts for any other tool call ID, especially the inner shell call. If a stray `[tau-internal] Tool call ... is complete.` prompt appears, call `wait` for that ID and record the full result. Treat this as a leak unless there is a clear documented reason it belongs to the parent conversation.

If no stray completion appears before the inner `sleep 12` would have finished, record that no leak was observed. This check caught a prior manual discrepancy where a canceled delegate's inner `sleep` later produced a parent-visible completion.

#### Optional event-log checks

If you have direct access to harness event logs, verify:

* Successful cancel emitted `HarnessInfo` with `tool call cancellation request`.
* The canceled delegate emitted `ToolBackgroundError` with `Tool call canceled`.
* No `SessionPromptSteered` or queued pending prompt remains for canceled nested delegate completions.
* Completed results are consumed once, and the consumed result is not available to later `wait` calls.

#### Reporting format for `cancel` verification

Report concise but complete findings:

* List each tested route and whether it passed: running delegate, no-arg wait, wait suppression, duplicate cancel, unknown id, empty id, completed delegate, shell cancellation, unsupported non-shell tool, `sub_agent_id`, sibling isolation, slow duration, and inner-tool leak.
* Include exact unexpected errors or output.
* Mention any timing surprises, missed completion prompts, duplicate prompts, leaked inner completions, or ordering uncertainty.
* Confirm the `cancel` success output is only `Tool cancellation requested`; it is an async, best-effort request, not a delivery receipt for child cleanup.
* Include whether errors distinguish completed delegates from unknown ids.
* Include whether the `delegate` placeholder made the target ID clear enough, and whether `self_agent_id` and `sub_agent_id` were present without redundant aliases.
* Include whether slow canceled delegates reported `duration_seconds`.
* Include whether the UI hid completion prompts that should be hidden, or whether that could not be directly verified.


### Message tool verification plan

Use this plan when asked to verify the `message` tool, especially in multi-agent scenarios. The goal is to prove that messages are routed correctly among the main agent, sub-agents, sibling sub-agents, the special `user` recipient, and completed or invalid recipients. Also verify timing, sender IDs, async delivery, payload escaping in hidden prompts, exact payload preservation in durable `AgentMessage` events, and error behavior.

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
* Message payload preservation in durable events, and XML escaping in hidden prompts, for multiline content, blank lines, unicode, JSON-like text, backticks, and literal `<message>` tags inside the payload.
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

After the `delegate` placeholder results return, note the caller `self_agent_id`, each `sub_agent_id`, and both delegate tool call IDs. Use `sub_agent_id` as the message recipient. Send the first batch of messages in parallel:

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

* The delegate placeholder and final result expose `self_agent_id` and `sub_agent_id` without redundant aliases.
* Each agent saw the direct main-agent messages addressed to it.
* Each agent saw the peer message from the other agent.
* Each `COMMAND: SEND_PEER` caused exactly one peer `message` call with result `Message sent`.
* Delayed messages arrived even though the agents were already running.
* The visible sender ID for messages from the main agent is present and matches the `self_agent_id` from the delegate result. Save that sender ID; it is the main agent recipient ID for the next phase.

After both delegates complete, try to send a post-completion message to each old `sub_agent_id`. Expect an error until completed-agent wakeup is implemented. Current behavior may report this the same way as an unknown recipient.

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

The main agent should receive `[tau-internal]` inbound messages from each sub-agent. Record whether the sender ID in those inbound messages matches the sub-agent `sub_agent_id`. Sleep for about three seconds, then send one delayed direct message to each agent:

```text
To Agent C:
- MAIN to C delayed message. nonce=main-c-002. Please log exact text and sender id.

To Agent D:
- MAIN to D delayed message. nonce=main-d-002. Please log exact text and sender id.
```

Wait for both delegates. Verify that their final logs match the parent-visible reports already received by the main agent.

After both complete, again send post-completion messages to both old `sub_agent_id` values and expect errors until completed-agent wakeup is implemented.

#### Phase 3: verify self, content, and simple validation errors

After the main agent recipient ID is known, send a message from the main agent to itself. Expect a model-visible `[tau-internal]` inbound message whose sender is the same main agent ID and whose payload is exact.

Then send a multiline self-message like this:

```text
MULTILINE self content probe. nonce=self-main-002
line 2 unicode: café 🚀

line 4 xml-ish: <message>inner</message> & chars
line 5 code-ish: `backticks` and {"json":true}
```

Verify that blank lines, unicode, backticks, and JSON-like text remain readable, and that ampersands plus literal inner `<message>` tags are XML-escaped inside the delivered wrapper. If you inspect durable `AgentMessage` events, verify that the stored payload is still exact and unescaped.

Finally, call `message` with an empty string to a valid recipient. Expect a tool error such as `` `message` must not be empty ``. Also verify an unknown recipient error if it was not already checked in Phase 1.

#### Reporting format for `message` verification

Report concise but complete findings:

* List each tested route and whether it passed: main to child, child to child, child to parent, child to `user`, main to self, invalid recipient, completed recipient, empty payload, rich content payload.
* Include exact unexpected errors or output.
* Mention any timing surprises, missed messages, duplicate messages, or ordering uncertainty.
* Confirm the `message` success output is only `Message sent`; delivery is async, so no delivery receipt is expected.
* Include whether errors distinguish completed recipients from unknown recipients. Current behavior may use the same unknown-recipient error for both.
* Include whether parent recipient ID discovery was clear from `self_agent_id` or still had to be inferred from sub-agent logs.
* Include whether the delivered wrapper XML-escaped payloads containing literal `<message>` tags and ampersands.


### Verification procedure

Create a scratch directory in `/tmp` for your experiments and always avoid dangerous or disruptive actions during testing.

For every tool thoroughly consider all corner cases, including ones which are not covered
in this document.

Report back:

* discrepancies between this document and actual usage,
* things that are wrong, confusing, inconsistent or unclear in both this document and actual tool output
* ideas for improvements both in the tool behavior and this document
