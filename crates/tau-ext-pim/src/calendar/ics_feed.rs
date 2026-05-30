use std::collections::BTreeMap;
use std::io::Read;
use std::time::Duration;

use tau_proto::SecretValue;
#[cfg(test)]
use time::format_description::well_known::Rfc3339;
use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time};
use url::Url;

use super::config::{ValidatedAccount, ValidatedBackendConfig};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_ICS_BYTES: u64 = 2 * 1024 * 1024;
const ICS_CURSOR_PREFIX: &str = "ics:";

/// Read-only iCalendar feed backend.
pub struct IcsFeedBackend {
    secrets: BTreeMap<String, SecretValue>,
    agent: ureq::Agent,
}

/// One calendar visible through a backend account.
pub struct BackendCalendar {
    /// Calendar id used in tool calls.
    pub id: String,
    /// User-facing display name.
    pub display_name: String,
    /// Whether the calendar is read-only.
    pub read_only: bool,
}

/// One event parsed from an iCalendar feed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IcsEvent {
    /// Stable event id.
    pub id: String,
    /// iCalendar UID.
    pub uid: String,
    /// Event summary.
    pub summary: String,
    /// Event description.
    pub description: Option<String>,
    /// Event location.
    pub location: Option<String>,
    /// Raw start value.
    pub start: String,
    /// Raw end value.
    pub end: String,
    /// Parsed start, when unambiguous.
    pub start_utc: Option<OffsetDateTime>,
    /// Parsed end, when unambiguous.
    pub end_utc: Option<OffsetDateTime>,
    /// Event status.
    pub status: Option<String>,
    /// Whether `CLASS` marks this event private/confidential.
    pub private: bool,
    /// Organizer value.
    pub organizer: Option<String>,
    /// Attendee values.
    pub attendees: Vec<String>,
    /// Whether parsing found recurrence data that is not expanded yet.
    pub recurring: bool,
    /// Whether time filtering could not fully interpret this event's time.
    pub time_unparsed: bool,
}

/// One page of iCalendar feed events.
pub struct IcsEventPage {
    /// Events in this page.
    pub events: Vec<IcsEvent>,
    /// Cursor for the next page, when more matching events remain.
    pub next_cursor: Option<String>,
    /// Whether more matching events remain after this page.
    pub truncated: bool,
}

/// Time range for event and free-busy queries.
#[derive(Clone, Copy, Debug, Default)]
pub struct TimeRange {
    /// Inclusive lower bound.
    pub min: Option<OffsetDateTime>,
    /// Exclusive upper bound.
    pub max: Option<OffsetDateTime>,
}

impl IcsFeedBackend {
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

    /// List synthetic calendars exposed by an iCalendar feed account.
    pub fn list_calendars(&self, account: &ValidatedAccount) -> Vec<BackendCalendar> {
        account
            .allowed_calendars
            .iter()
            .map(|id| BackendCalendar {
                id: id.clone(),
                display_name: account.display_name.clone().unwrap_or_else(|| id.clone()),
                read_only: true,
            })
            .collect()
    }

    /// List events from the account's feed.
    pub fn list_events(
        &self,
        account: &ValidatedAccount,
        calendar: &str,
        range: TimeRange,
        limit: usize,
    ) -> Result<Vec<IcsEvent>, String> {
        Ok(self
            .list_events_page(account, calendar, range, limit, None)?
            .events)
    }

