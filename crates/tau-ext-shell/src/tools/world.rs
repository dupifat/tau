//! Low-level filesystem/process boundary for ext-shell tools.
//!
//! VCR replay substitutes primitive outside-world operations, not final tool
//! results, so tool parsing, formatting, truncation, and validation still run
//! during replay.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tau_proto::{CborValue, ToolUseState};

use crate::display::ToolFailure;

const CASSETTE_VERSION: u32 = 1;

pub(crate) struct ShellWorld {
    mode: WorldMode,
}

enum WorldMode {
    Real,
    Recording {
        store: tau_vcr::VcrStore,
        key: String,
        cassette: WorldCassette,
    },
    Replay {
        key: String,
        cassette: WorldCassette,
        next_op: usize,
    },
}

impl ShellWorld {
    #[cfg(test)]
    pub(crate) fn real() -> Self {
        Self {
            mode: WorldMode::Real,
        }
    }

    pub(crate) fn for_tool(
        tool_name: &str,
        call_id: &str,
        arguments: &CborValue,
        config: Option<tau_vcr::VcrConfig>,
    ) -> Result<Self, ToolFailure> {
        let Some(config) = config else {
            return Ok(Self {
                mode: WorldMode::Real,
            });
        };
        let key = call_id.to_owned();
        let store = config.store();
        let request = world_request(tool_name, arguments)?;
        if let Some(cassette) = store.get::<WorldCassette>(&key).map_err(vcr_failure)? {
            validate_cassette(&key, &cassette, &request)?;
            return Ok(Self {
                mode: WorldMode::Replay {
                    key,
                    cassette,
                    next_op: 0,
                },
            });
        }
        if config.mode == tau_vcr::VcrMode::ReplayOnly {
            return Err(vcr_failure(tau_vcr::VcrError::Missing { key }));
        }
        Ok(Self {
            mode: WorldMode::Recording {
                store,
                key,
                cassette: WorldCassette {
                    version: CASSETTE_VERSION,
                    request,
                    ops: Vec::new(),
                },
            },
        })
    }

    pub(crate) fn finish(self) -> Result<(), ToolFailure> {
        match self.mode {
            WorldMode::Real => Ok(()),
            WorldMode::Recording {
                store,
                key,
                cassette,
            } => store.put(&key, &cassette).map_err(vcr_failure),
            WorldMode::Replay {
                key,
                cassette,
                next_op,
            } => {
                if next_op == cassette.ops.len() {
                    Ok(())
                } else {
                    Err(ToolFailure::new(format!(
                        "vcr replay for {key} left {} unconsumed world op(s)",
                        cassette.ops.len() - next_op
                    )))
                }
            }
        }
    }

    pub(crate) fn is_dir(&mut self, path: &Path) -> io::Result<bool> {
        match &mut self.mode {
            WorldMode::Real => Ok(std::fs::metadata(path)?.is_dir()),
            WorldMode::Recording { cassette, .. } => {
                let result = std::fs::metadata(path).map(|metadata| metadata.is_dir());
                cassette.ops.push(WorldOp::IsDir {
                    path: cassette_path(path),
                    result: OpResult::from_io_result_ref(&result),
                });
                result
            }
            WorldMode::Replay {
                key,
                cassette,
                next_op,
            } => {
                let op = next_replay_op(key, cassette, next_op, "is_dir", path)?;
                let WorldOp::IsDir {
                    path: expected_path,
                    result,
                } = op
                else {
                    return Err(unexpected_replay_op(key, "is_dir", path));
                };
                check_replay_path(key, "is_dir", expected_path, path)?;
                result.clone().into_io_result()
            }
        }
    }

