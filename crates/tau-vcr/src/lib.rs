//! Minimal YAML cassette storage helpers for Tau tests.
//!
//! `tau-vcr` deliberately stays below provider and tool semantics. It owns VCR
//! mode parsing, cassette directory/key handling, key validation, and YAML
//! `get`/`put` operations. Callers own cassette schemas, request validation,
//! live-vs-replay branching, timing, and response replay.
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

const ENV_MODE: &str = "TAU_VCR";
const ENV_DIR: &str = "TAU_VCR_DIR";

/// VCR operating mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VcrMode {
    /// Do not read or write cassettes.
    Off,
    /// Replay an existing cassette, otherwise let the caller record a new one.
    RecordIfMissing,
    /// Require an existing cassette and replay it.
    ReplayOnly,
}

impl VcrMode {
    /// Parses a mode string such as `off`, `record-if-missing`, or
    /// `replay-only`.
    pub fn parse(value: &str) -> Result<Self, VcrError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "off" => Ok(Self::Off),
            "record-if-missing" => Ok(Self::RecordIfMissing),
            "replay-only" => Ok(Self::ReplayOnly),
            other => Err(VcrError::InvalidMode(other.to_owned())),
        }
    }
}

/// VCR mode and cassette storage directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VcrConfig {
    /// Operating mode for cassette reads/writes.
    pub mode: VcrMode,
    /// Directory containing cassette files.
    pub dir: PathBuf,
}

impl VcrConfig {
    /// Creates a VCR config rooted at `dir`.
    #[must_use]
    pub fn new(mode: VcrMode, dir: impl Into<PathBuf>) -> Self {
        Self {
            mode,
            dir: dir.into(),
        }
    }

    /// Reads VCR config from `TAU_VCR` and `TAU_VCR_DIR`.
    ///
    /// Returns `Ok(None)` when `TAU_VCR` is unset or `off`. `TAU_VCR_DIR` is
    /// required for `record-if-missing` and `replay-only` modes.
    pub fn from_env() -> Result<Option<Self>, VcrError> {
        let mode = match std::env::var(ENV_MODE) {
            Ok(value) => VcrMode::parse(&value)?,
            Err(std::env::VarError::NotPresent) => VcrMode::Off,
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(VcrError::EnvNotUnicode(ENV_MODE));
            }
        };
        if mode == VcrMode::Off {
            return Ok(None);
        }
        let dir = std::env::var_os(ENV_DIR).ok_or(VcrError::MissingEnv(ENV_DIR))?;
        Ok(Some(Self::new(mode, PathBuf::from(dir))))
    }

    /// Returns a cassette store rooted at this config's directory.
    #[must_use]
    pub fn store(&self) -> VcrStore {
        VcrStore::new(&self.dir)
    }
}

/// Filesystem-backed YAML cassette key/value store.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VcrStore {
    dir: PathBuf,
}

impl VcrStore {
    /// Creates a cassette store rooted at `dir`.
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Returns the cassette path for `key`.
    ///
    /// Keys are logical identifiers, not paths. Only ASCII alphanumeric
    /// characters, `-`, and `_` are accepted.
    fn path(&self, key: &str) -> Result<PathBuf, VcrError> {
        if key.is_empty()
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(VcrError::InvalidKey(key.to_owned()));
        }
        Ok(self.dir.join(format!("{key}.yaml")))
    }

    /// Reads and parses the cassette for `key`.
    ///
    /// Returns `Ok(None)` when the cassette does not exist.
    pub fn get<T>(&self, key: &str) -> Result<Option<T>, VcrError>
    where
        T: DeserializeOwned,
    {
        let path = self.path(key)?;
        if !path.exists() {
            return Ok(None);
        }
        read_yaml(&path).map(Some)
    }

    /// Serializes and writes the cassette for `key`, replacing any existing
    /// file.
    pub fn put<T>(&self, key: &str, value: &T) -> Result<(), VcrError>
    where
        T: Serialize,
    {
        let path = self.path(key)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| VcrError::CreateDir {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        write_yaml(&path, value)
    }
}

pub fn request_mismatch<T, U>(key: impl Into<String>, expected: &T, actual: &U) -> VcrError
where
    T: Serialize,
    U: Serialize,
{
    VcrError::RequestMismatch {
        key: key.into(),
        expected: mismatch_payload(expected),
        actual: mismatch_payload(actual),
    }
}

