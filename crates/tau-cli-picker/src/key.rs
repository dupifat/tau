use std::io;

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
pub(crate) enum LogicalKey {
    Up,
    Down,
    Tab,
    BackTab,
    Enter,
    Esc,
    CtrlC,
    Char(char),
    Unknown,
}

/// Single source of truth for the picker key map.
pub(crate) fn logical_to_action(key: LogicalKey) -> PickerKey {
    match key {
        LogicalKey::Up | LogicalKey::BackTab => PickerKey::Up,
        LogicalKey::Down | LogicalKey::Tab => PickerKey::Down,
        LogicalKey::Enter => PickerKey::Enter,
        LogicalKey::Esc | LogicalKey::CtrlC => PickerKey::Cancelled,
        LogicalKey::Char(c) => match c {
            'j' => PickerKey::Down,
            'k' => PickerKey::Up,
            'q' => PickerKey::Cancelled,
            _ => PickerKey::Ignored,
        },
        LogicalKey::Unknown => PickerKey::Ignored,
    }
}

/// Reads the next key from the terminal via `crossterm`.
///
/// Filters out non-`Press` events so that workspaces enabling keyboard
/// enhancement flags do not double-fire actions.
pub(crate) fn read_terminal_key() -> io::Result<PickerKey> {
    loop {
        let event = crossterm::event::read()?;
        let crossterm::event::Event::Key(key) = event else {
            continue;
        };
        if key.kind != crossterm::event::KeyEventKind::Press {
            continue;
        }
        let logical = terminal_key_to_logical(key);
        return Ok(logical_to_action(logical));
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
/// [`read_terminal_key`] via `crossterm`, which handles the ambiguity.
pub(crate) fn read_byte_key(reader: &mut impl io::Read) -> io::Result<PickerKey> {
    let mut b = [0_u8; 1];
    let n = reader.read(&mut b)?;
    if n == 0 {
        return Ok(PickerKey::Cancelled);
    }
    let logical = match b[0] {
        0x03 => LogicalKey::CtrlC,
        b'\n' | b'\r' => LogicalKey::Enter,
        b'\t' => LogicalKey::Tab,
        0x1b => read_escape_sequence(reader)?,
        byte if byte.is_ascii() && !byte.is_ascii_control() => LogicalKey::Char(byte as char),
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
