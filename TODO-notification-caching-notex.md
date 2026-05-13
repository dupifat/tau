# Notification idle-summary caching investigation notes

Session analyzed: `/home/dpc/.local/state/tau/sessions/tau-agent-5meq62`.

## Current finding

The latest idle-summary turn still broke cache efficiency.

Important events:

- `sp-28` user turn: `input_tokens=25823`, `cached_tokens=25088`, cache hit over reusable input about `97.2%`.
- `sp-29` `std-notifications` idle-summary turn: `input_tokens=25330`, `cached_tokens=3072`, cache hit over reusable input about `12.1%`.
- `sp-29` had `previous_response={ id: resp_03f744cfe2e3ed46016a04be4e9afc8195b1b81a28999243f9, message_index: 71 }`.
- `sp-29` had `share_user_cache_key=true`.
- `sp-29` used the Responses WebSocket transport.
- `sp-29` `ws_pool_delta={ upgrades: 0, silent_reconnects: 0, chain_strips_on_fresh: 0 }`.

So the obvious failures were not present: the harness did preserve a parent chain anchor, the side query opted into the user cache key, and the WebSocket pool did not report a fresh-socket chain strip or silent reconnect. Despite that, provider-reported cached tokens collapsed to the static-ish baseline.

## Interpretation

This looks like a provider/cache-key/request-shape mismatch that the current event log cannot prove.

The `session.prompt_created` event proves Tau intended to send a chained one-message continuation for the notification summary. It does not prove the exact serialized WebSocket `response.create` body that reached Codex.

The next debugging step should capture a sanitized request-shape fingerprint at the agent boundary, after `responses::build_request` and `ws_envelope`, not just at the harness boundary.

## Likely suspects

1. The actual wire body for `std-notifications` still differs from the user continuation in some non-obvious field.
2. `prompt_cache_key` may still not be identical on the wire, despite `share_user_cache_key=true` in `SessionPromptCreated`.
3. Codex may treat `previous_response_id` continuations from an extension-originated prompt differently because of a field Tau still varies, or because of backend-side state tied to something other than the visible request fields.
4. The chain may be accepted but the prompt cache may not consider the side-branch continuation cache-compatible; current counters cannot distinguish this from a wire mismatch.

## Add next

Add temporary structured diagnostics around request construction and WebSocket send, ideally behind trace/debug logging:

- `session_prompt_id`
- `originator_kind` and extension name when present
- `share_user_cache_key`
- final `prompt_cache_key` hash or full value if acceptable locally
- whether `previous_response_id` is present
- `previous_response_id` short hash/prefix
- `previous_response.message_index`
- total harness message count
- final wire `input.len()` after slicing
- final wire `tool_choice`
- hashes of stable serialized request sections:
  - full request with `input` and `previous_response_id` blanked
  - tools array
  - instructions/system prompt
  - model params fields: reasoning/text/service tier

Also emit one explicit `info` or persisted event when a non-tool extension side query is sent, comparing its request-shape hash to the most recent user turn's request-shape hash. The comparison should happen in the agent after wire-body construction, because the harness-side fingerprint is not enough.

## How to re-check

For a new session, compare the final user turn immediately before idle summary with the `std-notifications` summary turn:

```sh
python - <<'PY'
import json, sys
p = sys.argv[1]
for i, line in enumerate(open(p), 1):
    rec = json.loads(line)
    ev = rec.get('event') or {}
    if (ev.get('event') or rec.get('event_name')) != 'agent.response_finished':
        continue
    pl = ev.get('payload', {})
    if rec.get('type') == 'published':
        continue
    print(i, pl.get('session_prompt_id'), pl.get('originator'),
          'input', pl.get('input_tokens'),
          'cached', pl.get('cached_tokens'),
          'ws', pl.get('ws_pool_delta'))
PY /home/dpc/.local/state/tau/sessions/SESSION_ID/events.jsonl
```

A fixed notification summary should have cached tokens close to the previous user turn's cached tokens, not around `3072`.