    /// List one cursor page of events from the account's feed.
    pub fn list_events_page(
        &self,
        account: &ValidatedAccount,
        calendar: &str,
        range: TimeRange,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<IcsEventPage, String> {
        ensure_calendar_allowed(account, calendar)?;
        let offset = parse_ics_cursor(cursor)?;
        let text = self.fetch_feed(account)?;
        let mut events = parse_ics_events(&text)?;
        events.retain(|event| event_overlaps(event, range));
        events.sort_by_key(event_sort_key);
        let truncated = offset.saturating_add(limit) < events.len();
        let next_cursor = if truncated {
            Some(format!("{ICS_CURSOR_PREFIX}{}", offset + limit))
        } else {
            None
        };
        let events = events.into_iter().skip(offset).take(limit).collect();
        Ok(IcsEventPage {
            events,
            next_cursor,
            truncated,
        })
    }

    /// Read one event from the account's feed.
    pub fn read_event(
        &self,
        account: &ValidatedAccount,
        calendar: &str,
        event_id: &str,
    ) -> Result<IcsEvent, String> {
        ensure_calendar_allowed(account, calendar)?;
        let text = self.fetch_feed(account)?;
        parse_ics_events(&text)?
            .into_iter()
            .find(|event| event.id == event_id)
            .ok_or_else(|| format!("calendar event `{event_id}` was not found"))
    }

    fn fetch_feed(&self, account: &ValidatedAccount) -> Result<String, String> {
        let url = self.feed_url(account)?;
        let mut response = self
            .agent
            .get(&url)
            .header("Accept", "text/calendar, text/plain;q=0.8, */*;q=0.1")
            .call()
            .map_err(|error| format!("fetching iCalendar feed failed: {error}"))?;
        if !response.status().is_success() {
            let code = response.status().as_u16();
            return Err(format!("iCalendar feed returned HTTP {code}"));
        }
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(MAX_ICS_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("reading iCalendar feed failed: {error}"))?;
        if MAX_ICS_BYTES < bytes.len() as u64 {
            return Err("iCalendar feed is too large".to_owned());
        }
        String::from_utf8(bytes).map_err(|_| "iCalendar feed is not valid UTF-8".to_owned())
    }

    fn feed_url(&self, account: &ValidatedAccount) -> Result<String, String> {
        let Some(ValidatedBackendConfig::IcsFeed { url_secret, url }) = &account.backend else {
            return Err(format!(
                "calendar account `{}` is not an ics_feed account",
                account.id
            ));
        };
        let url = match (url_secret, url) {
            (Some(secret), None) => self
                .secrets
                .get(secret)
                .ok_or_else(|| {
                    format!(
                        "calendar account `{}` missing url_secret `{secret}`",
                        account.id
                    )
                })?
                .expose_secret()
                .to_owned(),
            (None, Some(url)) => url.clone(),
            _ => return Err("invalid ics_feed source configuration".to_owned()),
        };
        normalize_feed_url(&url)
    }
}

fn normalize_feed_url(url: &str) -> Result<String, String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("iCalendar feed URL must not be empty".to_owned());
    }
    if trimmed.chars().any(|c| c.is_control()) {
        return Err("iCalendar feed URL must not contain control characters".to_owned());
    }
    let normalized;
    let candidate = if let Some(rest) = trimmed.strip_prefix("webcal://") {
        normalized = format!("https://{rest}");
        normalized.as_str()
    } else {
        trimmed
    };
    let parsed = Url::parse(candidate)
        .map_err(|error| format!("iCalendar feed URL must be absolute: {error}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("iCalendar feed URL must use https://, http://, or webcal://".to_owned());
    }
    if parsed.host_str().is_none() {
        return Err("iCalendar feed URL must include a host".to_owned());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("iCalendar feed URL must not include credentials".to_owned());
    }
    Ok(candidate.to_owned())
}

fn parse_ics_cursor(cursor: Option<&str>) -> Result<usize, String> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    let Some(offset) = cursor.strip_prefix(ICS_CURSOR_PREFIX) else {
        return Err("cursor is not an iCalendar feed cursor returned by this tool".to_owned());
    };
    if offset.is_empty() || !offset.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("iCalendar feed cursor is invalid".to_owned());
    }
    offset
        .parse::<usize>()
        .map_err(|_| "iCalendar feed cursor is too large".to_owned())
}

fn ensure_calendar_allowed(account: &ValidatedAccount, calendar: &str) -> Result<(), String> {
    if account
        .allowed_calendars
        .iter()
        .any(|allowed| allowed == calendar)
    {
        return Ok(());
    }
    Err(format!(
        "calendar `{calendar}` is not allowed for account `{}`",
        account.id
    ))
}

