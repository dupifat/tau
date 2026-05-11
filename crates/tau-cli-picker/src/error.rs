use std::{fmt, io};

#[derive(Debug)]
pub enum PickerError {
    Io(io::Error),
    Empty,
    NoEnabledItems,
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

impl std::error::Error for PickerError {}

impl From<io::Error> for PickerError {
    fn from(source: io::Error) -> Self {
        Self::Io(source)
    }
}
