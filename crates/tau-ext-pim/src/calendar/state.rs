use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const LOG_SCHEMA: u32 = 1;

/// One persisted calendar audit log entry.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct CalendarLogEntry {
    /// Log schema version.
    pub(crate) schema: u32,
    /// Wall-clock timestamp in milliseconds since the Unix epoch.
    pub(crate) ts_unix_ms: u64,
    /// Log entry kind, currently `tool`.
    pub(crate) kind: String,
    /// Calendar tool command name.
    pub(crate) command: String,
    /// Command outcome status.
    pub(crate) status: String,
    /// Configured account id when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) account: Option<String>,
    /// Calendar id when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) calendar: Option<String>,
    /// Event id when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) event_id: Option<String>,
    /// Lower RFC3339 time bound from the tool call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) time_min: Option<String>,
    /// Upper RFC3339 time bound from the tool call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) time_max: Option<String>,
    /// Requested row limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) limit: Option<u32>,
    /// Number of rows or records returned when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) item_count: Option<u64>,
    /// Sanitized error reason for failed commands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
}

impl CalendarLogEntry {
    /// Build a schema-v1 tool audit entry.
    pub(crate) fn tool(command: &str, status: &str) -> Self {
        Self {
            schema: LOG_SCHEMA,
            ts_unix_ms: current_unix_millis(),
            kind: "tool".to_owned(),
            command: command.to_owned(),
            status: status.to_owned(),
            account: None,
            calendar: None,
            event_id: None,
            time_min: None,
            time_max: None,
            limit: None,
            item_count: None,
            reason: None,
        }
    }
}

/// Calendar module persistent state under the extension state directory.
pub(crate) struct StateStore {
    state_dir: PathBuf,
}

impl StateStore {
    /// Open the calendar state area and create required private directories.
    pub(crate) fn open(state_dir: PathBuf) -> Result<Self, String> {
        create_private_dir_all(&state_dir)?;
        create_private_dir_all(&state_dir.join("logs"))?;
        Ok(Self { state_dir })
    }

    /// Append one calendar audit log entry.
    pub(crate) fn append_calendar_log(&self, entry: &CalendarLogEntry) -> Result<(), String> {
        let path = self.calendar_log_path();
        let mut bytes = serde_json::to_vec(entry).map_err(|error| error.to_string())?;
        bytes.push(b'\n');
        let mut file = open_private_append(&path)
            .map_err(|error| format!("failed to open {}: {error}", path.display()))?;
        file.write_all(&bytes)
            .map_err(|error| format!("failed to append {}: {error}", path.display()))?;
        file.sync_data()
            .map_err(|error| format!("failed to sync {}: {error}", path.display()))
    }

    /// Load recent calendar audit log entries in chronological order.
    pub(crate) fn recent_calendar_log(
        &self,
        limit: usize,
    ) -> Result<Vec<CalendarLogEntry>, String> {
        let path = self.calendar_log_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let bytes = read_sensitive_file(&path)
            .map_err(|error| format!("failed to open {}: {error}", path.display()))?;
        let mut entries = VecDeque::new();
        for line in BufReader::new(bytes.as_slice()).lines() {
            let line =
                line.map_err(|error| format!("failed to read {}: {error}", path.display()))?;
            if line.trim().is_empty() {
                continue;
            }
            let Ok(entry) = serde_json::from_str::<CalendarLogEntry>(&line) else {
                continue;
            };
            if entry.schema != LOG_SCHEMA {
                continue;
            }
            entries.push_back(entry);
            while limit < entries.len() {
                entries.pop_front();
            }
        }
        Ok(entries.into_iter().collect())
    }

    fn calendar_log_path(&self) -> PathBuf {
        self.state_dir.join("logs").join("calendar.jsonl")
    }
}

fn current_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn create_private_dir_all(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|error| error.to_string())?;
    chmod_private_dir(path)
}

fn read_sensitive_file(path: &Path) -> Result<Vec<u8>, String> {
    chmod_private_file(path)?;
    fs::read(path).map_err(|error| error.to_string())
}

fn open_private_append(path: &Path) -> Result<fs::File, std::io::Error> {
    let mut options = fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(path)?;
    chmod_private_file_handle(&file)?;
    Ok(file)
}

#[cfg(unix)]
fn chmod_private_dir(path: &Path) -> Result<(), String> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("failed to chmod {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn chmod_private_dir(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn chmod_private_file(path: &Path) -> Result<(), String> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("failed to chmod {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn chmod_private_file(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn chmod_private_file_handle(file: &fs::File) -> Result<(), std::io::Error> {
    file.set_permissions(fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn chmod_private_file_handle(_file: &fs::File) -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn file_mode(path: &Path) -> u32 {
        fs::metadata(path).expect("metadata").permissions().mode() & 0o777
    }

    #[test]
    fn recent_calendar_log_ignores_invalid_entries_and_keeps_limit() {
        // Logs can be manually truncated or edited during debugging. Invalid
        // lines should not break `/calendar log last`, and the tail limit should
        // still be enforced.
        let temp = tempfile::TempDir::new().expect("tempdir");
        let state = StateStore::open(temp.path().join("state")).expect("state");
        state
            .append_calendar_log(&CalendarLogEntry::tool("list_events", "ok"))
            .expect("append first");
        fs::write(
            temp.path().join("state/logs/calendar.jsonl"),
            b"not-json\n{\"schema\":2}\n{\"schema\":1,\"ts_unix_ms\":2,\"kind\":\"tool\",\"command\":\"read_event\",\"status\":\"ok\"}\n{\"schema\":1,\"ts_unix_ms\":3,\"kind\":\"tool\",\"command\":\"free_busy\",\"status\":\"ok\"}\n",
        )
        .expect("rewrite log");

        let entries = state.recent_calendar_log(1).expect("recent log");

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].command, "free_busy");
    }

    #[cfg(unix)]
    #[test]
    fn calendar_log_files_are_owner_only() {
        // Calendar logs contain schedule metadata. Existing permissive paths
        // must be tightened on both append and read paths.
        let temp = tempfile::TempDir::new().expect("tempdir");
        let state_dir = temp.path().join("state");
        fs::create_dir_all(state_dir.join("logs")).expect("mkdir");
        fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o755)).expect("chmod state");
        let log_path = state_dir.join("logs/calendar.jsonl");
        fs::write(&log_path, b"").expect("log");
        fs::set_permissions(&log_path, fs::Permissions::from_mode(0o644)).expect("chmod log");

        let state = StateStore::open(state_dir.clone()).expect("state");
        state
            .append_calendar_log(&CalendarLogEntry::tool("list_events", "ok"))
            .expect("append log");
        assert_eq!(file_mode(&state_dir), 0o700);
        assert_eq!(file_mode(&state_dir.join("logs")), 0o700);
        assert_eq!(file_mode(&log_path), 0o600);

        fs::set_permissions(&log_path, fs::Permissions::from_mode(0o644)).expect("chmod log");
        let entries = state.recent_calendar_log(1).expect("recent log");
        assert_eq!(entries.len(), 1);
        assert_eq!(file_mode(&log_path), 0o600);
    }
}
