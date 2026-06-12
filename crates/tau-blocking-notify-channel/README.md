# tau-blocking-notify-channel

A small coalescing notification channel with multiple senders and one receiver.

The channel stores one notification bit. Calling `Sender::notify` sets that bit;
`Receiver::recv` blocks until the bit is set, then resets it. Multiple
notifications before a receive coalesce into a single wakeup, so bursty producers
do not build an unbounded queue.

When every sender is dropped, the channel becomes disconnected. A pending
notification is still delivered before `recv` or `try_recv` reports
`Disconnected`.

## Why this exists

Tau uses this primitive for wakeups where only “something changed” matters, such
as terminal redraw notifications in `tau-cli-term-raw`. A standard
`std::sync::mpsc::channel::<()>()` would require draining queued wakeups to
preserve coalescing and could grow under burst load.

## Example

```rust
let (tx, rx) = tau_blocking_notify_channel::channel();

tx.notify();
assert_eq!(rx.recv(), Ok(()));
assert_eq!(rx.try_recv(), Ok(false));

drop(tx);
assert!(rx.recv().is_err());
```
