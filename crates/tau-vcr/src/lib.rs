//! Minimal cassette recording and replay helpers for Tau provider tests.
//!
//! `tau-vcr` deliberately stays provider-agnostic: callers choose the
//! deterministic cassette name and pass the exact upstream request body they
//! want validated. Provider crates own semantic request construction; this
//! crate owns durable YAML storage, raw event timing, and replay delay scaling.

use std::borrow::Cow;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const CASSETTE_VERSION: u32 = 1;
const ENV_MODE: &str = "TAU_VCR";
const ENV_DIR: &str = "TAU_VCR_DIR";
/// VCR operating mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VcrMode {
    /// Do not read or write cassettes.
    Off,
    /// Replay an existing cassette, otherwise record a new one.
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

/// VCR cassette storage configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VcrConfig {
    /// Operating mode for cassette reads/writes.
    pub mode: VcrMode,
    /// Directory containing cassette files.
    pub dir: PathBuf,
}

impl VcrConfig {
    /// Creates a VCR config rooted at `dir`.
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

    /// Returns the cassette path for `key`.
    pub fn cassette_path(&self, key: &TurnKey) -> PathBuf {
        self.dir.join(key.file_name())
    }
}

/// Opens a cassette turn, validating or storing `request_body` for `key`
/// according to `config`.
pub fn begin(
    config: &VcrConfig,
    key: TurnKey,
    request_body: serde_json::Value,
) -> Result<VcrTurn, VcrError> {
    let path = config.cassette_path(&key);
    let should_record = match config.mode {
        VcrMode::Off => return Ok(VcrTurn::Off),
        VcrMode::ReplayOnly => false,
        VcrMode::RecordIfMissing => !path.exists(),
    };

    if should_record {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| VcrError::CreateDir {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        return Ok(VcrTurn::Record(Recording {
            path,
            cassette: Cassette {
                version: CASSETTE_VERSION,
                request: RecordedRequest { body: request_body },
                response: RecordedResponse {
                    raw_events: Vec::new(),
                },
            },
            last_event_at: Instant::now(),
        }));
    }

    let cassette = read_cassette(&path)?;
    if cassette.version != CASSETTE_VERSION {
        return Err(VcrError::UnsupportedVersion {
            path,
            version: cassette.version,
        });
    }
    if cassette.request.body != request_body {
        return Err(VcrError::RequestMismatch {
            path,
            expected: cassette.request.body,
            actual: request_body,
        });
    }
    Ok(VcrTurn::Replay(Replay { cassette }))
}

/// Stable identifier for one provider request cassette.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TurnKey {
    /// Deterministic Tau session id for the test or run.
    pub session_id: String,
    /// Deterministic Tau agent prompt id, e.g. `ap-{agent_id}-{index}`.
    pub agent_prompt_id: String,
    /// Provider transport label, e.g. `http-sse` or `websocket`.
    pub transport: String,
}

impl TurnKey {
    /// Creates a cassette key from session, prompt, and transport labels.
    pub fn new(
        session_id: impl Into<String>,
        agent_prompt_id: impl Into<String>,
        transport: impl Into<String>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            agent_prompt_id: agent_prompt_id.into(),
            transport: transport.into(),
        }
    }

    /// Returns the cassette file name for this key.
    pub fn file_name(&self) -> String {
        format!(
            "{}-{}-{}.yaml",
            sanitize_path_component(&self.session_id),
            sanitize_path_component(&self.agent_prompt_id),
            sanitize_path_component(&self.transport),
        )
    }
}

/// Result of opening a VCR turn.
#[derive(Debug)]
pub enum VcrTurn {
    /// VCR is disabled; caller should use the live upstream path.
    Off,
    /// Caller should use the live upstream path and record raw events.
    ///
    /// This variant is only returned by [`VcrMode::RecordIfMissing`] when no
    /// cassette exists for the requested turn.
    Record(Recording),
    /// Caller should replay recorded raw events instead of using upstream.
    Replay(Replay),
}

/// In-progress cassette recording.
#[derive(Debug)]
pub struct Recording {
    path: PathBuf,
    cassette: Cassette,
    last_event_at: Instant,
}

impl Recording {
    /// Returns the path this recording will write on [`finish`](Self::finish).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Records one raw upstream event with elapsed wall-clock timing.
    pub fn record_raw_event(&mut self, raw: impl Into<String>) {
        let now = Instant::now();
        let delta = now.saturating_duration_since(self.last_event_at);
        self.last_event_at = now;
        self.record_raw_event_after(delta, raw);
    }

    /// Records one raw upstream event with an explicit delay since the
    /// previous event.
    pub fn record_raw_event_after(&mut self, delta: Duration, raw: impl Into<String>) {
        self.cassette.response.raw_events.push(RecordedRawEvent {
            delta_micros: duration_micros_u64(delta),
            raw: raw.into(),
        });
    }

    /// Returns the cassette currently being built.
    pub fn cassette(&self) -> &Cassette {
        &self.cassette
    }