fn parse_ics_events(text: &str) -> Result<Vec<IcsEvent>, String> {
    let lines = unfold_ics_lines(text);
    let mut events = Vec::new();
    let mut current = Vec::new();
    let mut in_event = false;
    for line in lines {
        let upper = line.to_ascii_uppercase();
        match upper.as_str() {
            "BEGIN:VEVENT" if !in_event => {
                in_event = true;
                current.clear();
            }
            "END:VEVENT" if in_event => {
                if let Some(event) = parse_event(&current) {
                    events.push(event);
                }
                in_event = false;
                current.clear();
            }
            _ if in_event => current.push(line),
            _ => {}
        }
    }
    Ok(events)
}

fn unfold_ics_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::<String>::new();
    for raw in text.lines() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(last) = lines.last_mut() {
                last.push_str(&line[1..]);
            }
        } else {
            lines.push(line.to_owned());
        }
    }
    lines
}

fn parse_event(lines: &[String]) -> Option<IcsEvent> {
    let mut uid = None;
    let mut summary = None;
    let mut description = None;
    let mut location = None;
    let mut start = None;
    let mut end = None;
    let mut status = None;
    let mut organizer = None;
    let mut attendees = Vec::new();
    let mut private = false;
    let mut recurring = false;
    let mut duration_unparsed = false;

    for line in lines {
        let Some(property) = parse_property(line) else {
            continue;
        };
        match property.name.as_str() {
            "UID" => uid = Some(unescape_text(&property.value)),
            "SUMMARY" => summary = Some(unescape_text(&property.value)),
            "DESCRIPTION" => description = Some(unescape_text(&property.value)),
            "LOCATION" => location = Some(unescape_text(&property.value)),
            "DTSTART" => start = Some(parse_ics_time(&property)),
            "DTEND" => end = Some(parse_ics_time(&property)),
            "DURATION" if end.is_none() => {
                end = start.clone();
                duration_unparsed = true;
            }
            "STATUS" => status = Some(unescape_text(&property.value)),
            "CLASS" => {
                let value = property.value.trim();
                private = value.eq_ignore_ascii_case("PRIVATE")
                    || value.eq_ignore_ascii_case("CONFIDENTIAL");
            }
            "ORGANIZER" => organizer = Some(unescape_text(&property.value)),
            "ATTENDEE" => attendees.push(unescape_text(&property.value)),
            "RRULE" | "RDATE" | "EXDATE" | "RECURRENCE-ID" => recurring = true,
            _ => {}
        }
    }

    let uid = uid.unwrap_or_else(|| {
        let digest = blake3::hash(lines.join("\n").as_bytes());
        digest.to_hex()[..16].to_owned()
    });
    let start = start?;
    let end = end.unwrap_or_else(|| start.clone());
    let time_unparsed = duration_unparsed || start.utc.is_none() || end.utc.is_none();
    Some(IcsEvent {
        id: uid.clone(),
        uid,
        summary: summary.unwrap_or_else(|| "(untitled)".to_owned()),
        description,
        location,
        start: start.raw,
        end: end.raw,
        start_utc: start.utc,
        end_utc: end.utc,
        status,
        private,
        organizer,
        attendees,
        recurring,
        time_unparsed,
    })
}

struct IcsProperty {
    name: String,
    params: Vec<(String, String)>,
    value: String,
}

#[derive(Clone)]
struct ParsedIcsTime {
    raw: String,
    utc: Option<OffsetDateTime>,
}

fn parse_property(line: &str) -> Option<IcsProperty> {
    let (left, value) = line.split_once(':')?;
    let mut parts = left.split(';');
    let name = parts.next()?.to_ascii_uppercase();
    let mut params = Vec::new();
    for part in parts {
        if let Some((key, value)) = part.split_once('=') {
            params.push((key.to_ascii_uppercase(), value.trim_matches('"').to_owned()));
        }
    }
    Some(IcsProperty {
        name,
        params,
        value: value.to_owned(),
    })
}

