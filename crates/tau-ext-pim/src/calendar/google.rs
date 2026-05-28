use std::collections::BTreeMap;
use std::io::Read;
use std::time::Duration;

use serde_json::Value;
use tau_proto::SecretValue;
use time::format_description::well_known::Rfc3339;
use url::form_urlencoded;

use super::config::{ValidatedAccount, ValidatedBackendConfig};
use super::ics_feed::TimeRange;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_CALENDAR_API_BASE: &str = "https://www.googleapis.com/calendar/v3";
const MAX_ERROR_BODY_BYTES: usize = 4096;

/// Read-capable Google Calendar API backend.
pub struct GoogleBackend {
    secrets: BTreeMap<String, SecretValue>,
    agent: ureq::Agent,
}

/// One Google calendar visible to the account.
pub struct GoogleCalendar {
    /// Calendar id used in tool and API calls.
    pub id: String,
    /// Calendar display name.
    pub summary: String,
    /// Whether this is the authenticated user's primary calendar.
    pub primary: bool,
    /// Whether the calendar is read-only for this authenticated user.
    pub read_only: bool,
}

/// One Google Calendar event.
pub struct GoogleEvent {
    /// Backend event id.
    pub id: String,
    /// Event ETag.
    pub etag: Option<String>,
    /// iCalendar UID, when Google exposes it.
    pub i_cal_uid: Option<String>,
    /// Event summary.
    pub summary: String,
    /// Event description.
    pub description: Option<String>,
    /// Event location.
    pub location: Option<String>,
    /// Event start date or date-time.
    pub start: String,
    /// Event end date or date-time.
    pub end: String,
    /// Event status.
    pub status: Option<String>,
    /// Organizer email or display name.
    pub organizer: Option<String>,
    /// Attendee emails.
    pub attendees: Vec<String>,
    /// Whether the event is part of a recurring series.
    pub recurring: bool,
}