/// Error returned by cassette storage and mode parsing.
#[derive(Debug)]
pub enum VcrError {
    /// `TAU_VCR` contained an unknown mode.
    InvalidMode(String),
    /// Required environment variable was not present.
    MissingEnv(&'static str),
    /// Environment variable was not valid Unicode.
    EnvNotUnicode(&'static str),
    /// Cassette key contained unsupported characters.
    InvalidKey(String),
    /// Requested cassette was not found.
    Missing {
        /// Logical cassette key.
        key: String,
    },
    /// Failed to create a cassette directory.
    CreateDir {
        /// Directory path that could not be created.
        path: PathBuf,
        /// Underlying IO error.
        source: std::io::Error,
    },
    /// Failed to read a cassette file.
    Read {
        /// Cassette path.
        path: PathBuf,
        /// Underlying IO error.
        source: std::io::Error,
    },
    /// Failed to write a cassette file.
    Write {
        /// Cassette path.
        path: PathBuf,
        /// Underlying IO error.
        source: std::io::Error,
    },
    /// Failed to parse a cassette file.
    Parse {
        /// Cassette path.
        path: PathBuf,
        /// Underlying YAML error.
        source: serde_yaml_ng::Error,
    },
    /// Failed to serialize a cassette file.
    Serialize {
        /// Cassette path.
        path: PathBuf,
        /// Underlying YAML error.
        source: serde_yaml_ng::Error,
    },
    /// Cassette schema version is not supported by the caller.
    UnsupportedVersion {
        /// Logical cassette key.
        key: String,
        /// Version found in the cassette.
        version: u32,
    },
    /// Replay cassette request did not match the actual request.
    RequestMismatch {
        /// Logical cassette key.
        key: String,
        /// Request stored in the cassette, serialized for diagnostics.
        expected: String,
        /// Actual request supplied by the caller, serialized for diagnostics.
        actual: String,
    },
}

impl fmt::Display for VcrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMode(mode) => write!(f, "invalid TAU_VCR mode `{mode}`"),
            Self::InvalidKey(key) => write!(
                f,
                "invalid cassette key `{key}`; expected only a-z, A-Z, 0-9, -, or _"
            ),
            Self::MissingEnv(name) => write!(f, "{name} must be set when TAU_VCR is enabled"),
            Self::EnvNotUnicode(name) => write!(f, "{name} is not valid Unicode"),
            Self::Missing { key } => write!(f, "missing cassette `{key}`"),
            Self::CreateDir { path, source } => {
                write!(
                    f,
                    "failed to create cassette dir {}: {source}",
                    path.display()
                )
            }
            Self::Read { path, source } => {
                write!(f, "failed to read cassette {}: {source}", path.display())
            }
            Self::Write { path, source } => {
                write!(f, "failed to write cassette {}: {source}", path.display())
            }
            Self::Parse { path, source } => {
                write!(f, "failed to parse cassette {}: {source}", path.display())
            }
            Self::Serialize { path, source } => {
                write!(
                    f,
                    "failed to serialize cassette {}: {source}",
                    path.display()
                )
            }
            Self::UnsupportedVersion { key, version } => {
                write!(f, "cassette `{key}` has unsupported version {version}")
            }
            Self::RequestMismatch { key, .. } => {
                write!(f, "cassette `{key}` request does not match")
            }
        }
    }
}

impl std::error::Error for VcrError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CreateDir { source, .. }
            | Self::Read { source, .. }
            | Self::Write { source, .. } => Some(source),
            Self::Parse { source, .. } | Self::Serialize { source, .. } => Some(source),
            Self::InvalidMode(_)
            | Self::InvalidKey(_)
            | Self::Missing { .. }
            | Self::MissingEnv(_)
            | Self::EnvNotUnicode(_)
            | Self::UnsupportedVersion { .. }
            | Self::RequestMismatch { .. } => None,
        }
    }
}

fn read_yaml<T>(path: &Path) -> Result<T, VcrError>
where
    T: DeserializeOwned,
{
    let bytes = std::fs::read(path).map_err(|source| VcrError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    serde_yaml_ng::from_slice(&bytes).map_err(|source| VcrError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

fn write_yaml<T>(path: &Path, cassette: &T) -> Result<(), VcrError>
where
    T: Serialize,
{
    let text = serde_yaml_ng::to_string(cassette).map_err(|source| VcrError::Serialize {
        path: path.to_path_buf(),
        source,
    })?;
    std::fs::write(path, text).map_err(|source| VcrError::Write {
        path: path.to_path_buf(),
        source,
    })
}

fn mismatch_payload<T>(value: &T) -> String
where
    T: Serialize,
{
    serde_yaml_ng::to_string(value).unwrap_or_else(|error| format!("<serialize error: {error}>"))
}

#[cfg(test)]
mod tests;