fn parse_ics_time(property: &IcsProperty) -> ParsedIcsTime {
    let value = property.value.trim();
    let is_date = property
        .params
        .iter()
        .any(|(key, value)| key == "VALUE" && value.eq_ignore_ascii_case("DATE"));
    let utc = if is_date || value.len() == 8 {
        parse_ics_date(value)
    } else if value.ends_with('Z') {
        parse_ics_utc_datetime(value)
    } else {
        None
    };
    ParsedIcsTime {
        raw: value.to_owned(),
        utc,
    }
}

fn parse_ics_date(value: &str) -> Option<OffsetDateTime> {
    if value.len() != 8 || !value.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let year = value[0..4].parse::<i32>().ok()?;
    let month = Month::try_from(value[4..6].parse::<u8>().ok()?).ok()?;
    let day = value[6..8].parse::<u8>().ok()?;
    let date = Date::from_calendar_date(year, month, day).ok()?;
    Some(PrimitiveDateTime::new(date, Time::MIDNIGHT).assume_utc())
}

fn parse_ics_utc_datetime(value: &str) -> Option<OffsetDateTime> {
    if value.len() != 16 || !value.ends_with('Z') {
        return None;
    }
    let core = &value[..15];
    if core.as_bytes().get(8) != Some(&b'T') {
        return None;
    }
    if !core[..8].chars().all(|c| c.is_ascii_digit())
        || !core[9..].chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let date = parse_ics_date(&core[..8])?;
    let hour = core[9..11].parse::<u8>().ok()?;
    let minute = core[11..13].parse::<u8>().ok()?;
    let second = core[13..15].parse::<u8>().ok()?;
    let time = Time::from_hms(hour, minute, second).ok()?;
    Some(PrimitiveDateTime::new(date.date(), time).assume_utc())
}

fn event_overlaps(event: &IcsEvent, range: TimeRange) -> bool {
    let (Some(start), Some(end)) = (event.start_utc, event.end_utc) else {
        return true;
    };
    if let Some(min) = range.min
        && !is_before(min, end)
    {
        return false;
    }
    if let Some(max) = range.max
        && !is_before(start, max)
    {
        return false;
    }
    true
}

fn event_sort_key(event: &IcsEvent) -> Option<OffsetDateTime> {
    event.start_utc
}

fn is_before(left: OffsetDateTime, right: OffsetDateTime) -> bool {
    left < right
}

