use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use url::Url;

/// Top-level calendar module configuration.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CalendarExtensionConfig {
    /// Whether calendar access is enabled.
    pub enable: bool,
    /// Configured calendar accounts.
    pub accounts: Vec<CalendarAccountConfig>,
    /// Calendar read/write policy.
    pub policy: CalendarPolicyConfig,
}

/// One configured calendar account.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CalendarAccountConfig {
    /// Stable account identifier used by tool commands.
    pub id: String,
    /// Per-account enable flag. Accounts are disabled unless explicitly
    /// enabled.
    pub enable: bool,
    /// Optional display name for user-facing account lists.
    pub display_name: Option<String>,
    /// Calendar backend configuration.
    pub backend: Option<CalendarBackendConfig>,
    /// Per-account calendar selection policy.
    pub calendars: CalendarSelectionConfig,
    /// Default IANA timezone for new events and date-only interpretation.
    pub timezone: Option<String>,
}

/// Backend-specific calendar account configuration.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CalendarBackendConfig {
    /// Generic read-only iCalendar feed.
    IcsFeed {
        /// Secret containing the feed URL.
        url_secret: Option<String>,
        /// Literal feed URL. Prefer `url_secret` for private feeds.
        url: Option<String>,
    },
    /// Native Google Calendar API backend.
    Google {
        /// Secret containing the OAuth client id.
        client_id_secret: String,
        /// Optional secret containing the OAuth client secret.
        client_secret_secret: Option<String>,
        /// Secret containing a Google OAuth refresh token.
        refresh_token_secret: String,
        /// Optional Google Calendar API base URL for tests or proxies.
        api_base: Option<String>,
    },
    /// Generic CalDAV backend.
    Caldav {
        /// CalDAV service URL.
        url: Option<String>,
        /// Login user name for Basic-style DAV servers.
        login: Option<String>,
        /// Secret containing a DAV password or app password.
        password_secret: Option<String>,
    },
}

/// Per-account calendar visibility configuration.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CalendarSelectionConfig {
    /// Default calendar id used when a safe command can omit `calendar`.
    pub default: Option<String>,
    /// Calendar ids the agent may see. Empty means none.
    pub allow: Vec<String>,
}

/// Calendar data exposure and mutation policy.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CalendarPolicyConfig {
    /// Read-side privacy policy.
    pub read: CalendarReadPolicyConfig,
    /// Write-side approval policy.
    pub write: CalendarWritePolicyConfig,
}

/// Calendar read privacy policy.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CalendarReadPolicyConfig {
    /// How events marked private are exposed to the model.
    pub private_events: PrivateEventsPolicy,
    /// Whether event descriptions are exposed by `read_event`.
    pub descriptions: DescriptionPolicy,
}

impl Default for CalendarReadPolicyConfig {
    fn default() -> Self {
        Self {
            private_events: PrivateEventsPolicy::BusyOnly,
            descriptions: DescriptionPolicy::ApprovedOnly,
        }
    }
}

/// Policy for events flagged private by the provider/feed.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PrivateEventsPolicy {
    /// Expose only time and busy/private flags.
    #[default]
    BusyOnly,
    /// Expose configured event detail fields.
    Details,
}

/// Policy for model-visible event descriptions.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DescriptionPolicy {
    /// Do not expose descriptions through the model tool unless a later
    /// approval flow is added.
    #[default]
    ApprovedOnly,
    /// Always expose descriptions for readable events.
    Always,
    /// Never expose descriptions.
    Omit,
}

/// Calendar write approval policy.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CalendarWritePolicyConfig {
    /// Whether model-requested calendar mutations must be queued for user
    /// approval before provider APIs are called.
    pub require_approval: bool,
    /// Maximum attendees accepted on one create/update request.
    pub max_attendees: u32,
}

impl Default for CalendarWritePolicyConfig {
    fn default() -> Self {
        Self {
            require_approval: true,
            max_attendees: 50,
        }
    }
}

