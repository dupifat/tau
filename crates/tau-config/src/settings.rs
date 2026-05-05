//! User settings loaded from `~/.config/tau/` with `.d/` directory
//! overrides. Three config files:
//!
//! - `cli.json5` — CLI display preferences
//! - `harness.json5` — harness/agent settings (default model, etc.)
//! - `models.json5` — LLM provider and model registry
//!
//! Uses the `config` crate for layered JSON5 loading.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// CLI settings
// ---------------------------------------------------------------------------

/// CLI display settings loaded from `cli.json5`.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct CliSettings {
    /// Show a greeting message on startup.
    pub greeting: bool,
    /// Show the tau ASCII logo on startup.
    pub show_logo: bool,
    /// Use a bar-shaped cursor in the CLI. When false, use a steady
    /// block cursor instead.
    pub bar_cursor: bool,
}

impl Default for CliSettings {
    fn default() -> Self {
        Self {
            greeting: true,
            show_logo: true,
            bar_cursor: true,
        }
    }
}

// ---------------------------------------------------------------------------
// CLI runtime state
// ---------------------------------------------------------------------------

/// Mutable CLI state persisted across runs at
/// `<state_dir>/cli.json`. Distinct from `CliSettings` (config) —
/// this file is written by the CLI itself in response to slash
/// commands like `/show-diff`, `/show-thinking`, and
/// `/show-cache-stats`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct CliState {
    /// Whether to render file-mutation diffs in their full expanded
    /// form (vs the compact `+N/-M` chip). Toggled by `/show-diff`.
    pub show_diff: bool,
    /// Whether to render the agent's reasoning summary (the
    /// `agent.thinking` block). Toggled by `/show-thinking`.
    pub show_thinking: bool,
    /// Whether to render provider prompt-cache hit stats in the model
    /// status bar. Toggled by `/show-cache-stats`.
    pub show_cache_stats: bool,
}

impl Default for CliState {
    fn default() -> Self {
        Self {
            show_diff: false,
            show_thinking: true,
            show_cache_stats: true,
        }
    }
}

impl CliState {
    /// Load the persisted CLI state. Missing / malformed file → defaults.
    #[must_use]
    pub fn load(dirs: &TauDirs) -> Self {
        let Some(dir) = dirs.state_dir.as_ref() else {
            return Self::default();
        };
        let path = dir.join("cli.json");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    /// Persist current state. Best-effort: ignores write failures so
    /// a slash command never fails because the user's state dir is
    /// read-only.
    pub fn save(&self, dirs: &TauDirs) {
        let Some(dir) = dirs.state_dir.as_ref() else {
            return;
        };
        let _ = std::fs::create_dir_all(dir);
        let path = dir.join("cli.json");
        let _ = serde_json::to_string_pretty(self)
            .ok()
            .and_then(|s| std::fs::write(&path, s).ok());
    }
}

// ---------------------------------------------------------------------------
// Harness settings
// ---------------------------------------------------------------------------

/// Harness/agent settings loaded from `harness.json5`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct HarnessSettings {
    /// Default model provider/model to use (e.g.
    /// "anthropic/claude-sonnet-4-20250514").
    pub default_model: Option<String>,

    /// Default effort per model (`provider/model` -> level).
    pub default_efforts: HashMap<String, tau_proto::Effort>,

    /// Extension table, keyed by name. Built-in entries (`core-agent`,
    /// `core-shell`) come pre-baked at the harness level; anything the
    /// user writes here overrides those per-field, or adds a new
    /// extension.
    ///
    /// Example `harness.json5`:
    /// ```json5
    /// {
    ///   extensions: {
    ///     // disable the built-in shell extension
    ///     "core-shell": { enable: false },
    ///     // run the agent through ssh on a remote box
    ///     "core-agent": { prefix: ["ssh", "user@host"] },
    ///     // a third-party extension
    ///     mything: { command: ["/usr/local/bin/my-tau-ext"] },
    ///   },
    /// }
    /// ```
    pub extensions: HashMap<String, ExtensionEntry>,
}

/// One entry in the harness's `extensions` map.
///
/// All fields are optional in the on-disk form so users can override
/// just the fields they care about for built-in extensions; the
/// harness merges these with built-in defaults at startup.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExtensionEntry {
    /// argv prefix prepended before `command`. Useful for wrappers
    /// that don't change the inner command, e.g.
    /// `["ssh", "user@host"]` to run remotely or
    /// `["bwrap", "--ro-bind", "/", "/", "--"]` to sandbox.
    pub prefix: Vec<String>,

    /// argv of the extension itself. `command[0]` is the executable;
    /// the rest are arguments. For built-in extensions this defaults
    /// to `[<current-exe>, "ext", <name>]`; for new entries
    /// this must be set explicitly.
    pub command: Vec<String>,

    /// Whether to run this extension. Defaults to `true`. Set to
    /// `false` to keep the entry in config but skip spawning.
    pub enable: bool,

    /// Role tag. Exactly one extension must have `role: "agent"`.
    /// Built-in `agent` defaults to that; everything else is treated
    /// as a tool extension.
    pub role: Option<String>,

    /// Free-form configuration object handed to the extension at
    /// startup via `LifecycleConfigure`. The harness does not
    /// interpret it — the extension defines and validates its own
    /// schema. Defaults to an empty object so extensions can rely
    /// on always seeing a value.
    pub config: serde_json::Value,
}

