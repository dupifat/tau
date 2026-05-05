//! User and project configuration loading and merge logic.
//!
//! Default paths follow standard platform conventions via the `dirs` crate:
//!
//! - user config: `<config_dir>/tau/config.toml` (e.g.
//!   `~/.config/tau/config.toml`)
//! - project config: `<project-root>/.tau.toml`
//!
//! Project config layering is additive for `[[extensions]]`: project entries
//! are appended on top of user entries and never remove them.

pub mod settings;

use std::error::Error;
use std::path::{Path, PathBuf};
use std::{fmt, fs, io};

use serde::Deserialize;

/// The resolved harness configuration after layering user and project config.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Config {
    pub core: CoreConfig,
    pub extensions: Vec<ExtensionConfig>,
}

impl Config {
    /// Applies one parsed config file on top of the current resolved config.
    pub fn merge_file(&mut self, file: ConfigFile) {
        self.core.merge(file.core);
        self.extensions.extend(file.extensions);
    }
}

/// Resolved core configuration values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoreConfig {
    pub mode: CoreMode,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            mode: CoreMode::Embedded,
        }
    }
}

impl CoreConfig {
    fn merge(&mut self, file_core: FileCoreConfig) {
        if let Some(mode) = file_core.mode {
            self.mode = mode;
        }
    }
}

/// Minimal runtime mode selection for the harness.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreMode {
    Embedded,
    Daemon,
}

/// One configured extension process entry.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub role: Option<String>,
    /// Config object handed to the extension via
    /// `LifecycleConfigure`. Defaults to an empty object so
    /// extensions always see a value.
    #[serde(default = "empty_object")]
    pub config: serde_json::Value,
}

fn empty_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

/// The shape of one TOML config file before layering is applied.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    #[serde(default)]
    pub core: FileCoreConfig,
    #[serde(default)]
    pub extensions: Vec<ExtensionConfig>,
}

/// Partial core configuration as it appears in a single file.
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileCoreConfig {
    #[serde(default)]
    pub mode: Option<CoreMode>,
}

/// Loading behavior knobs.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LoadOptions {
    /// Overrides the user config path. Missing explicit paths are treated as an
    /// error.
    pub user_config_path: Option<PathBuf>,
    /// Enables project config loading.
    pub enable_project_config: bool,
    /// Overrides the project config path. Missing explicit paths are treated as
    /// an error.
    pub project_config_path: Option<PathBuf>,
}

/// Filesystem and environment inputs used to derive default config paths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadPaths {
    /// Platform config directory (e.g. `~/.config` on Linux).
    pub config_dir: Option<PathBuf>,
    pub current_dir: PathBuf,
}

impl LoadPaths {
    /// Reads process paths from the current environment.
    pub fn from_process() -> io::Result<Self> {
        Ok(Self {
            config_dir: dirs::config_dir(),
            current_dir: std::env::current_dir()?,
        })
    }
}

/// Errors that can occur while loading configuration files.
#[derive(Debug)]
pub enum LoadConfigError {
    CurrentDirectory(io::Error),
    MissingExplicitFile {
        path: PathBuf,
    },
    ReadFile {
        path: PathBuf,
        source: io::Error,
    },
    ParseFile {
        path: PathBuf,
        source: toml::de::Error,
    },
}

impl fmt::Display for LoadConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrentDirectory(source) => {
                write!(f, "failed to resolve current directory: {source}")
            }
            Self::MissingExplicitFile { path } => {
                write!(f, "explicit config file does not exist: {}", path.display())
            }
            Self::ReadFile { path, source } => {
                write!(f, "failed to read config file {}: {source}", path.display())
            }
            Self::ParseFile { path, source } => {
                write!(
                    f,
                    "failed to parse config file {}: {source}",
                    path.display()
                )
            }
        }
    }
}

impl Error for LoadConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::CurrentDirectory(source) => Some(source),
            Self::MissingExplicitFile { .. } => None,
            Self::ReadFile { source, .. } => Some(source),
            Self::ParseFile { source, .. } => Some(source),
        }
    }
}

/// Parses one TOML config file from a string.
pub fn parse_config_str(input: &str) -> Result<ConfigFile, toml::de::Error> {
    toml::from_str(input)
}

/// Returns the default user config path for the provided environment inputs.
#[must_use]
pub fn default_user_config_path(paths: &LoadPaths) -> Option<PathBuf> {
    paths
        .config_dir
        .as_deref()
        .map(|path| path.join("tau").join("config.toml"))
}

/// Returns the default project config path for a project root.
#[must_use]
pub fn default_project_config_path(project_root: &Path) -> PathBuf {
    project_root.join(".tau.toml")
}

/// Loads config using real process environment defaults.
pub fn load(options: &LoadOptions) -> Result<Config, LoadConfigError> {
    let paths = LoadPaths::from_process().map_err(LoadConfigError::CurrentDirectory)?;
    load_with_paths(options, &paths)
}

/// Loads config using explicit environment inputs, which makes testing easy.
pub fn load_with_paths(
    options: &LoadOptions,
    paths: &LoadPaths,
) -> Result<Config, LoadConfigError> {
    let mut config = Config::default();

    let user_path = options
        .user_config_path
        .clone()
        .or_else(|| default_user_config_path(paths));
    maybe_merge_file(
        &mut config,
        user_path.as_deref(),
        options.user_config_path.is_some(),
    )?;

    if options.enable_project_config {
        let project_path = options
            .project_config_path
            .clone()
            .unwrap_or_else(|| default_project_config_path(&paths.current_dir));
        maybe_merge_file(
            &mut config,
            Some(project_path.as_path()),
            options.project_config_path.is_some(),
        )?;
    }

    Ok(config)
}

fn maybe_merge_file(
    config: &mut Config,
    path: Option<&Path>,
    explicit_path: bool,
) -> Result<(), LoadConfigError> {
    let Some(path) = path else {
        return Ok(());
    };

    if !path.exists() {
        return if explicit_path {
            Err(LoadConfigError::MissingExplicitFile {
                path: path.to_path_buf(),
            })
        } else {
            Ok(())
        };
    }

    let file = load_file(path)?;
    config.merge_file(file);
    Ok(())
}

fn load_file(path: &Path) -> Result<ConfigFile, LoadConfigError> {
    let contents = fs::read_to_string(path).map_err(|source| LoadConfigError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;

    parse_config_str(&contents).map_err(|source| LoadConfigError::ParseFile {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests;
