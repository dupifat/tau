use std::io::Cursor;

use crate::key::{LogicalKey, PickerKey, logical_to_action, read_byte_key};
use crate::{PickerError, PickerItem, pick_with_io};

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
fn logical_mapping_is_single_source_of_truth() {
    // Sanity-check a few mappings to lock down the contract.
    assert_eq!(logical_to_action(LogicalKey::Up), PickerKey::Up);
    assert_eq!(logical_to_action(LogicalKey::Down), PickerKey::Down);
    assert_eq!(logical_to_action(LogicalKey::Tab), PickerKey::Down);
    assert_eq!(logical_to_action(LogicalKey::BackTab), PickerKey::Up);
    assert_eq!(logical_to_action(LogicalKey::Enter), PickerKey::Enter);
    assert_eq!(logical_to_action(LogicalKey::Esc), PickerKey::Cancelled);
    assert_eq!(logical_to_action(LogicalKey::CtrlC), PickerKey::Cancelled);
    assert_eq!(logical_to_action(LogicalKey::Char('j')), PickerKey::Down);
    assert_eq!(logical_to_action(LogicalKey::Char('k')), PickerKey::Up);
    assert_eq!(
        logical_to_action(LogicalKey::Char('q')),
        PickerKey::Cancelled
    );
    assert_eq!(logical_to_action(LogicalKey::Char(' ')), PickerKey::Ignored);
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