impl GoogleBackend {
    /// Build a backend using the extension-authorized secret set.
    pub fn new(secrets: BTreeMap<String, SecretValue>) -> Self {
        let tls_config = ureq::tls::TlsConfig::builder()
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(REQUEST_TIMEOUT))
            .http_status_as_error(false)
            .tls_config(tls_config)
            .build();
        let agent = ureq::Agent::new_with_config(config);
        Self { secrets, agent }
    }

    /// List Google calendars allowed by account policy.
    pub fn list_calendars(
        &self,
        account: &ValidatedAccount,
    ) -> Result<Vec<GoogleCalendar>, String> {
        let token = self.access_token(account)?;
        let api_base = api_base(account)?;
        let url = format!("{api_base}/users/me/calendarList");
        let json = self.get_json(&url, &token)?;
        let calendars = json
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(parse_calendar)
            .filter_map(|calendar| allowed_google_calendar(account, calendar))
            .collect();
        Ok(calendars)
    }

    /// List Google events in a calendar.
    pub fn list_events(
        &self,
        account: &ValidatedAccount,
        calendar_id: &str,
        range: TimeRange,
        limit: usize,
    ) -> Result<Vec<GoogleEvent>, String> {
        ensure_google_calendar_allowed(account, calendar_id)?;
        let token = self.access_token(account)?;
        let api_base = api_base(account)?;
        let mut query = form_urlencoded::Serializer::new(String::new());
        query.append_pair("singleEvents", "true");
        query.append_pair("orderBy", "startTime");
        query.append_pair("maxResults", &limit.to_string());
        if let Some(min) = range.min {
            query.append_pair(
                "timeMin",
                &min.format(&Rfc3339)
                    .map_err(|error| format!("formatting time_min failed: {error}"))?,
            );
        }
        if let Some(max) = range.max {
            query.append_pair(
                "timeMax",
                &max.format(&Rfc3339)
                    .map_err(|error| format!("formatting time_max failed: {error}"))?,
            );
        }
        let url = format!(
            "{api_base}/calendars/{}/events?{}",
            encode_path_segment(calendar_id),
            query.finish()
        );
        let json = self.get_json(&url, &token)?;
        let events = json
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(parse_event)
            .collect();
        Ok(events)
    }

    /// Read one Google event.
    pub fn read_event(
        &self,
        account: &ValidatedAccount,
        calendar_id: &str,
        event_id: &str,
    ) -> Result<GoogleEvent, String> {
        ensure_google_calendar_allowed(account, calendar_id)?;
        let token = self.access_token(account)?;
        let api_base = api_base(account)?;
        let url = format!(
            "{api_base}/calendars/{}/events/{}",
            encode_path_segment(calendar_id),
            encode_path_segment(event_id)
        );
        parse_event(&self.get_json(&url, &token)?).ok_or_else(|| {
            format!("Google event `{event_id}` response was missing required fields")
        })
    }

    fn access_token(&self, account: &ValidatedAccount) -> Result<String, String> {
        let Some(ValidatedBackendConfig::Google {
            client_id_secret,
            client_secret_secret,
            refresh_token_secret,
            ..
        }) = &account.backend
        else {
            return Err(format!(
                "calendar account `{}` is not a google account",
                account.id
            ));
        };
        let client_id = self.secret(client_id_secret)?;
        let refresh_token = self.secret(refresh_token_secret)?;
        let mut body = form_urlencoded::Serializer::new(String::new());
        body.append_pair("client_id", &client_id);
        body.append_pair("refresh_token", &refresh_token);
        body.append_pair("grant_type", "refresh_token");
        if let Some(secret_name) = client_secret_secret {
            body.append_pair("client_secret", &self.secret(secret_name)?);
        }
        let mut response = self
            .agent
            .post(GOOGLE_TOKEN_URL)
            .content_type("application/x-www-form-urlencoded")
            .send(body.finish())
            .map_err(|error| format!("refreshing Google access token failed: {error}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "refreshing Google access token returned HTTP {}: {}",
                response.status().as_u16(),
                read_error_body(&mut response)
            ));
        }
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|error| format!("reading Google token response failed: {error}"))?;
        let json: Value = serde_json::from_str(&text)
            .map_err(|error| format!("Google token response was not JSON: {error}"))?;
        json.get("access_token")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "Google token response missing access_token".to_owned())
    }

    fn get_json(&self, url: &str, access_token: &str) -> Result<Value, String> {
        let mut response = self
            .agent
            .get(url)
            .header("Authorization", format!("Bearer {access_token}"))
            .header("Accept", "application/json")
            .call()
            .map_err(|error| format!("Google Calendar API request failed: {error}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "Google Calendar API returned HTTP {}: {}",
                response.status().as_u16(),
                read_error_body(&mut response)
            ));
        }
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|error| format!("reading Google Calendar API response failed: {error}"))?;
        serde_json::from_str(&text)
            .map_err(|error| format!("Google Calendar API response was not JSON: {error}"))
    }

    fn secret(&self, name: &str) -> Result<String, String> {
        self.secrets
            .get(name)
            .map(|secret| secret.expose_secret().to_owned())
            .ok_or_else(|| format!("Google calendar secret `{name}` was not provided"))
    }
}

fn api_base(account: &ValidatedAccount) -> Result<&str, String> {
    let Some(ValidatedBackendConfig::Google { api_base, .. }) = &account.backend else {
        return Err(format!(
            "calendar account `{}` is not a google account",
            account.id
        ));
    };
    Ok(api_base.as_deref().unwrap_or(GOOGLE_CALENDAR_API_BASE))
}

fn allowed_google_calendar(
    account: &ValidatedAccount,
    mut calendar: GoogleCalendar,
) -> Option<GoogleCalendar> {
    if google_calendar_id_allowed(account, &calendar.id) {
        return Some(calendar);
    }
    if calendar.primary && google_calendar_id_allowed(account, "primary") {
        calendar.id = "primary".to_owned();
        return Some(calendar);
    }
    None
}

fn google_calendar_id_allowed(account: &ValidatedAccount, calendar_id: &str) -> bool {
    account
        .allowed_calendars
        .iter()
        .any(|allowed| allowed == calendar_id)
}

fn ensure_google_calendar_allowed(
    account: &ValidatedAccount,
    calendar_id: &str,
) -> Result<(), String> {
    if google_calendar_id_allowed(account, calendar_id) {
        return Ok(());
    }
    Err(format!(
        "calendar `{calendar_id}` is not allowed for account `{}`",
        account.id
    ))
}

fn parse_calendar(value: &Value) -> Option<GoogleCalendar> {
    let id = value.get("id")?.as_str()?.to_owned();
    let summary = value
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or(&id)
        .to_owned();
    let access_role = value
        .get("accessRole")
        .and_then(Value::as_str)
        .unwrap_or("reader");
    let primary = value
        .get("primary")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Some(GoogleCalendar {
        id,
        summary,
        primary,
        read_only: matches!(access_role, "freeBusyReader" | "reader"),
    })
}

