use std::collections::VecDeque;
use std::io;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{LogicalKey, PickerKey, logical_to_action, read_byte_key, terminal_key_to_logical};

struct ScriptedReader {
    steps: VecDeque<io::Result<Option<u8>>>,
}

impl ScriptedReader {
    fn new(steps: impl IntoIterator<Item = io::Result<Option<u8>>>) -> Self {
        Self {
            steps: steps.into_iter().collect(),
        }
    }
}

impl io::Read for ScriptedReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.steps.pop_front() {
            Some(Ok(Some(byte))) => {
                buf[0] = byte;
                Ok(1)
            }
            Some(Ok(None)) | None => Ok(0),
            Some(Err(err)) => Err(err),
        }
    }
}

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

/// Protects terminal-event Ctrl-C/Ctrl-D decoding so terminal input keeps the
/// same cancellation behavior as the byte-stream test reader.
#[test]
fn terminal_control_chars_decode_to_logical_cancellation_keys() {
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    let ctrl_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);

    assert_eq!(terminal_key_to_logical(ctrl_c), LogicalKey::CtrlC);
    assert_eq!(terminal_key_to_logical(ctrl_d), LogicalKey::CtrlD);
}

/// Ensures only documented plain character shortcuts are honored; unrelated
/// Ctrl/Alt modified characters should not navigate or cancel the picker.
#[test]
fn terminal_modified_character_shortcuts_are_ignored() {
    for key in ['j', 'k', 'q'] {
        let ctrl_key = KeyEvent::new(KeyCode::Char(key), KeyModifiers::CONTROL);
        let alt_key = KeyEvent::new(KeyCode::Char(key), KeyModifiers::ALT);

        assert_eq!(terminal_key_to_logical(ctrl_key), LogicalKey::Unknown);
        assert_eq!(terminal_key_to_logical(alt_key), LogicalKey::Unknown);
    }
}

/// Ensures transient read interruptions inside an escape sequence are retried
/// instead of turning a valid arrow-key sequence into a picker I/O failure.
#[test]
fn byte_reader_retries_interrupted_escape_sequence_reads() {
    let interrupted = io::Error::from(io::ErrorKind::Interrupted);
    let mut reader = ScriptedReader::new([
        Ok(Some(0x1b)),
        Err(interrupted),
        Ok(Some(b'[')),
        Err(io::Error::from(io::ErrorKind::Interrupted)),
        Ok(Some(b'B')),
    ]);

    assert_eq!(
        read_byte_key(&mut reader).expect("down arrow"),
        PickerKey::Down
    );
}