    /// Writes the cassette to disk.
    pub fn finish(self) -> Result<(), VcrError> {
        write_cassette(&self.path, &self.cassette)
    }
}

/// Replayable cassette contents.
#[derive(Clone, Debug)]
pub struct Replay {
    cassette: Cassette,
}

impl Replay {
    /// Returns the loaded cassette.
    pub fn cassette(&self) -> &Cassette {
        &self.cassette
    }

    /// Iterates over raw upstream events with their delays scaled by `speed`.
    ///
    /// A speed of `100.0` makes a 100 ms recorded gap replay as 1 ms.
    /// Non-finite or non-positive speeds fall back to real-time speed
    /// (`1.0`).
    pub fn events_at_speed(&self, speed: f64) -> impl Iterator<Item = ReplayEvent<'_>> {
        self.cassette
            .response
            .raw_events
            .iter()
            .map(move |event| ReplayEvent {
                delay: scale_delay(Duration::from_micros(event.delta_micros), speed),
                raw: &event.raw,
            })
    }
}

/// One raw event to replay.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplayEvent<'a> {
    /// Delay to wait before emitting [`raw`](Self::raw).
    pub delay: Duration,
    /// Raw upstream event payload exactly as recorded.
    pub raw: &'a str,
}

/// On-disk cassette file.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Cassette {
    /// Cassette schema version.
    pub version: u32,
    /// Exact request body this cassette was recorded for.
    pub request: RecordedRequest,
    /// Raw upstream response stream captured for replay.
    pub response: RecordedResponse,
}

/// Exact request captured for cassette validation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RecordedRequest {
    /// Provider-owned upstream request body. The provider decides whether this
    /// is raw or normalized before passing it to `tau-vcr`.
    pub body: serde_json::Value,
}

/// Raw upstream response stream captured for replay.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RecordedResponse {
    /// Ordered raw upstream events with inter-event delays.
    pub raw_events: Vec<RecordedRawEvent>,
}

/// One raw upstream event in a cassette.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RecordedRawEvent {
    /// Delay since the previously recorded event, in microseconds.
    pub delta_micros: u64,
    /// Raw upstream event payload, e.g. one SSE `data: ...\n\n` block or one
    /// WebSocket text frame.
    pub raw: String,
}

/// Error returned by cassette recording and replay.
#[derive(Debug)]
pub enum VcrError {
    /// `TAU_VCR` contained an unknown mode.
    InvalidMode(String),
    /// Required environment variable was not present.
    MissingEnv(&'static str),
    /// Environment variable was not valid Unicode.
    EnvNotUnicode(&'static str),
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
    /// Cassette schema version is not supported by this crate.
    UnsupportedVersion {
        /// Cassette path.
        path: PathBuf,
        /// Version found in the cassette.
        version: u32,
    },
    /// Replay cassette request did not match the actual request.
    RequestMismatch {
        /// Cassette path.
        path: PathBuf,
        /// Request stored in the cassette.
        expected: serde_json::Value,
        /// Actual request supplied by the caller.
        actual: serde_json::Value,
    },
}

impl fmt::Display for VcrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMode(mode) => write!(f, "invalid TAU_VCR mode `{mode}`"),
            Self::MissingEnv(name) => write!(f, "{name} must be set when TAU_VCR is enabled"),
            Self::EnvNotUnicode(name) => write!(f, "{name} is not valid Unicode"),
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
            Self::UnsupportedVersion { path, version } => write!(
                f,
                "cassette {} has unsupported version {version}",
                path.display()
            ),
            Self::RequestMismatch { path, .. } => {
                write!(f, "cassette {} request does not match", path.display())
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
            | Self::MissingEnv(_)
            | Self::EnvNotUnicode(_)
            | Self::UnsupportedVersion { .. }
            | Self::RequestMismatch { .. } => None,
        }
    }
}

fn read_cassette(path: &Path) -> Result<Cassette, VcrError> {
    let bytes = std::fs::read(path).map_err(|source| VcrError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    serde_yaml_ng::from_slice(&bytes).map_err(|source| VcrError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

fn write_cassette(path: &Path, cassette: &Cassette) -> Result<(), VcrError> {
    let text = serde_yaml_ng::to_string(cassette).map_err(|source| VcrError::Serialize {
        path: path.to_path_buf(),
        source,
    })?;
    std::fs::write(path, text).map_err(|source| VcrError::Write {
        path: path.to_path_buf(),
        source,
    })
}

fn sanitize_path_component(value: &str) -> Cow<'_, str> {
    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Cow::Borrowed(value);
    }
    Cow::Owned(
        value
            .bytes()
            .map(|byte| {
                if byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' {
                    byte as char
                } else {
                    '_'
                }
            })
            .collect(),
    )
}

fn duration_micros_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

fn scale_delay(delay: Duration, speed: f64) -> Duration {
    let speed = if speed.is_finite() && 0.0 < speed {
        speed
    } else {
        1.0
    };
    Duration::from_secs_f64(delay.as_secs_f64() / speed)
}

#[cfg(test)]
mod tests;