/// Validated calendar configuration.
pub struct ValidatedConfig {
    /// Whether calendar access is enabled.
    pub enable: bool,
    /// Accounts keyed by configured account ID.
    pub accounts: BTreeMap<String, ValidatedAccount>,
    /// Account IDs in configuration order for deterministic display.
    pub account_order: Vec<String>,
    /// Validated calendar privacy and write policy.
    pub policy: ValidatedPolicy,
}

/// Validated calendar privacy and write policy.
pub struct ValidatedPolicy {
    /// Read-side privacy policy.
    pub read: ValidatedReadPolicy,
    /// Write-side approval policy.
    pub write: ValidatedWritePolicy,
}

/// Validated calendar read privacy policy.
pub struct ValidatedReadPolicy {
    /// How events marked private are exposed to the model.
    pub private_events: PrivateEventsPolicy,
    /// Whether event descriptions are exposed by `read_event`.
    pub descriptions: DescriptionPolicy,
}

/// Validated calendar write policy.
pub struct ValidatedWritePolicy {
    /// Whether model-requested calendar mutations must be queued for approval.
    pub require_approval: bool,
    /// Maximum attendees accepted on one create/update request.
    pub max_attendees: usize,
}

/// Validated calendar account configuration.
pub struct ValidatedAccount {
    /// Stable account identifier used by tool commands.
    pub id: String,
    /// Whether this account is enabled.
    pub enable: bool,
    /// Optional display name.
    pub display_name: Option<String>,
    /// Configured backend.
    pub backend: Option<ValidatedBackendConfig>,
    /// Default calendar id.
    pub default_calendar: Option<String>,
    /// Allowed calendar ids.
    pub allowed_calendars: Vec<String>,
    /// Default IANA timezone.
    pub timezone: Option<String>,
}

/// Validated backend-specific calendar account configuration.
pub enum ValidatedBackendConfig {
    /// Generic read-only iCalendar feed.
    IcsFeed {
        /// Secret containing the feed URL.
        url_secret: Option<String>,
        /// Literal feed URL.
        url: Option<String>,
    },
    /// Native Google Calendar API backend.
    Google {
        /// Secret containing the OAuth client id.
        client_id_secret: String,
        /// Optional secret containing the OAuth client secret.
        client_secret_secret: Option<String>,
        /// Secret containing a Google OAuth refresh token.
        refresh_token_secret: String,
        /// Optional Google Calendar API base URL for tests or proxies.
        api_base: Option<String>,
    },
    /// Generic CalDAV backend.
    Caldav {
        /// CalDAV service URL.
        url: Option<String>,
        /// Login user name for Basic-style DAV servers.
        login: Option<String>,
        /// Secret containing a DAV password or app password.
        password_secret: Option<String>,
    },
}

impl CalendarExtensionConfig {
    /// Validate this configuration and normalize account lookup structures.
    pub fn validate(self) -> Result<ValidatedConfig, String> {
        let mut ids = BTreeSet::new();
        let mut accounts = BTreeMap::new();
        let mut account_order = Vec::new();
        for account in self.accounts {
            if account.id.trim().is_empty() {
                return Err("calendar account id must not be empty".to_owned());
            }
            if !ids.insert(account.id.clone()) {
                return Err(format!("duplicate calendar account id `{}`", account.id));
            }
            validate_calendar_patterns(&account.calendars.allow)?;
            if let Some(default) = &account.calendars.default {
                validate_calendar_pattern(default)?;
                if !account
                    .calendars
                    .allow
                    .iter()
                    .any(|allowed| allowed == default)
                {
                    return Err(format!(
                        "calendar account `{}` default calendar `{default}` must also be listed in calendars.allow",
                        account.id
                    ));
                }
            }
            let id = account.id.clone();
            account_order.push(id.clone());
            accounts.insert(id, ValidatedAccount::from_config(account)?);
        }
        Ok(ValidatedConfig {
            enable: self.enable,
            accounts,
            account_order,
            policy: self.policy.validate()?,
        })
    }
}