impl Default for ExtensionEntry {
    fn default() -> Self {
        // `enable: true` so a user writing
        // `extensions: { foo: { command: [...] } }` doesn't need to
        // also write `enable: true` for the entry to actually run.
        Self {
            prefix: Vec::new(),
            command: Vec::new(),
            enable: true,
            role: None,
            config: serde_json::Value::Object(serde_json::Map::new()),
        }
    }
}

/// Built-in extension shipped with `tau`. Used by
/// [`HarnessSettings::resolve_extensions`] to seed the table before
/// applying user overrides.
pub struct BuiltinExtension {
    pub name: &'static str,
    pub command: Vec<String>,
    pub role: Option<&'static str>,
    pub enable: bool,
    /// Built-in default config for this extension, merged below any
    /// user-provided `config: { … }` object in `harness.json5`.
    pub config: serde_json::Value,
}

impl HarnessSettings {
    /// Merge user-provided `extensions` entries on top of the
    /// supplied built-in extensions and produce a flat list of
    /// [`crate::ExtensionConfig`]s ready for the harness to spawn.
    ///
    /// Per-key merging:
    /// - Field-level overlay for built-in keys: any field the user set replaces
    ///   the built-in's value; unset fields keep the built-in's defaults.
    /// - User keys not in the built-in list are added as-is. They must specify
    ///   a non-empty `command`.
    /// - Entries with `enable: false` are dropped.
    ///
    /// Returns `Err` for entries that end up with an empty `command`
    /// after the merge — only possible for user-added unknown keys.
    pub fn resolve_extensions(
        &self,
        builtins: Vec<BuiltinExtension>,
    ) -> Result<Vec<crate::ExtensionConfig>, ResolveExtensionsError> {
        // Pass 1: seed an indexed map with built-ins, in order.
        let mut order: Vec<String> = builtins.iter().map(|b| b.name.to_owned()).collect();
        let mut entries: HashMap<String, ResolvedExtension> = builtins
            .into_iter()
            .map(|b| {
                (
                    b.name.to_owned(),
                    ResolvedExtension {
                        prefix: Vec::new(),
                        command: b.command,
                        enable: b.enable,
                        role: b.role.map(str::to_owned),
                        config: b.config,
                    },
                )
            })
            .collect();

        // Pass 2: overlay user entries. Sort user keys deterministically.
        let mut user_keys: Vec<&String> = self.extensions.keys().collect();
        user_keys.sort();
        for name in user_keys {
            let user = &self.extensions[name];
            match entries.get_mut(name) {
                Some(existing) => {
                    if !user.prefix.is_empty() {
                        existing.prefix = user.prefix.clone();
                    }
                    if !user.command.is_empty() {
                        existing.command = user.command.clone();
                    }
                    existing.enable = user.enable;
                    if user.role.is_some() {
                        existing.role = user.role.clone();
                    }
                    // Config: user object overlays builtin object
                    // field-by-field; non-object user values replace
                    // the builtin entirely. This lets a user override
                    // `idle_seconds` without re-stating the rest of
                    // the builtin defaults.
                    existing.config = merge_json(existing.config.take(), user.config.clone());
                }
                None => {
                    if user.command.is_empty() {
                        return Err(ResolveExtensionsError::EmptyCommand(name.clone()));
                    }
                    order.push(name.clone());
                    entries.insert(
                        name.clone(),
                        ResolvedExtension {
                            prefix: user.prefix.clone(),
                            command: user.command.clone(),
                            enable: user.enable,
                            role: user.role.clone(),
                            config: user.config.clone(),
                        },
                    );
                }
            }
        }

        // Pass 3: produce ExtensionConfigs in declared order, dropping
        // disabled entries. argv = prefix ++ command; argv[0] is the
        // executable, rest are args.
        let mut out = Vec::new();
        for name in order {
            let entry = entries.remove(&name).expect("seeded above");
            if !entry.enable {
                continue;
            }
            let mut argv = entry.prefix;
            argv.extend(entry.command);
            let (program, args) = match argv.split_first() {
                Some((first, rest)) => (first.clone(), rest.to_vec()),
                None => return Err(ResolveExtensionsError::EmptyCommand(name)),
            };
            out.push(crate::ExtensionConfig {
                name,
                command: program,
                args,
                role: entry.role,
                config: entry.config,
            });
        }
        Ok(out)
    }
}

