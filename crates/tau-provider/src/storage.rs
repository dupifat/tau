//! Auth credential storage.
//!
//! Credentials live as one file per provider under
//! `~/.local/state/tau/auth.d/<name>.json`. Writes are serialized with a
//! per-provider sidecar lock and persisted with atomic replacement.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use std::{fs, io};

use serde::{Deserialize, Serialize};
use tau_config::atomic::atomic_write_following_symlink;
use tau_proto::ProviderName;

/// Returns the auth state directory.
///
/// Prefers `XDG_STATE_HOME/tau` (`~/.local/state/tau` on Linux).
/// Falls back to `data_local_dir/tau` on platforms where `state_dir`
/// is not available (macOS, Windows).
fn state_dir() -> Option<PathBuf> {
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .map(|d| d.join("tau"))
}

/// A filesystem-backed credential store.
#[derive(Clone, Debug)]
pub struct ProviderStore {
    state_dir: PathBuf,
}

impl ProviderStore {
    /// Open the default Tau provider credential store.
    pub fn open_default() -> io::Result<Self> {
        let state_dir = state_dir().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "cannot determine data directory")
        })?;
        Ok(Self { state_dir })
    }

    /// Returns the per-provider auth directory `auth.d/`.
    pub fn auth_dir(&self) -> PathBuf {
        self.state_dir.join("auth.d")
    }

    /// Returns a handle for one named provider.
    pub fn provider(&self, provider_name: ProviderName) -> ProviderHandle {
        ProviderHandle {
            store: self.clone(),
            provider_name,
        }
    }

    /// Load all credentials from disk.
    ///
    /// Reads every `auth.d/*.json` provider file. Missing directories yield an
    /// empty store. Files whose stem fails [`ProviderName`] validation are
    /// skipped with a warning rather than aborting the whole load.
    pub fn load(&self) -> io::Result<AuthStore> {
        let mut providers: HashMap<ProviderName, Credentials> = HashMap::new();

        let dir = self.auth_dir();
        if dir.is_dir() {
            for entry in fs::read_dir(&dir)? {
                let entry = entry?;
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                if path.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let provider = match ProviderName::try_new(stem.to_owned()) {
                    Ok(p) => p,
                    Err(error) => {
                        tracing::warn!(
                            path = %path.display(),
                            "skipping auth file with invalid provider name: {error}"
                        );
                        continue;
                    }
                };
                let text = fs::read_to_string(&path)?;
                let creds: Credentials = serde_json::from_str(&text)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                providers.insert(provider, creds);
            }
        }

        Ok(AuthStore { providers })
    }
}

/// A handle to one provider's credential file.
#[derive(Clone, Debug)]
pub struct ProviderHandle {
    store: ProviderStore,
    provider_name: ProviderName,
}

impl ProviderHandle {
    /// Returns the provider name this handle addresses.
    pub fn provider_name(&self) -> &ProviderName {
        &self.provider_name
    }

    /// Returns the file path that backs this provider's credentials.
    pub fn auth_path(&self) -> PathBuf {
        self.store
            .auth_dir()
            .join(format!("{}.json", self.provider_name))
    }

    /// Returns the sidecar lock path that serializes this provider file.
    pub fn lock_path(&self) -> PathBuf {
        self.store
            .auth_dir()
            .join(format!("{}.lock", self.provider_name))
    }

    /// Load this provider's credentials without taking a lock.
    pub fn load(&self) -> io::Result<Option<Credentials>> {
        load_provider_from_path(&self.auth_path())
    }

    /// Atomically save this provider's credentials while holding its lock.
    pub fn save(&self, credentials: &Credentials) -> io::Result<()> {
        self.with_lock(|locked| locked.save(credentials))
    }

    /// Remove this provider's credentials while holding its lock.
    ///
    /// Removes `auth.d/<name>.json` if present, and also strips the entry
    /// from legacy `auth.json` if that file still exists. Returns true if
    /// any state on disk changed.
    pub fn delete(&self) -> io::Result<bool> {
        self.with_lock(|locked| locked.delete())
    }

    /// Run a callback while holding the exclusive sidecar lock for this
    /// provider.
    pub fn with_lock<T>(
        &self,
        f: impl FnOnce(&LockedProviderHandle<'_>) -> io::Result<T>,
    ) -> io::Result<T> {
        let lock_file = self.open_lock_file()?;
        lock_file.lock()?;
        let locked = LockedProviderHandle {
            handle: self,
            lock_file,
        };
        let result = f(&locked);
        let unlock_result = locked.lock_file.unlock();
        match (result, unlock_result) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }

    fn open_lock_file(&self) -> io::Result<File> {
        let path = self.lock_path();
        let dir = path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "no parent for provider lock path")
        })?;
        fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
        OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
    }
}

