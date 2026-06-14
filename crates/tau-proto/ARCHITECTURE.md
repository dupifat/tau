# tau-proto architecture

`tau-proto` owns Tau's shared wire data transfer objects and codec helpers. Treat every public type here as protocol surface unless it is explicitly private to tests.

## Directional messages and CBOR

Harness input and output messages are directionally typed. Keep request/response envelopes in the correct enum, and preserve existing serde names unless a migration plan updates all producers, consumers, docs, and recorded fixtures.

`encode_message` writes one self-delimiting CBOR item. `decode_message_from_slice` and the harness input/output slice helpers must decode exactly one item and reject trailing bytes; use `MessageReader` for streams of concatenated messages.

## Provider-visible tool responses

Tool result events carry raw CBOR for non-provider consumers, but provider prompt construction must render tool outputs through `ToolResponse::render()`. That render path is the central defense-in-depth normalization boundary after tool-local semantic escaping.

`ToolResponse::render()` must keep headers as safe single lines, preserve ASCII LF body separators for line-oriented records, escape other model-visible control and separator characters, and keep binary/fallback rendering bounded. This is not terminal/UI escaping; terminal renderers still need their own sanitization for display state and layout.

## Event names and routing

`Event` serde `rename` values, `EventName` constants, and `Event::name()` are one contract. When adding or renaming an event, update all three together and update `docs/events.md` when the selected guide should mention the event.

First-party event categories (`tool`, `action`, `agent`, `extension`, `provider`, `harness`, `ui`, `shell`, `session`, and `term`) are reserved for typed protocol events. `CustomEvent` names must use extension-owned categories so extension payloads cannot spoof first-party routing or policy keys.

## Validated identifiers

Wire identifiers such as `ToolName` and `ToolGroupName` are validated newtypes. Do not add default constructors that create values rejected by serde deserialization. Shared validation helpers should be kept in sync across equivalent identifier types.

## Compatibility expectations

Prefer additive optional fields with serde defaults for backward compatibility. Required fields should be intentional and covered by tests when missing data would make downstream UI, harness, or provider behavior ambiguous.