fn parse_event(value: &Value) -> Option<GoogleEvent> {
    let id = value.get("id")?.as_str()?.to_owned();
    let start = google_event_time(value.get("start")?)?;
    let end = google_event_time(value.get("end")?)?;
    let attendees = value
        .get("attendees")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|attendee| attendee.get("email").and_then(Value::as_str))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let organizer = value.get("organizer").and_then(|organizer| {
        organizer
            .get("email")
            .or_else(|| organizer.get("displayName"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    Some(GoogleEvent {
        id,
        etag: value.get("etag").and_then(Value::as_str).map(str::to_owned),
        i_cal_uid: value
            .get("iCalUID")
            .and_then(Value::as_str)
            .map(str::to_owned),
        summary: value
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("(untitled)")
            .to_owned(),
        description: value
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned),
        location: value
            .get("location")
            .and_then(Value::as_str)
            .map(str::to_owned),
        start,
        end,
        status: value
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_owned),
        organizer,
        attendees,
        recurring: value.get("recurringEventId").is_some() || value.get("recurrence").is_some(),
    })
}

fn google_event_time(value: &Value) -> Option<String> {
    value
        .get("dateTime")
        .or_else(|| value.get("date"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn encode_path_segment(value: &str) -> String {
    form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn read_error_body(response: &mut ureq::http::Response<ureq::Body>) -> String {
    let mut bytes = Vec::new();
    let _ = response
        .body_mut()
        .as_reader()
        .take(MAX_ERROR_BODY_BYTES as u64 + 1)
        .read_to_end(&mut bytes);
    if MAX_ERROR_BODY_BYTES < bytes.len() {
        bytes.truncate(MAX_ERROR_BODY_BYTES);
    }
    sanitize_error_text(&String::from_utf8_lossy(&bytes))
}

fn sanitize_error_text(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_calendar_list_items() {
        let json = serde_json::json!({
            "id": "primary",
            "summary": "Primary",
            "accessRole": "reader"
        });

        let calendar = parse_calendar(&json).expect("calendar parses");

        assert_eq!(calendar.id, "primary");
        assert!(calendar.read_only);
    }

    #[test]
    fn primary_alias_is_tool_facing_when_allowed() {
        // Google calendarList returns the primary calendar's email-like id, but
        // the events API accepts the stable `primary` alias. Keep list output
        // consistent with configs that allow only that alias.
        let account = google_account(vec!["primary"]);
        let json = serde_json::json!({
            "id": "user@example.com",
            "summary": "Personal",
            "primary": true,
            "accessRole": "owner"
        });

        let calendar = allowed_google_calendar(&account, parse_calendar(&json).expect("calendar"))
            .expect("primary alias allowed");

        assert_eq!(calendar.id, "primary");
        assert_eq!(calendar.summary, "Personal");
    }

    #[test]
    fn calendar_summary_does_not_grant_google_access() {
        // Display names are mutable and not unique. Access checks intentionally
        // use Google ids plus the explicit `primary` alias only.
        let account = google_account(vec!["Work"]);
        let json = serde_json::json!({
            "id": "work@example.com",
            "summary": "Work",
            "accessRole": "reader"
        });

        let calendar = allowed_google_calendar(&account, parse_calendar(&json).expect("calendar"));

        assert!(calendar.is_none());
    }

    #[test]
    fn parses_event_date_times_dates_and_attendees() {
        let json = serde_json::json!({
            "id": "evt",
            "etag": "abc",
            "summary": "Meeting",
            "start": { "dateTime": "2026-05-28T12:00:00Z" },
            "end": { "date": "2026-05-29" },
            "attendees": [{ "email": "a@example.com" }],
            "recurringEventId": "series"
        });

        let event = parse_event(&json).expect("event parses");

        assert_eq!(event.id, "evt");
        assert_eq!(event.end, "2026-05-29");
        assert_eq!(event.attendees, vec!["a@example.com"]);
        assert!(event.recurring);
    }

    fn google_account(allowed_calendars: Vec<&str>) -> ValidatedAccount {
        ValidatedAccount {
            id: "google".to_owned(),
            enable: true,
            display_name: None,
            backend: Some(ValidatedBackendConfig::Google {
                client_id_secret: "client".to_owned(),
                client_secret_secret: None,
                refresh_token_secret: "refresh".to_owned(),
                api_base: None,
            }),
            default_calendar: None,
            allowed_calendars: allowed_calendars.into_iter().map(str::to_owned).collect(),
            timezone: None,
        }
    }
}