fn unescape_text(value: &str) -> String {
    let mut out = String::new();
    let mut escaped = false;
    for c in value.chars() {
        if escaped {
            match c {
                'n' | 'N' => out.push('\n'),
                '\\' | ',' | ';' => out.push(c),
                _ => out.push(c),
            }
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else {
            out.push(c);
        }
    }
    if escaped {
        out.push('\\');
    }
    out
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use super::*;
    use crate::calendar::config::{
        CalendarAccountConfig, CalendarBackendConfig, CalendarExtensionConfig,
        CalendarSelectionConfig,
    };

    #[test]
    fn parser_unfolds_and_extracts_basic_events() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:abc\r\nSUMMARY:Hello\r\n world\r\nDTSTART:20260528T120000Z\r\nDTEND:20260528T130000Z\r\nLOCATION:Room\\, 1\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

        let events = parse_ics_events(ics).expect("ics parses");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "abc");
        assert_eq!(events[0].summary, "Helloworld");
        assert_eq!(events[0].location.as_deref(), Some("Room, 1"));
        assert!(events[0].start_utc.is_some());
    }

    #[test]
    fn parser_keeps_tzid_times_but_marks_them_unparsed() {
        let ics = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:abc\nSUMMARY:Local\nDTSTART;TZID=America/Chicago:20260528T120000\nDTEND;TZID=America/Chicago:20260528T130000\nEND:VEVENT\nEND:VCALENDAR\n";

        let events = parse_ics_events(ics).expect("ics parses");

        assert_eq!(events[0].start, "20260528T120000");
        assert!(events[0].time_unparsed);
    }

    #[test]
    fn range_filter_uses_exclusive_event_overlap() {
        let ics = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:abc\nSUMMARY:UTC\nDTSTART:20260528T120000Z\nDTEND:20260528T130000Z\nEND:VEVENT\nEND:VCALENDAR\n";
        let event = parse_ics_events(ics)
            .expect("ics parses")
            .into_iter()
            .next()
            .expect("event");
        let before = OffsetDateTime::parse("2026-05-28T13:00:00Z", &Rfc3339).expect("time");
        let after = OffsetDateTime::parse("2026-05-28T14:00:00Z", &Rfc3339).expect("time");

        assert!(!event_overlaps(
            &event,
            TimeRange {
                min: Some(before),
                max: Some(after)
            }
        ));
    }
    #[test]
    fn backend_fetches_and_lists_http_feed_events() {
        let ics = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:abc\nSUMMARY:UTC\nDTSTART:20260528T120000Z\nDTEND:20260528T130000Z\nEND:VEVENT\nEND:VCALENDAR\n";
        let (url, handle) = serve_ics_once(ics);
        let cfg = CalendarExtensionConfig {
            enable: true,
            accounts: vec![CalendarAccountConfig {
                id: "feed".to_owned(),
                enable: true,
                backend: Some(CalendarBackendConfig::IcsFeed {
                    url_secret: None,
                    url: Some(url),
                }),
                calendars: CalendarSelectionConfig {
                    default: Some("main".to_owned()),
                    allow: vec!["main".to_owned()],
                },
                ..Default::default()
            }],
            ..Default::default()
        };
        let config = cfg.validate().expect("valid config");
        let account = config.accounts.get("feed").expect("feed account");
        let backend = IcsFeedBackend::new(BTreeMap::new());

        let events = backend
            .list_events(account, "main", TimeRange::default(), 10)
            .expect("events list");

        handle.join().expect("server exits");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].summary, "UTC");
    }

    #[test]
    fn backend_lists_ics_events_with_cursor_pages() {
        // Cursor paging should let the model continue a bounded calendar read
        // without asking for an unbounded feed dump.
        let ics = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:first\nSUMMARY:First\nDTSTART:20260528T120000Z\nDTEND:20260528T130000Z\nEND:VEVENT\nBEGIN:VEVENT\nUID:second\nSUMMARY:Second\nDTSTART:20260529T120000Z\nDTEND:20260529T130000Z\nEND:VEVENT\nEND:VCALENDAR\n";
        let (url, handle) = serve_ics_once(ics);
        let cfg = CalendarExtensionConfig {
            enable: true,
            accounts: vec![CalendarAccountConfig {
                id: "feed".to_owned(),
                enable: true,
                backend: Some(CalendarBackendConfig::IcsFeed {
                    url_secret: None,
                    url: Some(url),
                }),
                calendars: CalendarSelectionConfig {
                    default: Some("main".to_owned()),
                    allow: vec!["main".to_owned()],
                },
                ..Default::default()
            }],
            ..Default::default()
        };
        let config = cfg.validate().expect("valid config");
        let account = config.accounts.get("feed").expect("feed account");
        let backend = IcsFeedBackend::new(BTreeMap::new());

        let page = backend
            .list_events_page(account, "main", TimeRange::default(), 1, None)
            .expect("events page");

        handle.join().expect("server exits");
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.events[0].summary, "First");
        assert_eq!(page.next_cursor.as_deref(), Some("ics:1"));
        assert!(page.truncated);
    }

    #[test]
    fn ics_cursor_rejects_other_backend_cursors() {
        // Cursor values are intentionally backend-prefixed so agents cannot
        // accidentally replay a Google page token into an ICS feed query.
        assert_eq!(parse_ics_cursor(None), Ok(0));
        assert_eq!(parse_ics_cursor(Some("ics:12")), Ok(12));
        assert!(parse_ics_cursor(Some("google:token")).is_err());
    }

    fn serve_ics_once(body: &'static str) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut buf = [0_u8; 1024];
            let _ = stream.read(&mut buf);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/calendar\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        });
        (format!("http://{addr}/calendar.ics"), handle)
    }
}