/// A provider handle whose sidecar lock is held.
pub struct LockedProviderHandle<'a> {
    handle: &'a ProviderHandle,
    lock_file: File,
}

impl LockedProviderHandle<'_> {
    /// Load this provider's credentials while its lock is held.
    pub fn load(&self) -> io::Result<Option<Credentials>> {
        self.handle.load()
    }

    /// Atomically save this provider's credentials while its lock is held.
    pub fn save(&self, credentials: &Credentials) -> io::Result<()> {
        let path = self.handle.auth_path();
        let dir = path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "no parent for provider auth path")
        })?;
        fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }

        let json = serde_json::to_string_pretty(credentials)?;

        #[cfg(unix)]
        let default_permissions = {
            use std::os::unix::fs::PermissionsExt;
            Some(fs::Permissions::from_mode(0o600))
        };
        #[cfg(not(unix))]
        let default_permissions = None;

        atomic_write_following_symlink(&path, json.as_bytes(), default_permissions)
    }

    /// Remove this provider's credentials while its lock is held.
    pub fn delete(&self) -> io::Result<bool> {
        let mut changed = false;

        match fs::remove_file(self.handle.auth_path()) {
            Ok(()) => {
                changed = true;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }

        Ok(changed)
    }
}

fn load_provider_from_path(path: &PathBuf) -> io::Result<Option<Credentials>> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// The kind of provider (determines which OAuth flow or auth method).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    /// Local Ollama/llama.cpp — no auth needed.
    Ollama,
    /// OpenAI direct API key access.
    Openai,
    /// OpenAI via ChatGPT subscription (OAuth).
    OpenaiCodex,
    /// Anthropic direct API key access.
    Anthropic,
    /// GitHub Copilot subscription (device code OAuth).
    GithubCopilot,
}

impl ProviderKind {
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Ollama => "Ollama (local)",
            Self::Openai => "OpenAI (API key)",
            Self::OpenaiCodex => "OpenAI Codex (ChatGPT subscription)",
            Self::Anthropic => "Anthropic (API key)",
            Self::GithubCopilot => "GitHub Copilot (subscription)",
        }
    }

    pub fn requires_oauth(&self) -> bool {
        matches!(self, Self::OpenaiCodex | Self::GithubCopilot)
    }

    pub fn all() -> &'static [ProviderKind] {
        &[
            Self::Ollama,
            Self::Openai,
            Self::OpenaiCodex,
            Self::Anthropic,
            Self::GithubCopilot,
        ]
    }
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.display_name())
    }
}

/// Credentials for a single provider instance.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Credentials {
    /// No authentication needed (e.g. local Ollama).
    None {
        provider_kind: ProviderKind,
        #[serde(skip_serializing_if = "Option::is_none")]
        base_url: Option<String>,
    },
    /// Direct API key.
    ApiKey {
        provider_kind: ProviderKind,
        api_key: String,
    },
    /// OAuth token pair with expiration.
    Oauth {
        provider_kind: ProviderKind,
        access_token: String,
        refresh_token: String,
        /// Milliseconds since epoch when `access_token` expires.
        expires_at_ms: u64,
        /// Provider-specific account identifier (e.g. OpenAI account ID).
        #[serde(skip_serializing_if = "Option::is_none")]
        account_id: Option<String>,
    },
}

impl Credentials {
    pub fn provider_kind(&self) -> &ProviderKind {
        match self {
            Self::None { provider_kind, .. }
            | Self::ApiKey { provider_kind, .. }
            | Self::Oauth { provider_kind, .. } => provider_kind,
        }
    }
}

/// In-memory snapshot of all configured credentials.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AuthStore {
    pub providers: HashMap<ProviderName, Credentials>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_name_accepts_typical_names() {
        for name in [
            "local",
            "openai",
            "openai-codex",
            "github-copilot",
            "my.provider_2",
            "a",
        ] {
            assert!(
                ProviderName::try_new(name.to_owned()).is_ok(),
                "expected '{name}' to be accepted"
            );
        }
    }

    #[test]
    fn provider_name_rejects_unsafe_inputs() {
        for name in [
            "",
            ".hidden",
            "-leading-dash",
            "has space",
            "has/slash",
            "has\\backslash",
            "..",
            "../escape",
        ] {
            assert!(
                ProviderName::try_new(name.to_owned()).is_err(),
                "expected '{name}' to be rejected"
            );
        }
    }
}
