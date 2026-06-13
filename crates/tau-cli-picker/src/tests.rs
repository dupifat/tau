use std::collections::VecDeque;
use std::io::{self, Cursor};
use std::sync::{Arc, Mutex};

use crate::key::{PickerEvent, PickerKey, read_byte_key};
use crate::{
    PickerError, PickerItem, pick_with_event_reader, pick_with_io, picker_lines, resize_dimension,
};

fn items(labels: &[&str]) -> Vec<PickerItem> {
    labels.iter().map(|l| PickerItem::enabled(*l)).collect()
}

fn run(reader_bytes: &[u8], items: &[PickerItem]) -> Result<usize, PickerError> {
    let writer = Vec::<u8>::new();
    let reader = Cursor::new(reader_bytes.to_vec());
    pick_with_io("pick", items, writer, reader)
}

#[test]
fn enter_selects_first_enabled() {
    let it = items(&["one", "two"]);
    assert_eq!(run(b"\n", &it).expect("enter picks 0"), 0);
}

#[test]
fn cr_also_selects() {
    let it = items(&["one"]);
    assert_eq!(run(b"\r", &it).expect("cr picks 0"), 0);
}

#[test]
fn space_does_not_select() {
    // Space must NOT be Enter — reserved for a possible multi-select.
    // After space the buffer ends (EOF), which the byte reader treats
    // as Cancelled, so the call should not return Ok.
    let it = items(&["one", "two"]);
    assert!(matches!(run(b" ", &it), Err(PickerError::Cancelled)));
}

#[test]
fn j_moves_down_k_moves_up() {
    let it = items(&["a", "b", "c"]);
    assert_eq!(run(b"jj\n", &it).expect("jj enter"), 2);
    assert_eq!(run(b"jjk\n", &it).expect("jjk enter"), 1);
}

#[test]
fn arrow_keys_move() {
    let it = items(&["a", "b", "c"]);
    // Down arrow = ESC [ B
    assert_eq!(run(b"\x1b[B\x1b[B\n", &it).expect("two downs"), 2);
    // Up arrow from index 2
    assert_eq!(
        run(b"\x1b[B\x1b[B\x1b[A\n", &it).expect("two downs one up"),
        1
    );
}

#[test]
fn tab_moves_down_backtab_moves_up() {
    let it = items(&["a", "b", "c"]);
    assert_eq!(run(b"\t\t\n", &it).expect("two tabs"), 2);
    // BackTab = ESC [ Z
    assert_eq!(run(b"\t\t\x1b[Z\n", &it).expect("two tabs one backtab"), 1);
}

#[test]
fn ctrl_c_cancels() {
    let it = items(&["a", "b"]);
    assert!(matches!(run(b"\x03", &it), Err(PickerError::Cancelled)));
}

#[test]
fn ctrl_d_cancels() {
    // Ctrl-D commonly signals EOF; byte-stream callers should get the same
    // cancellation result as terminal users pressing Ctrl-C or Escape.
    let it = items(&["a", "b"]);
    assert!(matches!(run(b"\x04", &it), Err(PickerError::Cancelled)));
}

#[test]
fn q_cancels() {
    let it = items(&["a", "b"]);
    assert!(matches!(run(b"q", &it), Err(PickerError::Cancelled)));
}

#[test]
fn bare_esc_cancels() {
    // ESC followed by EOF must cancel — not block, not eat phantom bytes.
    let it = items(&["a", "b"]);
    assert!(matches!(run(b"\x1b", &it), Err(PickerError::Cancelled)));
}

#[test]
fn esc_then_unrelated_byte_cancels() {
    let it = items(&["a", "b"]);
    // ESC followed by a non-`[` byte is treated as bare ESC.
    assert!(matches!(run(b"\x1bx", &it), Err(PickerError::Cancelled)));
}

#[test]
fn empty_items_errors() {
    let it: Vec<PickerItem> = Vec::new();
    assert!(matches!(run(b"\n", &it), Err(PickerError::Empty)));
}

#[test]
fn all_disabled_errors() {
    let it = vec![PickerItem::disabled("a"), PickerItem::disabled("b")];
    assert!(matches!(run(b"\n", &it), Err(PickerError::NoEnabledItems)));
}

#[test]
fn disabled_items_are_skipped() {
    let it = vec![
        PickerItem::enabled("a"),
        PickerItem::disabled("b"),
        PickerItem::enabled("c"),
    ];
    // First enabled is index 0; one j should skip the disabled to land on 2.
    assert_eq!(run(b"j\n", &it).expect("skip disabled"), 2);
    // Two js wraps back to 0.
    assert_eq!(run(b"jj\n", &it).expect("skip disabled twice"), 0);
}

#[test]
fn first_enabled_is_initial_selection() {
    let it = vec![
        PickerItem::disabled("a"),
        PickerItem::disabled("b"),
        PickerItem::enabled("c"),
    ];
    assert_eq!(run(b"\n", &it).expect("third is enabled"), 2);
}

