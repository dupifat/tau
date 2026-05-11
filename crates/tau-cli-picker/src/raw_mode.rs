use std::io;

/// Guard that enables terminal raw mode on construction and restores
/// cooked mode on drop.
///
/// Callers must not construct this while a parent component already owns
/// raw mode — the drop will silently leave the parent in cooked mode.
pub(crate) struct RawModeGuard;

impl RawModeGuard {
    pub(crate) fn enable() -> io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}
