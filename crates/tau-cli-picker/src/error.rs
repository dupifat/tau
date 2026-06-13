use std::{fmt, io};

/// Errors returned while rendering or driving an interactive picker.
#[derive(Debug)]
pub enum PickerError {
    /// Underlying terminal or stream I/O failed.
    Io(io::Error),
    /// The picker was invoked with no items.
    Empty,
    /// The picker had items, but none were selectable.
    NoEnabledItems,
    /// The user or input stream cancelled with Escape, Ctrl-C, Ctrl-D/EOF, or
    /// `q`.
    Cancelled,
}

impl fmt::Display for PickerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(source) => write!(f, "I/O error: {source}"),
            Self::Empty => f.write_str("picker has no items"),
            Self::NoEnabledItems => f.write_str("picker has no enabled items"),
            Self::Cancelled => f.write_str("picker cancelled"),
        }
    }
}
impl std::error::Error for PickerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(source) => Some(source),
            Self::Empty | Self::NoEnabledItems | Self::Cancelled => None,
        }
    }
}

impl From<io::Error> for PickerError {
    fn from(source: io::Error) -> Self {
        Self::Io(source)
    }
}
