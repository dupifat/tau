# tau-cli-picker

`tau-cli-picker` is a small synchronous single-select terminal picker. It is intended for short external selection flows, not as an embedded widget inside Tau's main terminal UI.

## Terminal ownership

The public entry points have different ownership contracts:

- `pick` enables raw mode and renders to stderr.
- `pick_with_writer` enables raw mode and renders to a caller-provided writer.
- `pick_with_io` does not manage raw mode and is intended for tests or simple byte-stream hosts.

Embedded crossterm/TUI integration still needs a new public API that accepts host-provided events, resize notifications, and size samples.

## Rendering model

The picker builds a pure list of styled rows, then asks `tau-term-screen::Screen` to diff that frame to the terminal. Resize events are part of the picker event stream and cause an immediate erase, width update, and redraw.

For normal terminal heights, row 0 is the prompt and the remaining rows show a centered window of items. For a one-row terminal, the picker switches to a compact single-row frame containing both prompt and selected item, so it never intentionally renders more rows than the reported height.

## Scope

This crate owns only picker-local state: selected item, viewport window, and frame cleanup. It should not grow general TUI concepts such as async event loops, background redraw threads, application actions, or nested widget composition without first changing the public API to model terminal ownership explicitly.