#[derive(Debug)]
struct ResolvedExtension {
    prefix: Vec<String>,
    command: Vec<String>,
    enable: bool,
    role: Option<String>,
    config: serde_json::Value,
}

/// Merge `over` on top of `base` for extension config objects.
///
/// When both are JSON objects, keys are merged shallowly:
/// `over`'s keys win, `base`'s keys are kept where `over` doesn't
/// mention them. For any other shape (one side isn't an object),
/// `over` replaces `base` outright if it isn't `Null`. This is the
/// minimum needed to let a user override one field of a builtin's
/// config without restating the rest.
fn merge_json(base: serde_json::Value, over: serde_json::Value) -> serde_json::Value {
    match (base, over) {
        (serde_json::Value::Object(mut b), serde_json::Value::Object(o)) => {
            for (k, v) in o {
                b.insert(k, v);
            }
            serde_json::Value::Object(b)
        }
        (base, serde_json::Value::Null) => base,
        (_, over) => over,
    }
}

/// Error returned by [`HarnessSettings::resolve_extensions`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolveExtensionsError {
    /// A user-added extension entry has no `command` (and therefore
    /// no executable to spawn).
    EmptyCommand(String),
}

impl fmt::Display for ResolveExtensionsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyCommand(name) => write!(
                f,
                "extension {name:?} has no `command` set; user-added entries must specify the executable",
            ),
        }
    }
}

impl std::error::Error for ResolveExtensionsError {}

// ---------------------------------------------------------------------------
// Model registry
// ---------------------------------------------------------------------------

/// Top-level model configuration (mirrors Pi's models.json).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ModelRegistry {
    /// Named providers, keyed by provider name.
    pub providers: HashMap<String, ProviderConfig>,
}

/// One LLM provider configuration.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    /// Base URL for the API endpoint.
    #[serde(rename = "baseUrl")]
    pub base_url: Option<String>,
    /// API protocol: "anthropic", "openai-completions", etc.
    pub api: Option<String>,
    /// Authentication method: "api-key" (default when `apiKey` is set),
    /// "openai-codex", "github-copilot", or "none".
    pub auth: Option<String>,
    /// API key or environment variable name. Prefix with `!` for
    /// shell command execution (Pi convention).
    #[serde(rename = "apiKey")]
    pub api_key: Option<String>,
    /// Extra HTTP headers (key → value or env var name).
    pub headers: Option<HashMap<String, String>>,
    /// Optional provider-side prompt cache retention policy.
    #[serde(rename = "promptCacheRetention")]
    pub prompt_cache_retention: Option<PromptCacheRetention>,
    /// Compatibility flags for non-standard providers.
    #[serde(default)]
    pub compat: ProviderCompat,
    /// Models available from this provider.
    #[serde(default)]
    pub models: Vec<ModelConfig>,
}

/// Compatibility flags for providers that don't support all features.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct ProviderCompat {
    #[serde(rename = "supportsDeveloperRole")]
    pub supports_developer_role: bool,
    #[serde(rename = "supportsReasoningEffort")]
    pub supports_reasoning_effort: bool,
    #[serde(rename = "supportsPrefill")]
    pub supports_prefill: bool,
    #[serde(rename = "supportsPromptCacheKey")]
    pub supports_prompt_cache_key: bool,
    #[serde(rename = "supportsPromptCacheRetention")]
    pub supports_prompt_cache_retention: bool,
    /// Provider's API accepts `reasoning.summary` and streams
    /// `response.reasoning_summary_text.*` events. Currently only
    /// the OpenAI Responses API surface.
    #[serde(rename = "supportsReasoningSummary")]
    pub supports_reasoning_summary: bool,
}

impl Default for ProviderCompat {
    fn default() -> Self {
        Self {
            supports_developer_role: true,
            supports_reasoning_effort: true,
            supports_prefill: true,
            supports_prompt_cache_key: false,
            supports_prompt_cache_retention: false,
            supports_reasoning_summary: false,
        }
    }
}

/// Provider-side prompt cache retention policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
pub enum PromptCacheRetention {
    #[serde(rename = "in_memory")]
    InMemory,
    #[serde(rename = "24h")]
    Extended24h,
}

impl PromptCacheRetention {
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::InMemory => "in_memory",
            Self::Extended24h => "24h",
        }
    }
}

