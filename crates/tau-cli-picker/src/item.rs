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