impl CalendarPolicyConfig {
    fn validate(self) -> Result<ValidatedPolicy, String> {
        if self.write.max_attendees == 0 {
            return Err("calendar policy write.max_attendees must be positive".to_owned());
        }
        if 200 < self.write.max_attendees {
            return Err("calendar policy write.max_attendees must be <= 200".to_owned());
        }
        Ok(ValidatedPolicy {
            read: ValidatedReadPolicy {
                private_events: self.read.private_events,
                descriptions: self.read.descriptions,
            },
            write: ValidatedWritePolicy {
                require_approval: self.write.require_approval,
                max_attendees: self.write.max_attendees as usize,
            },
        })
    }
}

impl ValidatedAccount {
    fn from_config(value: CalendarAccountConfig) -> Result<Self, String> {
        let backend = match value.backend {
            Some(CalendarBackendConfig::IcsFeed { url_secret, url }) => {
                validate_ics_feed_source(url_secret.as_deref(), url.as_deref())?;
                Some(ValidatedBackendConfig::IcsFeed { url_secret, url })
            }
            Some(CalendarBackendConfig::Google {
                client_id_secret,
                client_secret_secret,
                refresh_token_secret,
                api_base,
            }) => {
                validate_secret_name("google client_id_secret", &client_id_secret)?;
                validate_secret_name("google refresh_token_secret", &refresh_token_secret)?;
                if let Some(secret) = &client_secret_secret {
                    validate_secret_name("google client_secret_secret", secret)?;
                }
                let api_base = api_base
                    .map(|api_base| normalize_backend_url("google api_base", &api_base))
                    .transpose()?;
                Some(ValidatedBackendConfig::Google {
                    client_id_secret,
                    client_secret_secret,
                    refresh_token_secret,
                    api_base,
                })
            }
            Some(CalendarBackendConfig::Caldav {
                url,
                login,
                password_secret,
            }) => Some(ValidatedBackendConfig::Caldav {
                url,
                login,
                password_secret,
            }),
            None => None,
        };
        Ok(Self {
            id: value.id,
            enable: value.enable,
            display_name: value.display_name,
            backend,
            default_calendar: value.calendars.default,
            allowed_calendars: value.calendars.allow,
            timezone: value.timezone,
        })
    }

    /// Return the stable backend kind name.
    pub fn backend_kind(&self) -> &'static str {
        match &self.backend {
            Some(ValidatedBackendConfig::IcsFeed { .. }) => "ics_feed",
            Some(ValidatedBackendConfig::Google { .. }) => "google",
            Some(ValidatedBackendConfig::Caldav { .. }) => "caldav",
            None => "none",
        }
    }
}

fn validate_ics_feed_source(url_secret: Option<&str>, url: Option<&str>) -> Result<(), String> {
    match (url_secret, url) {
        (Some(secret), None) if secret.trim().is_empty() => {
            Err("ics_feed url_secret must not be empty".to_owned())
        }
        (None, Some(url)) if url.trim().is_empty() => {
            Err("ics_feed url must not be empty".to_owned())
        }
        (Some(_), None) | (None, Some(_)) => Ok(()),
        (None, None) => Err("ics_feed requires exactly one of url_secret or url".to_owned()),
        (Some(_), Some(_)) => Err("ics_feed accepts only one of url_secret or url".to_owned()),
    }
}

