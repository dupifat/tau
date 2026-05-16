//! Append-only persistent prompt input history.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use tau_config::settings::TauDirs;
use tau_proto::UnixMicros;

const HISTORY_FILE: &str = "prompt-history.cbor";
const LOCK_FILE: &str = "prompt-history.lock";
const MAX_RECORD_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PROMPT_HISTORY_ENTRIES: usize = 1000;

#[derive(Clone, Debug)]
pub(crate) struct PromptHistoryStore {
    path: Option<PathBuf>,
}

#[derive(Debug, Deserialize, Serialize)]
struct PromptHistoryRecord {
    version: u8,
    recorded_at_micros: u64,
    text: String,
}

impl PromptHistoryStore {
    #[must_use]
    pub(crate) fn new(dirs: &TauDirs) -> Self {
        Self {
            path: dirs.state_dir.as_ref().map(|dir| dir.join(HISTORY_FILE)),
        }
    }

    pub(crate) fn load(&self) -> io::Result<Vec<String>> {
        let Some(path) = self.path.as_deref() else {
            return Ok(Vec::new());
        };
        load_prompt_history(path)
    }

    pub(crate) fn append(&self, text: &str) -> io::Result<()> {
        let Some(path) = self.path.as_deref() else {
            return Ok(());
        };
        if text.is_empty() {
            return Ok(());
        }
        append_prompt_history(path, text)
    }
}

fn load_prompt_history(path: &Path) -> io::Result<Vec<String>> {
    let Some(parent) = path.parent() else {
        return Ok(Vec::new());
    };
    fs::create_dir_all(parent)?;
    let lock_file = open_lock_file(parent)?;
    FileExt::lock_shared(&lock_file)?;
    let result = load_prompt_history_locked(path);
    let unlock_result = FileExt::unlock(&lock_file);
    match (result, unlock_result) {
        (Ok(entries), Ok(())) => Ok(entries),
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
    }
}

fn load_prompt_history_locked(path: &Path) -> io::Result<Vec<String>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut entries = VecDeque::new();
    loop {
        let mut length_bytes = [0_u8; 8];
        match file.read_exact(&mut length_bytes) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                tracing::warn!(
                    target: "tau_cli::prompt_history",
                    path = %path.display(),
                    "ignoring truncated prompt-history length header"
                );
                break;
            }
            Err(error) => return Err(error),
        }
        let record_length = u64::from_le_bytes(length_bytes);
        if MAX_RECORD_BYTES < record_length {
            tracing::warn!(
                target: "tau_cli::prompt_history",
                path = %path.display(),
                record_length,
                max_record_bytes = MAX_RECORD_BYTES,
                "ignoring corrupt prompt-history tail with oversized record"
            );
            break;
        }
        let mut record_bytes = vec![0_u8; record_length as usize];
        match file.read_exact(&mut record_bytes) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                tracing::warn!(
                    target: "tau_cli::prompt_history",
                    path = %path.display(),
                    record_length,
                    "ignoring truncated prompt-history tail record"
                );
                break;
            }
            Err(error) => return Err(error),
        }
        let record: PromptHistoryRecord = match ciborium::from_reader(record_bytes.as_slice()) {
            Ok(record) => record,
            Err(error) => {
                tracing::warn!(
                    target: "tau_cli::prompt_history",
                    path = %path.display(),
                    %error,
                    record_length,
                    "ignoring malformed prompt-history record"
                );
                continue;
            }
        };
        if record.version != 1 {
            tracing::warn!(
                target: "tau_cli::prompt_history",
                path = %path.display(),
                version = record.version,
                "ignoring unsupported prompt-history record"
            );
            continue;
        }
        if record.text.is_empty() {
            continue;
        }
        if MAX_PROMPT_HISTORY_ENTRIES <= entries.len() {
            entries.pop_front();
        }
        entries.push_back(record.text);
    }
    Ok(entries.into_iter().collect())
}

fn append_prompt_history(path: &Path, text: &str) -> io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    fs::create_dir_all(parent)?;
    let lock_file = open_lock_file(parent)?;
    FileExt::lock_exclusive(&lock_file)?;
    let result = append_prompt_history_locked(path, text);
    let unlock_result = FileExt::unlock(&lock_file);
    result.and(unlock_result)
}

fn append_prompt_history_locked(path: &Path, text: &str) -> io::Result<()> {
    let record = PromptHistoryRecord {
        version: 1,
        recorded_at_micros: UnixMicros::now().get(),
        text: text.to_owned(),
    };
    let mut encoded = Vec::new();
    ciborium::into_writer(&record, &mut encoded)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let encoded_len = encoded.len() as u64;
    if MAX_RECORD_BYTES < encoded_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("prompt-history record length {encoded_len} exceeds {MAX_RECORD_BYTES}"),
        ));
    }

    let mut entry = Vec::with_capacity(8 + encoded.len());
    entry.extend_from_slice(&encoded_len.to_le_bytes());
    entry.extend_from_slice(&encoded);

    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(&entry)?;
    file.flush()?;
    file.sync_data()
}

fn open_lock_file(parent: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(parent.join(LOCK_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_and_loads_prompt_history_in_order() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = PromptHistoryStore {
            path: Some(tmp.path().join(HISTORY_FILE)),
        };

        store.append("one").expect("append one");
        store.append("two\nlines").expect("append two");

        assert_eq!(store.load().expect("load"), vec!["one", "two\nlines"]);
    }

    #[test]
    fn ignores_torn_tail_record() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join(HISTORY_FILE);
        let store = PromptHistoryStore {
            path: Some(path.clone()),
        };

        store.append("kept").expect("append kept");
        let mut file = OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open history");
        file.write_all(&8_u64.to_le_bytes()).expect("write length");
        file.write_all(b"torn").expect("write partial payload");

        assert_eq!(store.load().expect("load"), vec!["kept"]);
    }

    #[test]
    fn ignores_malformed_record_and_keeps_reading() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join(HISTORY_FILE);
        let store = PromptHistoryStore {
            path: Some(path.clone()),
        };

        store.append("before").expect("append before");
        let mut file = OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open history");
        file.write_all(&4_u64.to_le_bytes()).expect("write length");
        file.write_all(b"junk").expect("write malformed payload");
        drop(file);
        store.append("after").expect("append after");

        assert_eq!(store.load().expect("load"), vec!["before", "after"]);
    }

    #[test]
    fn append_does_not_read_existing_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join(HISTORY_FILE);
        fs::write(&path, u64::MAX.to_le_bytes()).expect("write corrupt prefix");
        let store = PromptHistoryStore { path: Some(path) };

        store
            .append("new")
            .expect("append skips reading corrupt file");
    }
}
