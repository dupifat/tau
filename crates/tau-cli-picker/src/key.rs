use std::io;

/// High-level picker event produced by an input reader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PickerEvent {
    Key(PickerKey),
    Resize { width: u16, height: u16 },
}

/// High-level picker action produced by a key reader.
///
/// Both the terminal-event reader and the byte-stream reader funnel
/// through [`LogicalKey`] and then [`logical_to_action`] so that the
/// "what key does what" decision lives in a single place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PickerKey {
    Up,
    Down,
    Enter,
    Cancelled,
    Ignored,
}
/// Logical key recognized by the picker, independent of how it was
/// physically encoded (a `crossterm` event or a raw byte stream).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LogicalKey {
    Up,
    Down,
    Tab,
    BackTab,
    Enter,
    Esc,
    CtrlC,
    CtrlD,
    Char(char),
    Unknown,
}

/// Single source of truth for the picker key map.
fn logical_to_action(key: LogicalKey) -> PickerKey {
    match key {
        LogicalKey::Up | LogicalKey::BackTab => PickerKey::Up,
        LogicalKey::Down | LogicalKey::Tab => PickerKey::Down,
        LogicalKey::Enter => PickerKey::Enter,
        LogicalKey::Esc | LogicalKey::CtrlC | LogicalKey::CtrlD => PickerKey::Cancelled,
        LogicalKey::Char(c) => match c {
            'j' => PickerKey::Down,
            'k' => PickerKey::Up,
            'q' => PickerKey::Cancelled,
            _ => PickerKey::Ignored,
        },
        LogicalKey::Unknown => PickerKey::Ignored,
    }
}

/// Reads the next picker event from the terminal via `crossterm`.
///
/// Filters out non-`Press` key events so that workspaces enabling keyboard
/// enhancement flags do not double-fire actions. Resize events are surfaced so
/// the picker can redraw immediately instead of waiting for the next keypress.
pub(crate) fn read_terminal_event() -> io::Result<PickerEvent> {
    loop {
        match crossterm::event::read()? {
            crossterm::event::Event::Key(key) => {
                if key.kind != crossterm::event::KeyEventKind::Press {
                    continue;
                }
                let logical = terminal_key_to_logical(key);
                return Ok(PickerEvent::Key(logical_to_action(logical)));
            }
            crossterm::event::Event::Resize(width, height) => {
                return Ok(PickerEvent::Resize { width, height });
            }
            _ => {}
        }
    }
}

fn terminal_key_to_logical(key: crossterm::event::KeyEvent) -> LogicalKey {
    use crossterm::event::{KeyCode, KeyModifiers};
    match key.code {
        KeyCode::Up => LogicalKey::Up,
        KeyCode::Down => LogicalKey::Down,
        KeyCode::Tab => LogicalKey::Tab,
        KeyCode::BackTab => LogicalKey::BackTab,
        KeyCode::Enter => LogicalKey::Enter,
        KeyCode::Esc => LogicalKey::Esc,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => LogicalKey::CtrlC,
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => LogicalKey::CtrlD,
        KeyCode::Char(c) => LogicalKey::Char(c),
        _ => LogicalKey::Unknown,
    }
}

/// Reads the next key from a byte stream (used by `pick_with_io` and tests).
///
/// A bare `0x1b` (ESC) — i.e. ESC followed by EOF or a non-`[` byte —
/// is treated as cancellation. The reader is expected to be fully
/// buffered: a real-terminal reader would block on bare-ESC because
/// `io::Read` has no timeout. Production code uses
/// [`read_terminal_event`] via `crossterm`, which handles the ambiguity.
pub(crate) fn read_byte_key(reader: &mut impl io::Read) -> io::Result<PickerKey> {
    let mut b = [0_u8; 1];
    let n = reader.read(&mut b)?;
    if n == 0 {
        return Ok(PickerKey::Cancelled);
    }
    let logical = match b[0] {
        0x03 => LogicalKey::CtrlC,
        0x04 => LogicalKey::CtrlD,
        b'\n' | b'\r' => LogicalKey::Enter,
        b'\t' => LogicalKey::Tab,
        0x1b => read_escape_sequence(reader)?,
        byte if byte.is_ascii() && !byte.is_ascii_control() => LogicalKey::Char(char::from(byte)),
        _ => LogicalKey::Unknown,
    };
    Ok(logical_to_action(logical))
}

fn read_escape_sequence(reader: &mut impl io::Read) -> io::Result<LogicalKey> {
    let mut b = [0_u8; 1];
    let n = reader.read(&mut b)?;
    if n == 0 || b[0] != b'[' {
        // Bare ESC, or ESC followed by an unrelated key — treat as cancel.
        return Ok(LogicalKey::Esc);
    }
    let n = reader.read(&mut b)?;
    if n == 0 {
        return Ok(LogicalKey::Unknown);
    }
    Ok(match b[0] {
        b'A' => LogicalKey::Up,
        b'B' => LogicalKey::Down,
        b'Z' => LogicalKey::BackTab,
        _ => LogicalKey::Unknown,
    })
}

#[cfg(test)]
mod tests;
