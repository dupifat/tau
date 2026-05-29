//! Synchronous, blocking, single-select terminal list picker.
//!
//! Three entry points are exposed:
//!
//! - [`pick`] — full convenience. Enables raw mode and writes to `stderr`. Must
//!   not be called from inside a TUI that already owns raw mode.
//! - [`pick_with_writer`] — enables raw mode but writes the picker frame to a
//!   caller-provided writer.
//! - [`pick_with_io`] — does *not* manage raw mode. Intended for tests or
//!   non-terminal hosts that drive the picker via in-memory streams.

mod error;
mod item;
mod key;
mod raw_mode;

#[cfg(test)]
mod tests;

use std::io;

use tau_term_screen::screen::Screen;
use tau_term_screen::style::StyledText;
use tau_term_screen::truncate_to_width;

pub use crate::error::PickerError;
pub use crate::item::PickerItem;
use crate::key::{PickerKey, read_byte_key, read_terminal_key};
use crate::raw_mode::RawModeGuard;

/// Prompts the user to pick one of `items`, rendering to `stderr`.
///
/// Enables terminal raw mode for the duration of the call. The picker
/// must therefore not be invoked while another component already owns
/// raw mode — the inner [`Drop`] would silently restore cooked mode and
/// strand the parent. Use [`pick_with_io`] if the caller manages raw
/// mode itself.
///
/// The picker frame is written to `stderr` (fd 2) so that the typical
/// `cli | tool` pipe shape leaves `stdout` untouched.
pub fn pick(prompt: &str, items: &[PickerItem]) -> Result<usize, PickerError> {
    let _raw = RawModeGuard::enable()?;
    pick_with_key_reader(prompt, items, io::stderr(), read_terminal_key)
}

/// Like [`pick`], but writes the picker frame to `writer`.
///
/// Enables terminal raw mode for the duration of the call; the same
/// caveat as [`pick`] applies.
pub fn pick_with_writer(
    prompt: &str,
    items: &[PickerItem],
    writer: impl io::Write,
) -> Result<usize, PickerError> {
    let _raw = RawModeGuard::enable()?;
    pick_with_key_reader(prompt, items, writer, read_terminal_key)
}

/// Drives the picker against caller-provided IO.
///
/// Does **not** toggle terminal raw mode. Intended for tests and for
/// hosts that already own the terminal. `reader` should produce key
/// presses as bytes; see `crate::key` for the supported encoding.
pub fn pick_with_io(
    prompt: &str,
    items: &[PickerItem],
    writer: impl io::Write,
    mut reader: impl io::Read,
) -> Result<usize, PickerError> {
    pick_with_key_reader(prompt, items, writer, || read_byte_key(&mut reader))
}

fn pick_with_key_reader(
    prompt: &str,
    items: &[PickerItem],
    mut writer: impl io::Write,
    mut read_key: impl FnMut() -> io::Result<PickerKey>,
) -> Result<usize, PickerError> {
    if items.is_empty() {
        return Err(PickerError::Empty);
    }
    let mut selected = items
        .iter()
        .position(|item| item.enabled)
        .ok_or(PickerError::NoEnabledItems)?;
    let (mut width, mut height) = terminal_size();
    let mut screen = Screen::new(width);

    render(&mut screen, &mut writer, prompt, items, selected, height)?;
    loop {
        match read_key()? {
            PickerKey::Down => selected = adjacent_enabled_item(items, selected, true),
            PickerKey::Up => selected = adjacent_enabled_item(items, selected, false),
            PickerKey::Enter => {
                screen.update(&mut writer, &[], (0, 0))?;
                return Ok(selected);
            }
            PickerKey::Cancelled => {
                screen.update(&mut writer, &[], (0, 0))?;
                return Err(PickerError::Cancelled);
            }
            PickerKey::Ignored => {}
        }
        // Resample on each render so terminal resizes are honored.
        let (new_width, new_height) = terminal_size();
        if new_width != width {
            screen.set_width(new_width);
            width = new_width;
        }
        height = new_height;
        render(&mut screen, &mut writer, prompt, items, selected, height)?;
    }
}

fn render(
    screen: &mut Screen,
    writer: &mut impl io::Write,
    prompt: &str,
    items: &[PickerItem],
    selected: usize,
    terminal_height: usize,
) -> io::Result<()> {
    // Reserve one row for the prompt; leave at least one item visible
    // even on absurdly short terminals.
    let visible = terminal_height.saturating_sub(1).max(1);
    let (start, end) = visible_window(items.len(), selected, visible);

    let width = screen.width();
    let mut lines = Vec::with_capacity(end - start + 1);
    lines.push(StyledText::from(truncate_to_width(&format!("? {prompt}"), width)).to_cells());
    for (idx, item) in items.iter().enumerate().take(end).skip(start) {
        let marker = if !item.enabled {
            'X'
        } else if idx == selected {
            '>'
        } else {
            ' '
        };
        let line = truncate_to_width(&format!("{marker} {}", item.label), width);
        lines.push(StyledText::from(line).to_cells());
    }
    let cursor_row = selected - start + 1;
    screen.update(writer, &lines, (cursor_row, 0))
}

/// Returns `[start, end)` of the items to render, ensuring `selected`
/// is within view.
fn visible_window(total: usize, selected: usize, visible: usize) -> (usize, usize) {
    if total <= visible {
        return (0, total);
    }
    let half = visible / 2;
    let mut start = selected.saturating_sub(half);
    if start + visible > total {
        start = total - visible;
    }
    (start, start + visible)
}

fn adjacent_enabled_item(items: &[PickerItem], selected: usize, forward: bool) -> usize {
    for offset in 1..items.len() {
        let idx = if forward {
            (selected + offset) % items.len()
        } else {
            (selected + items.len() - offset) % items.len()
        };
        if items[idx].enabled {
            return idx;
        }
    }
    selected
}

fn terminal_size() -> (usize, usize) {
    crossterm::terminal::size().map_or((80, 24), |(w, h)| (w.into(), h.into()))
}