fn normalize_backend_url(field: &str, value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    if trimmed.chars().any(|c| c.is_control()) {
        return Err(format!("{field} must not contain control characters"));
    }
    let url =
        Url::parse(trimmed).map_err(|error| format!("{field} must be an absolute URL: {error}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("{field} must use http:// or https://"));
    }
    let Some(host) = url.host_str() else {
        return Err(format!("{field} must include a host"));
    };
    if url.scheme() == "http" && !is_loopback_host(host) {
        return Err(format!(
            "{field} may use http:// only for localhost or loopback test proxies"
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!("{field} must not include credentials"));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(format!("{field} must not include query or fragment"));
    }
    Ok(trimmed.trim_end_matches('/').to_owned())
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host == "127.0.0.1"
        || host == "::1"
        || host == "[::1]"
}

fn validate_secret_name(field: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return Err(format!(
            "{field} may only contain ASCII letters, digits, '_', '-', '.'"
        ));
    }
    Ok(())
}

fn validate_calendar_patterns(patterns: &[String]) -> Result<(), String> {
    for pattern in patterns {
        validate_calendar_pattern(pattern)?;
    }
    Ok(())
}

fn validate_calendar_pattern(value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err("calendar id pattern must not be empty".to_owned());
    }
    if value.chars().any(|c| c.is_control()) {
        return Err("calendar id pattern must not contain control characters".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_config_normalizes_api_base_and_rejects_query_fragments() {
        // The backend appends fixed API paths and query strings, so accepting a
        // configured query or fragment would create ambiguous request targets.
        let cfg = CalendarExtensionConfig {
            enable: true,
            accounts: vec![CalendarAccountConfig {
                id: "google".to_owned(),
                backend: Some(google_backend_with_api_base("https://proxy.example/api///")),
                ..Default::default()
            }],
            ..Default::default()
        };

        let config = cfg.validate().expect("api base validates");
        let account = config.accounts.get("google").expect("google account");
        let Some(ValidatedBackendConfig::Google { api_base, .. }) = &account.backend else {
            panic!("google backend expected");
        };
        assert_eq!(api_base.as_deref(), Some("https://proxy.example/api"));

        let cfg = CalendarExtensionConfig {
            enable: true,
            accounts: vec![CalendarAccountConfig {
                id: "google".to_owned(),
                backend: Some(google_backend_with_api_base(
                    "https://proxy.example/api?x=1",
                )),
                ..Default::default()
            }],
            ..Default::default()
        };
        let err = match cfg.validate() {
            Ok(_) => panic!("query must be rejected"),
            Err(err) => err,
        };
        assert!(err.contains("query or fragment"), "{err}");
    }

    #[test]
    fn google_config_rejects_unsafe_secret_names() {
        // Secret names are looked up in the harness-provided map, not shell
        // expanded paths. Keep them narrow to avoid surprising config meanings.
        let cfg = CalendarExtensionConfig {
            enable: true,
            accounts: vec![CalendarAccountConfig {
                id: "google".to_owned(),
                backend: Some(CalendarBackendConfig::Google {
                    client_id_secret: "../client".to_owned(),
                    client_secret_secret: None,
                    refresh_token_secret: "refresh".to_owned(),
                    api_base: None,
                }),
                ..Default::default()
            }],
            ..Default::default()
        };

        let err = match cfg.validate() {
            Ok(_) => panic!("path-like secret must be rejected"),
            Err(err) => err,
        };
        assert!(err.contains("may only contain"), "{err}");
    }

    #[test]
    fn google_config_allows_http_only_for_loopback_api_base() {
        // A non-HTTPS API base receives bearer tokens; keep plain HTTP limited
        // to local test proxies.
        let cfg = CalendarExtensionConfig {
            enable: true,
            accounts: vec![CalendarAccountConfig {
                id: "google".to_owned(),
                backend: Some(google_backend_with_api_base("http://127.0.0.1:8080/api")),
                ..Default::default()
            }],
            ..Default::default()
        };
        cfg.validate().expect("loopback http validates");

        let cfg = CalendarExtensionConfig {
            enable: true,
            accounts: vec![CalendarAccountConfig {
                id: "google".to_owned(),
                backend: Some(google_backend_with_api_base("http://proxy.example/api")),
                ..Default::default()
            }],
            ..Default::default()
        };
        let err = match cfg.validate() {
            Ok(_) => panic!("non-loopback http must be rejected"),
            Err(err) => err,
        };
        assert!(err.contains("loopback"), "{err}");
    }

    fn google_backend_with_api_base(api_base: &str) -> CalendarBackendConfig {
        CalendarBackendConfig::Google {
            client_id_secret: "client".to_owned(),
            client_secret_secret: None,
            refresh_token_secret: "refresh".to_owned(),
            api_base: Some(api_base.to_owned()),
        }
    }
}