    pub(crate) fn read_dir(&mut self, path: &Path) -> io::Result<Vec<WorldDirEntry>> {
        match &mut self.mode {
            WorldMode::Real => {
                let mut entries = Vec::new();
                for entry in std::fs::read_dir(path)? {
                    let entry = entry?;
                    entries.push(WorldDirEntry {
                        name: os_str_name(&entry.file_name()),
                        is_dir: entry.file_type()?.is_dir(),
                    });
                }
                Ok(entries)
            }
            WorldMode::Recording { cassette, .. } => {
                let result = (|| {
                    let mut entries = Vec::new();
                    for entry in std::fs::read_dir(path)? {
                        let entry = entry?;
                        entries.push(WorldDirEntry {
                            name: os_str_name(&entry.file_name()),
                            is_dir: entry.file_type()?.is_dir(),
                        });
                    }
                    Ok(entries)
                })();
                cassette.ops.push(WorldOp::ReadDir {
                    path: cassette_path(path),
                    result: OpResult::from_io_result_ref(&result).map_ok(|entries| {
                        entries.iter().map(RecordedDirEntry::from_world).collect()
                    }),
                });
                result
            }
            WorldMode::Replay {
                key,
                cassette,
                next_op,
            } => {
                let op = next_replay_op(key, cassette, next_op, "read_dir", path)?;
                let WorldOp::ReadDir {
                    path: expected_path,
                    result,
                } = op
                else {
                    return Err(unexpected_replay_op(key, "read_dir", path));
                };
                check_replay_path(key, "read_dir", expected_path, path)?;
                result.clone().into_io_result().map(|entries| {
                    entries
                        .into_iter()
                        .map(RecordedDirEntry::into_world)
                        .collect()
                })
            }
        }
    }

    pub(crate) fn read_file(&mut self, path: &Path) -> io::Result<Vec<u8>> {
        match &mut self.mode {
            WorldMode::Real => std::fs::read(path),
            WorldMode::Recording { cassette, .. } => {
                let result = std::fs::read(path);
                cassette.ops.push(WorldOp::ReadFile {
                    path: cassette_path(path),
                    result: OpResult::from_io_result_ref(&result)
                        .map_ok(tau_vcr::EscapedBytes::new),
                });
                result
            }
            WorldMode::Replay {
                key,
                cassette,
                next_op,
            } => {
                let op = next_replay_op(key, cassette, next_op, "read_file", path)?;
                let WorldOp::ReadFile {
                    path: expected_path,
                    result,
                } = op
                else {
                    return Err(unexpected_replay_op(key, "read_file", path));
                };
                check_replay_path(key, "read_file", expected_path, path)?;
                result
                    .clone()
                    .map_ok(|bytes| bytes.into_vec())
                    .into_io_result()
            }
        }
    }

    pub(crate) fn write_file(&mut self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        match &mut self.mode {
            WorldMode::Real => std::fs::write(path, bytes),
            WorldMode::Recording { cassette, .. } => {
                let result = std::fs::write(path, bytes);
                cassette.ops.push(WorldOp::WriteFile {
                    path: cassette_path(path),
                    bytes: tau_vcr::EscapedBytes::new(bytes),
                    result: OpResult::from_io_result_ref(&result),
                });
                result
            }
            WorldMode::Replay {
                key,
                cassette,
                next_op,
            } => {
                let op = next_replay_op(key, cassette, next_op, "write_file", path)?;
                let WorldOp::WriteFile {
                    path: expected_path,
                    bytes: expected_bytes,
                    result,
                } = op
                else {
                    return Err(unexpected_replay_op(key, "write_file", path));
                };
                check_replay_path(key, "write_file", expected_path, path)?;
                if expected_bytes.as_slice() != bytes {
                    return Err(replay_io_error(format!(
                        "vcr replay for {key} expected write_file({}) with {} byte(s) but got {} byte(s)",
                        path.display(),
                        expected_bytes.as_slice().len(),
                        bytes.len()
                    )));
                }
                result.clone().into_io_result()
            }
        }
    }

    pub(crate) fn path_exists(&mut self, path: &Path) -> io::Result<bool> {
        match &mut self.mode {
            WorldMode::Real => match std::fs::metadata(path) {
                Ok(_) => Ok(true),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(error),
            },
            WorldMode::Recording { cassette, .. } => {
                let result = match std::fs::metadata(path) {
                    Ok(_) => Ok(true),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
                    Err(error) => Err(error),
                };
                cassette.ops.push(WorldOp::PathExists {
                    path: cassette_path(path),
                    result: OpResult::from_io_result_ref(&result),
                });
                result
            }
            WorldMode::Replay {
                key,
                cassette,
                next_op,
            } => {
                let op = next_replay_op(key, cassette, next_op, "path_exists", path)?;
                let WorldOp::PathExists {
                    path: expected_path,
                    result,
                } = op
                else {
                    return Err(unexpected_replay_op(key, "path_exists", path));
                };
                check_replay_path(key, "path_exists", expected_path, path)?;
                result.clone().into_io_result()
            }
        }
    }

