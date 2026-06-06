# Message reference

Messages are point-to-point protocol traffic between one peer (extension,
provider, or UI client) and the harness. They are **not** bus facts: messages
are never broadcast as events, never written to the durable semantic event logs,
and not matched directly by event subscriptions.

Wire form: `{"message": "<flat_name>", "payload": {...}}` — flat snake_case
names, distinct from events' dotted `category.call` form. The protocol is
directional:

- [`HarnessInputMessage`](../crates/tau-proto/src/messages.rs): messages the
  harness accepts from peers.
- [`HarnessOutputMessage`](../crates/tau-proto/src/messages.rs): messages the
  harness sends to peers.

Bare top-level `Event` values are not valid protocol items. Peers ask the
harness to publish events with `emit`; the harness delivers events to peers with
`deliver`.

Type definitions live in
[`crates/tau-proto/src/messages.rs`](../crates/tau-proto/src/messages.rs). For
bus events themselves, see [events.md](events.md).

## Handshake

Exchanged when a peer first connects to the harness. The usual extension order
is: peer sends `hello`, then optional `subscribe` and `intercept`; the harness
sends `configure` for supervised extensions; the peer sends `ready` once setup
is done.

- **`hello`** *(peer → harness)* — A participant announces itself just after
  connecting: protocol version, client name, and client kind (`provider` /
  `tool` / `action` / `ui` / `core` / `external`). First message on every
  connection.
- **`subscribe`** *(peer → harness)* — A peer declares which delivered events it
  wants to receive, as a list of selectors (exact name or prefix). Without a
  subscription, only directed traffic reaches the peer.
- **`intercept`** *(peer → harness)* — A peer asks to receive matching event
  emissions before they hit the event log, with a priority. Lower priority runs
  first; each interceptor replies with `intercept_reply` to pass, rewrite, or
  drop the offered emission.
- **`ready`** *(peer → harness)* — Sent by an extension after its own startup
  work is done and it is ready to participate in tool dispatch. The harness
  supervisor reacts by emitting the `extension.ready` *event* on the bus so
  subscribers can observe online state without watching every per-component
  pipe.
- **`disconnect`** *(either direction)* — A peer or the harness signals an
  intentional disconnect, with an optional human-readable reason. Distinct from
  a socket dying unannounced. The writer thread also sends this as a best-effort
  sentinel when shutting an extension's stdin.

## Configuration (harness → extension)

- **`configure`** — Sent point-to-point by the harness to one extension
  immediately after that extension's `hello`. Carries whatever the `config: { …
  }` value was for that extension in `harness.yaml`, the extension state
  directory when available, and authorized secrets. In-process extensions don't
  carry a supervised config and receive the empty default.
- **`config_error`** *(extension → harness)* — An extension reports back that the
  `configure` payload it received was malformed or unusable; the harness
  surfaces the message just like a `harness.yaml` parse error so the user can
  see why their per-extension config was rejected.

## Emission and interception (peer ↔ harness)

These messages wrap a real bus `Event` while it is entering the harness. They
are messages — not events — because the wrapper is point-to-point protocol
metadata, not the fact subscribers ultimately observe.

- **`emit`** *(peer → harness)* — A peer's request to publish an event. Carries
  the inner event and a `transient` flag controlling whether eligible semantic
  facts should skip durable history. The harness owns source attribution,
  interception, sequencing, persistence, and eventual delivery.
- **`intercept_request`** *(harness → interceptor)* — Directed delivery of an
  emission that has not reached the event log yet. Carries the offered event and
  the same transient metadata.
- **`intercept_reply`** *(interceptor → harness)* — Exactly one response to an
  `intercept_request`: `pass` unchanged, `pass` with a replacement event, or
  `drop`.

## Transport (event delivery and acknowledgement)

The harness wraps every event it sends to a peer in `deliver`. Receivers ack
only committed runtime deliveries; direct snapshots and replay deliveries are
unsequenced and must not be acked. The harness does not retain the runtime event
stream in memory; late UI catch-up is reconstructed from session/agent stores
and current harness snapshots, and extension subscriptions remain live-only
unless a future explicit replay mode is added.

- **`deliver`** *(harness → peer)* — Harness-owned event delivery envelope:
  `EventDelivery { event, seq, recorded_at }`. `seq: Some(_)` means an ackable
  committed runtime delivery. `seq: None` means a direct, replay, or otherwise
  unsequenced delivery. `recorded_at` is present when a runtime or historical
  timestamp is meaningful.
- **`ack`** *(peer → harness)* — Cumulative acknowledgement that the receiver has
  processed all ackable deliveries with sequence `<= up_to`. Newer acks
  supersede older ones; duplicate or out-of-order acks are ignored by the
  harness.