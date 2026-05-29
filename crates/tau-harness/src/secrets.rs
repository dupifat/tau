//! Harness-owned secret loading and per-extension resolution.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};

use tau_config::settings::ExtensionSecretEntry;
use tau_proto::SecretValue;

use crate::settings::Config;

const ENV_PREFIX: &str = "TAU_SECRET_";

/// Errors reported while collecting or resolving extension secrets.
#[derive(Debug)]
pub enum SecretsError {
    /// A configured or environment-derived secret name is unsafe.
    InvalidName { name: String, context: String },
    /// A normalized environment secret name was provided more than once.
    EnvCollision { name: String },
    /// The secret file could not be read as UTF-8 text.
    InvalidUtf8 { path: PathBuf },
    /// A required configured secret is unavailable.
    MissingRequired { extension: String, secret: String },
    /// A secret file could not be read.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for SecretsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName { name, context } => {
                write!(
                    f,
                    "invalid secret name `{name}` in {context}; secret names may contain only ASCII letters, digits, '.', '_' and '-'"
                )
            }
            Self::EnvCollision { name } => write!(
                f,
                "multiple TAU_SECRET_* environment variables normalize to secret `{name}`"
            ),
            Self::InvalidUtf8 { path } => {
                write!(f, "secret file {} is not valid UTF-8", path.display())
            }
            Self::MissingRequired { extension, secret } => write!(
                f,
                "required secret `{secret}` for extension `{extension}` is missing; create <state_dir>/secrets/{}.yaml or set TAU_SECRET_{}",
                secret.to_ascii_lowercase(),
                secret.to_ascii_uppercase()
            ),
            Self::Io { path, source } => {
                write!(f, "failed to read secret file {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for SecretsError {}

/// Resolved secret source snapshot. Values must not be logged.
#[derive(Default)]
pub struct SecretSources {
    env: HashMap<String, String>,
}

impl fmt::Debug for SecretSources {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretSources")
            .field("env_secret_count", &self.env.len())
            .field("env_secret_names", &self.env.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Return true when `name` is a conservative single path component.
#[must_use]
pub fn is_valid_secret_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn validate_secret_name(name: &str, context: impl Into<String>) -> Result<(), SecretsError> {
    if is_valid_secret_name(name) {
        Ok(())
    } else {
        Err(SecretsError::InvalidName {
            name: name.to_owned(),
            context: context.into(),
        })
    }
}

/// Collect all `TAU_SECRET_*` variables and remove them from this process.
#[allow(unsafe_code)]
pub fn load_secret_sources() -> Result<SecretSources, SecretsError> {
    let mut env = HashMap::new();
    let mut remove = Vec::new();
    let mut error = None;
    for (key, value) in std::env::vars() {
        if let Some(suffix) = key.strip_prefix(ENV_PREFIX) {
            remove.push(key.clone());
            let name = suffix.to_ascii_lowercase();
            if let Err(err) = validate_secret_name(&name, "environment") {
                error.get_or_insert(err);
                continue;
            }
            if !value.is_empty() && env.insert(name.clone(), value).is_some() {
                error.get_or_insert(SecretsError::EnvCollision { name });
            }
        }
    }
    for key in remove {
        // SAFETY: `Harness::from_config` calls this during startup before any
        // supervised extensions are spawned, and before Tau starts background
        // threads that read environment variables. Removing these one-shot
        // secret variables here prevents later child processes from inheriting
        // them from the harness process environment.
        unsafe { std::env::remove_var(key) };
    }
    if let Some(error) = error {
        return Err(error);
    }
    Ok(SecretSources { env })
}

fn read_file_secret(state_dir: &Path, name: &str) -> Result<Option<String>, SecretsError> {
    let path = state_dir.join("secrets").join(format!("{name}.yaml"));
    match std::fs::read(&path) {
        Ok(bytes) => {
            let text = String::from_utf8(bytes)
                .map_err(|_| SecretsError::InvalidUtf8 { path: path.clone() })?;
            let value = text.trim().to_owned();
            Ok((!value.is_empty()).then_some(value))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(SecretsError::Io { path, source }),
    }
}

fn resolve_one_secret(
    state_dir: &Path,
    sources: &SecretSources,
    extension: &str,
    name: &str,
    declaration: &ExtensionSecretEntry,
) -> Result<Option<SecretValue>, SecretsError> {
    validate_secret_name(name, format!("extension `{extension}` secrets"))?;
    let normalized_name = name.to_ascii_lowercase();
    let file = read_file_secret(state_dir, &normalized_name)?;
    let value = sources.env.get(&normalized_name).cloned().or(file);
    match value {
        Some(value) if !value.is_empty() => Ok(Some(SecretValue::new(value))),
        _ if declaration.optional => Ok(None),
        _ => Err(SecretsError::MissingRequired {
            extension: extension.to_owned(),
            secret: name.to_owned(),
        }),
    }
}

/// Resolve all configured extension secrets from files and one-shot env vars.
pub fn resolve_extension_secrets(
    config: &Config,
    state_dir: &Path,
    sources: &SecretSources,
) -> Result<BTreeMap<String, BTreeMap<String, SecretValue>>, SecretsError> {
    let mut out = BTreeMap::new();
    for (extension, extension_config) in &config.extensions {
        let mut secrets = BTreeMap::new();
        for (name, declaration) in &extension_config.secrets {
            if let Some(value) =
                resolve_one_secret(state_dir, sources, extension, name, declaration)?
            {
                secrets.insert(name.clone(), value);
            }
        }
        out.insert(extension.clone(), secrets);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tempfile::TempDir;

    use super::*;
    use crate::settings::{Config, CoreConfig, CoreMode, ExtensionConfig};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn config_with_secret(optional: bool) -> Config {
        let mut secrets = BTreeMap::new();
        secrets.insert(
            "mail_password".to_owned(),
            ExtensionSecretEntry { optional },
        );
        Config {
            core: CoreConfig {
                mode: CoreMode::Embedded,
            },
            extensions: BTreeMap::from([(
                "std-email".to_owned(),
                ExtensionConfig {
                    name: "std-email".to_owned(),
                    command: "tau".to_owned(),
                    args: Vec::new(),
                    role: None,
                    config: serde_json::json!({}),
                    secrets,
                },
            )]),
        }
    }

    #[test]
    fn file_secret_values_are_trimmed() {
        let td = TempDir::new().expect("tempdir");
        let dir = td.path().join("secrets");
        std::fs::create_dir_all(&dir).expect("mkdir secrets");
        std::fs::write(dir.join("mail_password.yaml"), "\n value \t\n").expect("write");
        let sources = SecretSources::default();

        let resolved = resolve_extension_secrets(&config_with_secret(false), td.path(), &sources)
            .expect("resolve secrets");

        assert_eq!(
            resolved["std-email"]["mail_password"].expose_secret(),
            "value"
        );
    }

    #[test]
    #[allow(unsafe_code)]
    fn env_overrides_file_and_is_removed() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let td = TempDir::new().expect("tempdir");
        let dir = td.path().join("secrets");
        std::fs::create_dir_all(&dir).expect("mkdir secrets");
        std::fs::write(dir.join("mail_password.yaml"), "file").expect("write");
        // SAFETY: serialized test-only environment mutation.
        unsafe { std::env::set_var("TAU_SECRET_MAIL_PASSWORD", "env") };

        let sources = load_secret_sources().expect("load sources");
        assert!(std::env::var("TAU_SECRET_MAIL_PASSWORD").is_err());
        let resolved = resolve_extension_secrets(&config_with_secret(false), td.path(), &sources)
            .expect("resolve secrets");

        assert_eq!(
            resolved["std-email"]["mail_password"].expose_secret(),
            "env"
        );
    }

    #[test]
    #[allow(unsafe_code)]
    fn uppercase_configured_secret_name_matches_normalized_env() {
        // Secret names in config and TAU_SECRET_* suffixes are treated
        // case-insensitively, while the key handed to the extension stays as
        // configured so extension config can reference the same spelling.
        let _guard = ENV_LOCK.lock().expect("env lock");
        let mut config = config_with_secret(false);
        let extension = config.extensions.get_mut("std-email").expect("extension");
        extension.secrets.clear();
        extension.secrets.insert(
            "GOOGLE_CALENDAR_CLIENT_ID".to_owned(),
            ExtensionSecretEntry { optional: false },
        );
        // SAFETY: serialized test-only environment mutation.
        unsafe { std::env::set_var("TAU_SECRET_GOOGLE_CALENDAR_CLIENT_ID", "client") };

        let sources = load_secret_sources().expect("load sources");
        let resolved = resolve_extension_secrets(&config, TempDir::new().unwrap().path(), &sources)
            .expect("resolve secrets");

        assert_eq!(
            resolved["std-email"]["GOOGLE_CALENDAR_CLIENT_ID"].expose_secret(),
            "client"
        );
    }

    #[test]
    fn missing_required_secret_names_extension_and_secret() {
        let td = TempDir::new().expect("tempdir");
        let err = resolve_extension_secrets(
            &config_with_secret(false),
            td.path(),
            &SecretSources::default(),
        )
        .expect_err("missing required secret should fail");
        let msg = err.to_string();
        assert!(msg.contains("std-email"));
        assert!(msg.contains("mail_password"));
    }

    #[test]
    fn missing_optional_secret_is_omitted() {
        let td = TempDir::new().expect("tempdir");
        let resolved = resolve_extension_secrets(
            &config_with_secret(true),
            td.path(),
            &SecretSources::default(),
        )
        .expect("optional missing secret resolves");
        assert!(resolved["std-email"].is_empty());
    }

    #[test]
    fn invalid_secret_names_are_rejected_before_path_join() {
        let mut config = config_with_secret(false);
        let mut secrets = BTreeMap::new();
        secrets.insert("../bad".to_owned(), ExtensionSecretEntry::default());
        config
            .extensions
            .get_mut("std-email")
            .expect("test config should include std-email extension")
            .secrets = secrets;
        let td = TempDir::new().expect("tempdir");

        let err = resolve_extension_secrets(&config, td.path(), &SecretSources::default())
            .expect_err("invalid secret name should fail");
        assert!(err.to_string().contains("../bad"));
    }

    fn validate_env_pairs_for_test(
        pairs: impl IntoIterator<Item = (String, String)>,
    ) -> Result<SecretSources, SecretsError> {
        let mut env = HashMap::new();
        for (key, value) in pairs {
            if let Some(suffix) = key.strip_prefix(ENV_PREFIX) {
                let name = suffix.to_ascii_lowercase();
                validate_secret_name(&name, "environment")?;
                if !value.is_empty() && env.insert(name.clone(), value).is_some() {
                    return Err(SecretsError::EnvCollision { name });
                }
            }
        }
        Ok(SecretSources { env })
    }

    #[test]
    fn secret_sources_debug_redacts_values() {
        // Secret source values may come from TAU_SECRET_* environment variables;
        // Debug output must stay safe if the type is logged accidentally.
        let sources = SecretSources {
            env: HashMap::from([("mail_password".to_owned(), "super-secret".to_owned())]),
        };

        let rendered = format!("{sources:?}");

        assert!(rendered.contains("mail_password"));
        assert!(rendered.contains("env_secret_count"));
        assert!(!rendered.contains("super-secret"));
    }

    #[test]
    #[allow(unsafe_code)]
    fn normalized_env_name_collisions_are_rejected() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        // SAFETY: serialized test-only environment mutation.
        unsafe {
            std::env::set_var("TAU_SECRET_COLLIDE", "one");
            std::env::set_var("TAU_SECRET_collide", "two");
        }
        let err = load_secret_sources().expect_err("collision should fail");
        assert!(err.to_string().contains("collide"));
        assert!(std::env::var("TAU_SECRET_COLLIDE").is_err());
        assert!(std::env::var("TAU_SECRET_collide").is_err());

        let err = validate_env_pairs_for_test([
            ("TAU_SECRET_COLLIDE".to_owned(), "one".to_owned()),
            ("TAU_SECRET_collide".to_owned(), "two".to_owned()),
        ])
        .expect_err("collision should fail");
        assert!(err.to_string().contains("collide"));
    }
}