/// One model available from a provider.
#[derive(Clone, Debug, Deserialize)]
pub struct ModelConfig {
    /// Model identifier (e.g. "claude-sonnet-4-20250514").
    pub id: String,
    /// Optional display name.
    pub name: Option<String>,
    /// Max output tokens override.
    #[serde(rename = "maxOutputTokens")]
    pub max_output_tokens: Option<u64>,
    /// Total context window size, in tokens.
    #[serde(rename = "contextWindow")]
    pub context_window: Option<u64>,
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Errors from settings/model loading.
#[derive(Debug)]
pub enum SettingsError {
    Config(config::ConfigError),
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(source) => write!(f, "settings error: {source}"),
        }
    }
}

impl std::error::Error for SettingsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Config(source) => Some(source),
        }
    }
}

impl From<config::ConfigError> for SettingsError {
    fn from(source: config::ConfigError) -> Self {
        Self::Config(source)
    }
}

/// Returns the default tau config directory (`~/.config/tau`).
#[must_use]
pub fn config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("tau"))
}

/// Returns the default tau state directory (`~/.local/state/tau`).
#[must_use]
pub fn state_dir() -> Option<PathBuf> {
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .map(|d| d.join("tau"))
}

/// Overridable directory layout for tau. Use the defaults (`Self::default()`)
/// for normal user runs or construct explicit paths for tests and custom
/// installations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TauDirs {
    /// Where to look for `cli.json5`, `harness.json5`, `models.json5`, etc.
    pub config_dir: Option<PathBuf>,
    /// Where to read/write runtime state like `harness.json5`.
    pub state_dir: Option<PathBuf>,
}

impl Default for TauDirs {
    fn default() -> Self {
        Self {
            config_dir: config_dir(),
            state_dir: state_dir(),
        }
    }
}

/// Loads CLI settings from `cli.json5` with `cli.d/*.json5` overrides.
pub fn load_cli_settings() -> Result<CliSettings, SettingsError> {
    load_cli_settings_in(&TauDirs::default())
}

/// Like [`load_cli_settings`] but reads from an explicit directory layout.
pub fn load_cli_settings_in(dirs: &TauDirs) -> Result<CliSettings, SettingsError> {
    let Some(ref dir) = dirs.config_dir else {
        return Ok(CliSettings::default());
    };
    load_json5_layered(dir, "cli")
}

/// Loads harness settings from `harness.json5` with `harness.d/*.json5`
/// overrides.
pub fn load_harness_settings() -> Result<HarnessSettings, SettingsError> {
    load_harness_settings_in(&TauDirs::default())
}

/// Like [`load_harness_settings`] but reads from an explicit directory layout.
pub fn load_harness_settings_in(dirs: &TauDirs) -> Result<HarnessSettings, SettingsError> {
    let Some(ref dir) = dirs.config_dir else {
        return Ok(HarnessSettings::default());
    };
    load_json5_layered(dir, "harness")
}

/// Loads the model registry from `models.json5` with `models.d/*.json5`
/// overrides.
pub fn load_models() -> Result<ModelRegistry, SettingsError> {
    load_models_in(&TauDirs::default())
}

/// Like [`load_models`] but reads from an explicit directory layout.
pub fn load_models_in(dirs: &TauDirs) -> Result<ModelRegistry, SettingsError> {
    let Some(ref dir) = dirs.config_dir else {
        return Ok(ModelRegistry::default());
    };
    load_json5_layered(dir, "models")
}

/// Generic layered JSON5 loader: reads `{name}.json5` then all
/// `{name}.d/*.json5` files sorted alphabetically, merging each
/// layer on top.
fn load_json5_layered<T: for<'de> Deserialize<'de> + Default>(
    dir: &Path,
    name: &str,
) -> Result<T, SettingsError> {
    let base_path = dir.join(format!("{name}.json5"));
    let drop_dir = dir.join(format!("{name}.d"));

    let mut builder = config::Config::builder();

    // Base file is optional, but parse errors must surface.
    // We guard on exists() and use required(true) so a missing file
    // is fine but a malformed one is an error.
    if base_path.exists() {
        builder = builder.add_source(
            config::File::from(base_path)
                .format(config::FileFormat::Json5)
                .required(true),
        );
    }

    // Drop-in files: same — optional to have, but must parse.
    if drop_dir.is_dir() {
        let mut paths: Vec<PathBuf> = std::fs::read_dir(&drop_dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|ext| ext == "json5"))
            .collect();
        paths.sort();
        for path in paths {
            builder = builder.add_source(
                config::File::from(path)
                    .format(config::FileFormat::Json5)
                    .required(true),
            );
        }
    }

    let config = builder.build()?;

    // If no sources were added, return default.
    if config.cache.kind == config::ValueKind::Nil {
        return Ok(T::default());
    }

    config.try_deserialize().map_err(SettingsError::from)
}

#[cfg(test)]
mod tests;