#[test]
fn byte_reader_ignores_unknown_chars() {
    // Random printable ASCII not in the keymap → Ignored, picker keeps reading.
    let it = items(&["a", "b"]);
    assert_eq!(run(b"xy\n", &it).expect("unknown then enter"), 0);
}

#[test]
fn byte_reader_decodes_csi_arrows() {
    let mut reader = Cursor::new(b"\x1b[A".to_vec());
    assert_eq!(read_byte_key(&mut reader).expect("up arrow"), PickerKey::Up);
    let mut reader = Cursor::new(b"\x1b[B".to_vec());
    assert_eq!(
        read_byte_key(&mut reader).expect("down arrow"),
        PickerKey::Down
    );
}

#[test]
fn visible_window_centers_selection() {
    use crate::visible_window;
    // Fits entirely: full range.
    assert_eq!(visible_window(5, 2, 10), (0, 5));
    // Overflow: window slides with selection.
    assert_eq!(visible_window(20, 0, 5), (0, 5));
    assert_eq!(visible_window(20, 10, 5), (8, 13));
    assert_eq!(visible_window(20, 19, 5), (15, 20));
}

fn line_text(line: &[tau_term_screen::style::Cell]) -> String {
    line.iter().map(|cell| cell.ch).collect()
}

#[test]
fn one_row_terminal_uses_compact_frame() {
    let it = items(&["one", "two"]);
    let (lines, cursor_row) = picker_lines("pick", &it, 1, 80, 1);

    assert_eq!(lines.len(), 1);
    assert_eq!(cursor_row, 0);
    assert_eq!(line_text(&lines[0]), "> two — ? pick");
}

#[test]
fn compact_frame_prioritizes_selected_item_when_truncated() {
    let it = items(&["one", "selected-item"]);
    let (lines, cursor_row) = picker_lines("very long prompt", &it, 1, 8, 1);

    assert_eq!(cursor_row, 0);
    assert_eq!(line_text(&lines[0]), "> selec…");
}

#[test]
fn normal_terminal_uses_prompt_plus_items() {
    let it = items(&["one", "two"]);
    let (lines, cursor_row) = picker_lines("pick", &it, 1, 80, 3);

    assert_eq!(lines.len(), 3);
    assert_eq!(cursor_row, 2);
    assert_eq!(line_text(&lines[0]), "? pick");
    assert_eq!(line_text(&lines[2]), "> two");
}

#[derive(Clone, Default)]
struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl SharedWriter {
    fn bytes(&self) -> Vec<u8> {
        self.0.lock().expect("writer buffer poisoned").clone()
    }
}

impl io::Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .expect("writer buffer poisoned")
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn resize_event_redraws_without_waiting_for_key_resample() {
    let it = items(&["very long item label"]);
    let writer = SharedWriter::default();
    let output = writer.clone();
    let mut events = VecDeque::from([
        PickerEvent::Resize {
            width: 8,
            height: 3,
        },
        PickerEvent::Key(PickerKey::Enter),
    ]);
    let picked = pick_with_event_reader(
        "choose a thing",
        &it,
        writer,
        || Ok(events.pop_front().expect("test event available")),
        || (40, 5),
    )
    .expect("picker should accept after resize");

    assert_eq!(picked, 0);
    let bytes = output.bytes();
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("…"),
        "resized redraw should use narrow-width truncation: {text:?}"
    );
}

#[test]
fn zero_resize_dimensions_keep_current_size() {
    assert_eq!(resize_dimension(0, 40), 40);
    assert_eq!(resize_dimension(10, 40), 10);
}

#[test]
fn picker_clears_frame_on_input_error() {
    let it = items(&["one", "two"]);
    let writer = SharedWriter::default();
    let output = writer.clone();
    let err = pick_with_event_reader(
        "pick",
        &it,
        writer,
        || Err(io::Error::other("synthetic input error")),
        || (40, 5),
    )
    .expect_err("input error should propagate");

    assert!(matches!(err, PickerError::Io(_)));
    let bytes = output.bytes();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("[J"), "cleanup should clear frame: {text:?}");
}

#[test]
fn picker_clears_frame_on_user_cancel() {
    // Cancellation exits through a different path than input errors; keep
    // cleanup covered so aborted prompts do not leave picker rows on screen.
    let it = items(&["one", "two"]);
    let writer = SharedWriter::default();
    let output = writer.clone();
    let err = pick_with_event_reader(
        "pick",
        &it,
        writer,
        || Ok(PickerEvent::Key(PickerKey::Cancelled)),
        || (40, 5),
    )
    .expect_err("user cancellation should propagate");

    assert!(matches!(err, PickerError::Cancelled));
    let bytes = output.bytes();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("[J"), "cleanup should clear frame: {text:?}");
}
