use super::{LogicalKey, PickerKey, logical_to_action};

/// Verifies the central logical-key mapping so terminal and byte-stream readers
/// continue to share the same controls.
#[test]
fn logical_mapping_is_single_source_of_truth() {
    assert_eq!(logical_to_action(LogicalKey::Up), PickerKey::Up);
    assert_eq!(logical_to_action(LogicalKey::Down), PickerKey::Down);
    assert_eq!(logical_to_action(LogicalKey::Tab), PickerKey::Down);
    assert_eq!(logical_to_action(LogicalKey::BackTab), PickerKey::Up);
    assert_eq!(logical_to_action(LogicalKey::Enter), PickerKey::Enter);
    assert_eq!(logical_to_action(LogicalKey::Esc), PickerKey::Cancelled);
    assert_eq!(logical_to_action(LogicalKey::CtrlC), PickerKey::Cancelled);
    assert_eq!(logical_to_action(LogicalKey::CtrlD), PickerKey::Cancelled);
    assert_eq!(logical_to_action(LogicalKey::Char('j')), PickerKey::Down);
    assert_eq!(logical_to_action(LogicalKey::Char('k')), PickerKey::Up);
    assert_eq!(
        logical_to_action(LogicalKey::Char('q')),
        PickerKey::Cancelled
    );
    assert_eq!(logical_to_action(LogicalKey::Char(' ')), PickerKey::Ignored);
}
