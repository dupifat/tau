use std::{fmt, io};

use console::{Key, Term};

#[derive(Clone, Debug)]
pub struct PickerItem {
    pub label: String,
    pub enabled: bool,
}

impl PickerItem {
    pub fn enabled(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            enabled: true,
        }
    }

    pub fn disabled(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            enabled: false,
        }
    }
}

#[derive(Debug)]
pub enum PickerError {
    Io(io::Error),
    Empty,
    NoEnabledItems,
}

impl fmt::Display for PickerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(source) => write!(f, "I/O error: {source}"),
            Self::Empty => f.write_str("picker has no items"),
            Self::NoEnabledItems => f.write_str("picker has no enabled items"),
        }
    }
}

impl std::error::Error for PickerError {}

impl From<io::Error> for PickerError {
    fn from(source: io::Error) -> Self {
        Self::Io(source)
    }
}

pub fn pick(prompt: &str, items: &[PickerItem]) -> Result<usize, PickerError> {
    pick_with_term(prompt, items, &Term::stderr())
}

pub fn pick_with_term(
    prompt: &str,
    items: &[PickerItem],
    term: &Term,
) -> Result<usize, PickerError> {
    if items.is_empty() {
        return Err(PickerError::Empty);
    }
    let mut selected = items
        .iter()
        .position(|item| item.enabled)
        .ok_or(PickerError::NoEnabledItems)?;

    term.write_line(&format!("? {prompt}"))?;
    loop {
        for (idx, item) in items.iter().enumerate() {
            let marker = if !item.enabled {
                "X"
            } else if idx == selected {
                ">"
            } else {
                " "
            };
            term.write_line(&format!("{marker} {}", item.label))?;
        }
        match term.read_key()? {
            Key::ArrowDown | Key::Tab | Key::Char('j') => {
                selected = adjacent_enabled_item(items, selected, true);
            }
            Key::ArrowUp | Key::BackTab | Key::Char('k') => {
                selected = adjacent_enabled_item(items, selected, false);
            }
            Key::Enter | Key::Char(' ') => {
                term.clear_last_lines(items.len() + 1)?;
                return Ok(selected);
            }
            _ => {}
        }
        term.clear_last_lines(items.len())?;
    }
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