    pub(crate) fn create_dir_all(&mut self, path: &Path) -> io::Result<()> {
        match &mut self.mode {
            WorldMode::Real => std::fs::create_dir_all(path),
            WorldMode::Recording { cassette, .. } => {
                let result = std::fs::create_dir_all(path);
                cassette.ops.push(WorldOp::CreateDirAll {
                    path: cassette_path(path),
                    result: OpResult::from_io_result_ref(&result),
                });
                result
            }
            WorldMode::Replay {
                key,
                cassette,
                next_op,
            } => {
                let op = next_replay_op(key, cassette, next_op, "create_dir_all", path)?;
                let WorldOp::CreateDirAll {
                    path: expected_path,
                    result,
                } = op
                else {
                    return Err(unexpected_replay_op(key, "create_dir_all", path));
                };
                check_replay_path(key, "create_dir_all", expected_path, path)?;
                result.clone().into_io_result()
            }
        }
    }

    pub(crate) fn read_to_string(&mut self, path: &Path) -> io::Result<String> {
        String::from_utf8(self.read_file(path)?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    pub(crate) fn remove_file(&mut self, path: &Path) -> io::Result<()> {
        match &mut self.mode {
            WorldMode::Real => std::fs::remove_file(path),
            WorldMode::Recording { cassette, .. } => {
                let result = std::fs::remove_file(path);
                cassette.ops.push(WorldOp::RemoveFile {
                    path: cassette_path(path),
                    result: OpResult::from_io_result_ref(&result),
                });
                result
            }
            WorldMode::Replay {
                key,
                cassette,
                next_op,
            } => {
                let op = next_replay_op(key, cassette, next_op, "remove_file", path)?;
                let WorldOp::RemoveFile {
                    path: expected_path,
                    result,
                } = op
                else {
                    return Err(unexpected_replay_op(key, "remove_file", path));
                };
                check_replay_path(key, "remove_file", expected_path, path)?;
                result.clone().into_io_result()
            }
        }
    }
    pub(crate) fn replay_shell_outcome(
        &mut self,
    ) -> Result<Option<WorldShellOutcome>, ToolFailure> {
        match &mut self.mode {
            WorldMode::Real | WorldMode::Recording { .. } => Ok(None),
            WorldMode::Replay {
                key,
                cassette,
                next_op,
            } => {
                let Some(op) = cassette.ops.get(*next_op) else {
                    return Err(ToolFailure::new(format!(
                        "vcr replay for {key} expected shell outcome but cassette ended"
                    )));
                };
                *next_op += 1;
                let WorldOp::Shell { outcome } = op else {
                    return Err(ToolFailure::new(format!(
                        "vcr replay for {key} expected shell outcome but found different op"
                    )));
                };
                Ok(Some(outcome.clone()))
            }
        }
    }

    pub(crate) fn record_shell_outcome(&mut self, outcome: WorldShellOutcome) {
        if let WorldMode::Recording { cassette, .. } = &mut self.mode {
            cassette.ops.push(WorldOp::Shell { outcome });
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct WorldDirEntry {
    pub(crate) name: tau_vcr::EscapedBytes,
    pub(crate) is_dir: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum WorldShellOutcome {
    Finished {
        result: CborValue,
        display: Box<ToolUseState>,
        elapsed_ms: u64,
    },
    Cancelled,
}

#[cfg(unix)]
fn os_str_name(value: &std::ffi::OsStr) -> tau_vcr::EscapedBytes {
    use std::os::unix::ffi::OsStrExt;

    tau_vcr::EscapedBytes::new(value.as_bytes())
}

#[cfg(not(unix))]
fn os_str_name(value: &std::ffi::OsStr) -> tau_vcr::EscapedBytes {
    tau_vcr::EscapedBytes::new(value.to_string_lossy().as_bytes())
}

fn cassette_path(path: &Path) -> String {
    if let Ok(cwd) = std::env::current_dir()
        && let Ok(relative) = path.strip_prefix(&cwd)
    {
        if relative.as_os_str().is_empty() {
            return ".".to_owned();
        }
        return relative.display().to_string();
    }
    path.display().to_string()
}

fn next_replay_op<'a>(
    key: &str,
    cassette: &'a WorldCassette,
    next_op: &mut usize,
    op_name: &str,
    path: &Path,
) -> io::Result<&'a WorldOp> {
    let Some(op) = cassette.ops.get(*next_op) else {
        return Err(replay_io_error(format!(
            "vcr replay for {key} expected {op_name}({}) but cassette ended",
            path.display()
        )));
    };
    *next_op += 1;
    Ok(op)
}

fn unexpected_replay_op(key: &str, op_name: &str, path: &Path) -> io::Error {
    replay_io_error(format!(
        "vcr replay for {key} expected {op_name}({}) but found different op",
        path.display()
    ))
}

fn check_replay_path(
    key: &str,
    op_name: &str,
    expected_path: &str,
    actual_path: &Path,
) -> io::Result<()> {
    let actual_path = cassette_path(actual_path);
    if expected_path == actual_path {
        return Ok(());
    }
    Err(replay_io_error(format!(
        "vcr replay for {key} expected {op_name}({expected_path}) but got {op_name}({actual_path})"
    )))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct WorldCassette {
    version: u32,
    request: WorldRequest,
    ops: Vec<WorldOp>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
struct WorldRequest {
    tool: String,
    arguments: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum WorldOp {
    IsDir {
        path: String,
        result: OpResult<bool>,
    },
    ReadDir {
        path: String,
        result: OpResult<Vec<RecordedDirEntry>>,
    },
    ReadFile {
        path: String,
        result: OpResult<tau_vcr::EscapedBytes>,
    },
    WriteFile {
        path: String,
        bytes: tau_vcr::EscapedBytes,
        result: OpResult<()>,
    },
    PathExists {
        path: String,
        result: OpResult<bool>,
    },
    CreateDirAll {
        path: String,
        result: OpResult<()>,
    },
    RemoveFile {
        path: String,
        result: OpResult<()>,
    },
    Shell {
        outcome: WorldShellOutcome,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "status", content = "value", rename_all = "snake_case")]
enum OpResult<T> {
    Ok(T),
    Err(WorldIoError),
}

impl<T> OpResult<T> {
    fn from_io_result_ref(result: &io::Result<T>) -> Self
    where
        T: Clone,
    {
        match result {
            Ok(value) => Self::Ok(value.clone()),
            Err(error) => Self::Err(WorldIoError::from_io_error(error)),
        }
    }

    fn map_ok<U>(self, f: impl FnOnce(T) -> U) -> OpResult<U> {
        match self {
            Self::Ok(value) => OpResult::Ok(f(value)),
            Self::Err(error) => OpResult::Err(error),
        }
    }

    fn into_io_result(self) -> io::Result<T> {
        match self {
            Self::Ok(value) => Ok(value),
            Self::Err(error) => Err(error.into_io_error()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct WorldIoError {
    kind: String,
    message: String,
}

impl WorldIoError {
    fn from_io_error(error: &io::Error) -> Self {
        Self {
            kind: format!("{:?}", error.kind()),
            message: error.to_string(),
        }
    }

    fn into_io_error(self) -> io::Error {
        io::Error::new(io_error_kind(&self.kind), self.message)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RecordedDirEntry {
    name: tau_vcr::EscapedBytes,
    is_dir: bool,
}

impl RecordedDirEntry {
    fn from_world(entry: &WorldDirEntry) -> Self {
        Self {
            name: entry.name.clone(),
            is_dir: entry.is_dir,
        }
    }

    fn into_world(self) -> WorldDirEntry {
        WorldDirEntry {
            name: self.name,
            is_dir: self.is_dir,
        }
    }
}

fn world_request(tool_name: &str, arguments: &CborValue) -> Result<WorldRequest, ToolFailure> {
    let arguments = serde_json::to_value(arguments).map_err(|error| {
        ToolFailure::new(format!("failed to serialize vcr tool arguments: {error}"))
    })?;
    Ok(WorldRequest {
        tool: tool_name.to_owned(),
        arguments,
    })
}

fn validate_cassette(
    key: &str,
    cassette: &WorldCassette,
    request: &WorldRequest,
) -> Result<(), ToolFailure> {
    if cassette.version != CASSETTE_VERSION {
        return Err(vcr_failure(tau_vcr::VcrError::UnsupportedVersion {
            key: key.to_owned(),
            version: cassette.version,
        }));
    }
    if &cassette.request != request {
        return Err(vcr_failure(tau_vcr::request_mismatch(
            key,
            &cassette.request,
            request,
        )));
    }
    Ok(())
}

fn vcr_failure(error: tau_vcr::VcrError) -> ToolFailure {
    ToolFailure::new(format!("vcr error: {error}"))
}

fn replay_io_error(message: String) -> io::Error {
    io::Error::other(message)
}

fn io_error_kind(kind: &str) -> io::ErrorKind {
    match kind {
        "NotFound" => io::ErrorKind::NotFound,
        "PermissionDenied" => io::ErrorKind::PermissionDenied,
        "AlreadyExists" => io::ErrorKind::AlreadyExists,
        "InvalidInput" => io::ErrorKind::InvalidInput,
        "InvalidData" => io::ErrorKind::InvalidData,
        "TimedOut" => io::ErrorKind::TimedOut,
        "WriteZero" => io::ErrorKind::WriteZero,
        "Interrupted" => io::ErrorKind::Interrupted,
        "UnexpectedEof" => io::ErrorKind::UnexpectedEof,
        _ => io::ErrorKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use tau_proto::CborValue;

    use super::*;

    fn ls_args(path: &std::path::Path) -> CborValue {
        CborValue::Map(vec![(
            CborValue::Text("path".to_owned()),
            CborValue::Text(path.display().to_string()),
        )])
    }

    #[test]
    fn ls_vcr_records_world_ops_and_replays_through_tool_logic() {
        let real_dir = tempfile::TempDir::new().expect("real dir");
        std::fs::write(real_dir.path().join("beta"), "b").expect("write beta");
        std::fs::create_dir(real_dir.path().join("alpha")).expect("create alpha");
        let cassette_dir = tempfile::TempDir::new().expect("cassette dir");
        let args = ls_args(real_dir.path());

        let mut recording = ShellWorld::for_tool(
            "ls",
            "call_ls",
            &args,
            Some(tau_vcr::VcrConfig::new(
                tau_vcr::VcrMode::RecordIfMissing,
                cassette_dir.path(),
            )),
        )
        .expect("recording world");
        let recorded = crate::tools::ls::run_ls(&args, &mut recording).expect("recorded ls");
        recording.finish().expect("record cassette");
        std::fs::remove_file(real_dir.path().join("beta")).expect("remove live file");
        std::fs::remove_dir(real_dir.path().join("alpha")).expect("remove live dir");

        let mut replay = ShellWorld::for_tool(
            "ls",
            "call_ls",
            &args,
            Some(tau_vcr::VcrConfig::new(
                tau_vcr::VcrMode::ReplayOnly,
                cassette_dir.path(),
            )),
        )
        .expect("replay world");
        let replayed = crate::tools::ls::run_ls(&args, &mut replay).expect("replayed ls");
        replay.finish().expect("consume replay ops");

        assert_eq!(replayed.result, recorded.result);
        let cassette = std::fs::read_to_string(cassette_dir.path().join("call_ls.yaml"))
            .expect("read cassette");
        assert!(cassette.contains("op: is_dir"));
        assert!(cassette.contains("op: read_dir"));
        assert!(cassette.contains("name: alpha"));
        assert!(!cassette.contains("kind: utf8"));
        assert!(!cassette.contains("value: alpha"));
        assert!(!cassette.contains("1 alpha/"));
    }
    #[test]
    fn read_vcr_replays_file_bytes_through_read_logic() {
        let real_dir = tempfile::TempDir::new().expect("real dir");
        let file = real_dir.path().join("file.txt");
        std::fs::write(&file, b"alpha\n\xFFbeta").expect("write file");
        let cassette_dir = tempfile::TempDir::new().expect("cassette dir");
        let args = CborValue::Map(vec![
            (
                CborValue::Text("path".to_owned()),
                CborValue::Text(file.display().to_string()),
            ),
            (
                CborValue::Text("start_line".to_owned()),
                CborValue::Integer(2.into()),
            ),
        ]);

        let mut recording = ShellWorld::for_tool(
            "read",
            "call_read",
            &args,
            Some(tau_vcr::VcrConfig::new(
                tau_vcr::VcrMode::RecordIfMissing,
                cassette_dir.path(),
            )),
        )
        .expect("recording world");
        let recorded = crate::tools::read::read_file(&args, &mut recording).expect("recorded read");
        recording.finish().expect("record cassette");
        std::fs::write(&file, b"changed").expect("change live file");

        let mut replay = ShellWorld::for_tool(
            "read",
            "call_read",
            &args,
            Some(tau_vcr::VcrConfig::new(
                tau_vcr::VcrMode::ReplayOnly,
                cassette_dir.path(),
            )),
        )
        .expect("replay world");
        let replayed = crate::tools::read::read_file(&args, &mut replay).expect("replayed read");
        replay.finish().expect("consume replay ops");

        assert_eq!(replayed.result, recorded.result);
        let cassette = std::fs::read_to_string(cassette_dir.path().join("call_read.yaml"))
            .expect("read cassette");
        assert!(cassette.contains("op: read_file"));
        assert!(cassette.contains("\\uDCFFbeta"));
        assert!(!cassette.contains("- 255"));
    }
    #[test]
    fn edit_vcr_replay_asserts_write_without_mutating_live_file() {
        let real_dir = tempfile::TempDir::new().expect("real dir");
        let file = real_dir.path().join("file.txt");
        std::fs::write(&file, b"one\ntwo\n").expect("write file");
        let cassette_dir = tempfile::TempDir::new().expect("cassette dir");
        let args = CborValue::Map(vec![
            (
                CborValue::Text("path".to_owned()),
                CborValue::Text(file.display().to_string()),
            ),
            (
                CborValue::Text("edits".to_owned()),
                CborValue::Array(vec![CborValue::Map(vec![
                    (
                        CborValue::Text("after_line".to_owned()),
                        CborValue::Integer(1.into()),
                    ),
                    (
                        CborValue::Text("before_line".to_owned()),
                        CborValue::Integer(3.into()),
                    ),
                    (
                        CborValue::Text("newText".to_owned()),
                        CborValue::Text("TWO\n".to_owned()),
                    ),
                    (
                        CborValue::Text("guard".to_owned()),
                        CborValue::Text("two".to_owned()),
                    ),
                ])]),
            ),
        ]);

        let mut recording = ShellWorld::for_tool(
            "edit",
            "call_edit",
            &args,
            Some(tau_vcr::VcrConfig::new(
                tau_vcr::VcrMode::RecordIfMissing,
                cassette_dir.path(),
            )),
        )
        .expect("recording world");
        let recorded = crate::tools::edit::edit_file(&args, &mut recording).expect("recorded edit");
        recording.finish().expect("record cassette");
        assert_eq!(
            std::fs::read(&file).expect("read recorded file"),
            b"one\nTWO\n"
        );
        std::fs::write(&file, b"live should not change\n").expect("change live file");

        let mut replay = ShellWorld::for_tool(
            "edit",
            "call_edit",
            &args,
            Some(tau_vcr::VcrConfig::new(
                tau_vcr::VcrMode::ReplayOnly,
                cassette_dir.path(),
            )),
        )
        .expect("replay world");
        let replayed = crate::tools::edit::edit_file(&args, &mut replay).expect("replayed edit");
        replay.finish().expect("consume replay ops");

        assert_eq!(replayed.result, recorded.result);
        assert_eq!(
            std::fs::read(&file).expect("read live file"),
            b"live should not change\n"
        );
        let cassette = std::fs::read_to_string(cassette_dir.path().join("call_edit.yaml"))
            .expect("read cassette");
        assert!(cassette.contains("op: read_file"));
        assert!(cassette.contains("op: path_exists"));
        assert!(cassette.contains("op: write_file"));
    }
    #[test]
    fn apply_patch_vcr_replay_asserts_move_write_and_remove_without_mutating_live_files() {
        let real_dir = tempfile::TempDir::new().expect("real dir");
        let source = real_dir.path().join("source.txt");
        let dest = real_dir.path().join("dest.txt");
        std::fs::write(&source, "one\ntwo\n").expect("write source");
        let cassette_dir = tempfile::TempDir::new().expect("cassette dir");
        let args = CborValue::Text(format!(
            "*** Begin Patch\n*** Update File: {}\n*** Move to: {}\n@@\n one\n-two\n+TWO\n*** End Patch",
            source.display(),
            dest.display()
        ));

        let mut recording = ShellWorld::for_tool(
            "apply_patch",
            "call_patch",
            &args,
            Some(tau_vcr::VcrConfig::new(
                tau_vcr::VcrMode::RecordIfMissing,
                cassette_dir.path(),
            )),
        )
        .expect("recording world");
        let recorded = crate::tools::apply_patch::apply_patch(&args, &mut recording)
            .expect("recorded apply_patch");
        recording.finish().expect("record cassette");
        assert!(!source.exists());
        assert_eq!(
            std::fs::read_to_string(&dest).expect("read dest"),
            "one\nTWO\n"
        );
        std::fs::write(&source, "live source\n").expect("restore live source");
        std::fs::write(&dest, "live dest\n").expect("change live dest");

        let mut replay = ShellWorld::for_tool(
            "apply_patch",
            "call_patch",
            &args,
            Some(tau_vcr::VcrConfig::new(
                tau_vcr::VcrMode::ReplayOnly,
                cassette_dir.path(),
            )),
        )
        .expect("replay world");
        let replayed = crate::tools::apply_patch::apply_patch(&args, &mut replay)
            .expect("replayed apply_patch");
        replay.finish().expect("consume replay ops");

        assert_eq!(replayed.result, recorded.result);
        assert_eq!(
            std::fs::read_to_string(&source).expect("read source"),
            "live source\n"
        );
        assert_eq!(
            std::fs::read_to_string(&dest).expect("read dest"),
            "live dest\n"
        );
        let cassette = std::fs::read_to_string(cassette_dir.path().join("call_patch.yaml"))
            .expect("read cassette");
        assert!(cassette.contains("op: read_file"));
        assert!(cassette.contains("op: create_dir_all"));
        assert!(cassette.contains("op: write_file"));
        assert!(cassette.contains("op: is_dir"));
        assert!(cassette.contains("op: remove_file"));
    }

    #[test]
    fn apply_patch_vcr_relative_paths_do_not_record_cwd_absolute_paths() {
        let cwd = std::env::current_dir().expect("current dir");
        let real_dir = tempfile::Builder::new()
            .prefix("world-relative-")
            .tempdir_in(&cwd)
            .expect("real dir under cwd");
        let source = real_dir.path().join("source.txt");
        let dest = real_dir.path().join("dest.txt");
        std::fs::write(&source, "one\ntwo\n").expect("write source");
        let source_rel = source.strip_prefix(&cwd).expect("relative source");
        let dest_rel = dest.strip_prefix(&cwd).expect("relative dest");
        let cassette_dir = tempfile::TempDir::new().expect("cassette dir");
        let args = CborValue::Text(format!(
            "*** Begin Patch\n*** Update File: {}\n*** Move to: {}\n@@\n one\n-two\n+TWO\n*** End Patch",
            source_rel.display(),
            dest_rel.display()
        ));

        let mut recording = ShellWorld::for_tool(
            "apply_patch",
            "call_relative_patch",
            &args,
            Some(tau_vcr::VcrConfig::new(
                tau_vcr::VcrMode::RecordIfMissing,
                cassette_dir.path(),
            )),
        )
        .expect("recording world");
        crate::tools::apply_patch::apply_patch(&args, &mut recording)
            .expect("recorded apply_patch");
        recording.finish().expect("record cassette");

        let cassette =
            std::fs::read_to_string(cassette_dir.path().join("call_relative_patch.yaml"))
                .expect("read cassette");
        assert!(cassette.contains(&format!("path: {}", source_rel.display())));
        assert!(cassette.contains(&format!("path: {}", dest_rel.display())));
        assert!(!cassette.contains(cwd.to_str().expect("utf8 cwd")));
    }
}
