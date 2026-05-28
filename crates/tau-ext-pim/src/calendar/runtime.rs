use std::collections::BTreeMap;

use tau_proto::{CborValue, Event, SecretValue, ToolError, ToolResult, ToolStarted};

use super::actions;
use super::config::{
    CalendarExtensionConfig, ValidatedAccount, ValidatedBackendConfig, ValidatedConfig,
};
use super::google::{GoogleBackend, GoogleEvent};
use super::ics_feed::{
    IcsEvent, IcsFeedBackend, TimeRange, default_calendar_id, parse_rfc3339_bound,
};
use super::tool::{CalendarArgs, CalendarCommand, ToolInvocation};

const LIST_ACCOUNTS_FORMAT: &str =
    "format: id flags backend default_calendar timezone display_name";
const LIST_CALENDARS_FORMAT: &str = "format: account calendar flags backend display_name";
const LIST_EVENTS_FORMAT: &str = "format: account calendar event_id start end flags status summary";
const FREE_BUSY_FORMAT: &str = "format: account calendar event_id start end flags";
const DEFAULT_EVENT_LIMIT: u32 = 50;
const MAX_EVENT_LIMIT: u32 = 100;

/// Runtime state for the calendar module.
pub struct RuntimeState {
    config_state: ConfigState,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            config_state: ConfigState::Unconfigured,
        }
    }
}

enum ConfigState {
    Unconfigured,
    Configured(Engine),
    Rejected { reason: String },
}

struct Engine {
    config: ValidatedConfig,
    google: GoogleBackend,
    ics_feed: IcsFeedBackend,
}

impl RuntimeState {
    /// Configure the calendar module from an already-decoded calendar config.
    pub fn configure_with_config(
        &mut self,
        cfg: CalendarExtensionConfig,
        secrets: BTreeMap<String, SecretValue>,
    ) -> Result<(), String> {
        match cfg.validate() {
            Ok(config) => {
                self.config_state = ConfigState::Configured(Engine {
                    config,
                    google: GoogleBackend::new(secrets.clone()),
                    ics_feed: IcsFeedBackend::new(secrets),
                });
                Ok(())
            }
            Err(message) => {
                self.config_state = ConfigState::Rejected {
                    reason: message.clone(),
                };
                Err(message)
            }
        }
    }

    /// Dispatch a model-visible `calendar` tool invocation.
    pub fn dispatch(&mut self, invoke: ToolStarted) -> Event {
        let result = match &self.config_state {
            ConfigState::Configured(engine) => engine.dispatch(&invoke.arguments),
            ConfigState::Unconfigured => Err("calendar module has not been configured".to_owned()),
            ConfigState::Rejected { reason } => Err(format!(
                "calendar module configuration was rejected: {reason}"
            )),
        };
        match result {
            Ok(text) => tool_result(invoke, text),
            Err(message) => tool_error(invoke, message),
        }
    }

    /// Dispatch a user `/calendar` action invocation.
    pub fn dispatch_action(&mut self, invoke: tau_proto::ActionInvoke) -> Event {
        actions::dispatch_action(invoke)
    }
}

enum BackendEvent {
    Ics(IcsEvent),
    Google(GoogleEvent),
}

impl Engine {
    fn dispatch(&self, arguments: &CborValue) -> Result<String, String> {
        let invocation: ToolInvocation = arguments
            .deserialized()
            .map_err(|error| format!("invalid calendar tool arguments: {error}"))?;
        match invocation.command {
            CalendarCommand::ListAccounts => Ok(self.list_accounts()),
            CalendarCommand::ListCalendars => self.list_calendars(&invocation.args),
            CalendarCommand::ListEvents => self.list_events(&invocation.args),
            CalendarCommand::ReadEvent => self.read_event(&invocation.args),
            CalendarCommand::FreeBusy => self.free_busy(&invocation.args),
            CalendarCommand::CreateEvent
            | CalendarCommand::UpdateEvent
            | CalendarCommand::DeleteEvent
            | CalendarCommand::RespondInvite => {
                invocation.args.note_reserved_write_fields();
                Err(format!(
                    "calendar command `{}` is not available for read-only backends yet",
                    command_name(invocation.command)
                ))
            }
        }
    }

    fn list_accounts(&self) -> String {
        let mut lines = vec![LIST_ACCOUNTS_FORMAT.to_owned()];
        if !self.config.enable {
            return lines.join("\n");
        }
        for account_id in &self.config.account_order {
            let Some(account) = self.config.accounts.get(account_id) else {
                continue;
            };
            if !account.enable {
                continue;
            }
            let default_calendar = account.default_calendar.as_deref().unwrap_or("-");
            let timezone = account.timezone.as_deref().unwrap_or("-");
            let display_name = account.display_name.as_deref().unwrap_or("-");
            lines.push(format!(
                "{} {} {} {} {} {}",
                safe_field(&account.id),
                "enabled",
                safe_field(account.backend_kind()),
                safe_field(default_calendar),
                safe_field(timezone),
                safe_field(display_name)
            ));
        }
        lines.join("\n")
    }

    fn list_calendars(&self, args: &CalendarArgs) -> Result<String, String> {
        let mut lines = vec![LIST_CALENDARS_FORMAT.to_owned()];
        if !self.config.enable {
            return Ok(lines.join("\n"));
        }
        let accounts = self.accounts_for_read(args.account.as_deref())?;
        for account in accounts {
            match &account.backend {
                Some(ValidatedBackendConfig::IcsFeed { .. }) => {
                    for calendar in self.ics_feed.list_calendars(account) {
                        let flags = if calendar.read_only {
                            "read_only"
                        } else {
                            "writable"
                        };
                        lines.push(format!(
                            "{} {} {} {} {}",
                            safe_field(&account.id),
                            safe_field(&calendar.id),
                            flags,
                            safe_field(account.backend_kind()),
                            safe_field(&calendar.display_name)
                        ));
                    }
                }
                Some(ValidatedBackendConfig::Google { .. }) => {
                    for calendar in self.google.list_calendars(account)? {
                        let flags = if calendar.read_only {
                            "read_only"
                        } else {
                            "writable"
                        };
                        lines.push(format!(
                            "{} {} {} {} {}",
                            safe_field(&account.id),
                            safe_field(&calendar.id),
                            flags,
                            safe_field(account.backend_kind()),
                            safe_field(&calendar.summary)
                        ));
                    }
                }
                Some(ValidatedBackendConfig::Caldav { .. }) | None => {}
            }
        }
        Ok(lines.join("\n"))
    }

    fn list_events(&self, args: &CalendarArgs) -> Result<String, String> {
        let limit = normalized_limit(args.limit)?;
        let range = parse_range(args)?;
        let account = self.single_account(args.account.as_deref())?;
        let calendar = self.calendar_arg(account, args.calendar.as_deref())?;
        let events = self.events_for_account(account, calendar, range, limit)?;
        let mut lines = vec![LIST_EVENTS_FORMAT.to_owned()];
        for event in events {
            lines.push(format_event_line(account, calendar, &event));
        }
        Ok(lines.join("\n"))
    }

    fn read_event(&self, args: &CalendarArgs) -> Result<String, String> {
        let event_id = required_arg(args.event_id.as_deref(), "event_id")?;
        let account = self.single_account(args.account.as_deref())?;
        let calendar = self.calendar_arg(account, args.calendar.as_deref())?;
        let event = match &account.backend {
            Some(ValidatedBackendConfig::IcsFeed { .. }) => {
                BackendEvent::Ics(self.ics_feed.read_event(account, calendar, event_id)?)
            }
            Some(ValidatedBackendConfig::Google { .. }) => {
                BackendEvent::Google(self.google.read_event(account, calendar, event_id)?)
            }
            Some(ValidatedBackendConfig::Caldav { .. }) | None => {
                return Err(format!(
                    "calendar account `{}` backend `{}` does not support read_event yet",
                    account.id,
                    account.backend_kind()
                ));
            }
        };
        Ok(format_event_detail(account, calendar, &event))
    }

    fn free_busy(&self, args: &CalendarArgs) -> Result<String, String> {
        let limit = normalized_limit(args.limit)?;
        let range = parse_range(args)?;
        let account = self.single_account(args.account.as_deref())?;
        let calendar = self.calendar_arg(account, args.calendar.as_deref())?;
        let events = self.events_for_account(account, calendar, range, limit)?;
        let mut lines = vec![FREE_BUSY_FORMAT.to_owned()];
        for event in events {
            lines.push(format_free_busy_line(account, calendar, &event));
        }
        Ok(lines.join("\n"))
    }

    fn events_for_account(
        &self,
        account: &ValidatedAccount,
        calendar: &str,
        range: TimeRange,
        limit: usize,
    ) -> Result<Vec<BackendEvent>, String> {
        match &account.backend {
            Some(ValidatedBackendConfig::IcsFeed { .. }) => Ok(self
                .ics_feed
                .list_events(account, calendar, range, limit)?
                .into_iter()
                .map(BackendEvent::Ics)
                .collect()),
            Some(ValidatedBackendConfig::Google { .. }) => Ok(self
                .google
                .list_events(account, calendar, range, limit)?
                .into_iter()
                .map(BackendEvent::Google)
                .collect()),
            Some(ValidatedBackendConfig::Caldav { .. }) | None => Err(format!(
                "calendar account `{}` backend `{}` does not support event reads yet",
                account.id,
                account.backend_kind()
            )),
        }
    }

    fn accounts_for_read(&self, account: Option<&str>) -> Result<Vec<&ValidatedAccount>, String> {
        if let Some(account_id) = account {
            return Ok(vec![self.account_by_id(account_id)?]);
        }
        Ok(self
            .config
            .account_order
            .iter()
            .filter_map(|id| self.config.accounts.get(id))
            .filter(|account| account.enable)
            .collect())
    }

    fn single_account(&self, account: Option<&str>) -> Result<&ValidatedAccount, String> {
        if let Some(account_id) = account {
            return self.account_by_id(account_id);
        }
        let mut accounts = self.accounts_for_read(None)?.into_iter();
        let Some(first) = accounts.next() else {
            return Err("no enabled calendar accounts are configured".to_owned());
        };
        if accounts.next().is_some() {
            return Err(
                "account is required when multiple calendar accounts are enabled".to_owned(),
            );
        }
        Ok(first)
    }

    fn account_by_id(&self, account_id: &str) -> Result<&ValidatedAccount, String> {
        let account = self
            .config
            .accounts
            .get(account_id)
            .ok_or_else(|| format!("unknown calendar account `{account_id}`"))?;
        if !self.config.enable {
            return Err("calendar module is disabled".to_owned());
        }
        if !account.enable {
            return Err(format!("calendar account `{account_id}` is disabled"));
        }
        Ok(account)
    }

    fn calendar_arg<'a>(
        &self,
        account: &'a ValidatedAccount,
        calendar: Option<&'a str>,
    ) -> Result<&'a str, String> {
        if let Some(calendar) = calendar {
            return Ok(calendar);
        }
        let Some(calendar) = default_calendar_id(account) else {
            return Err(format!(
                "calendar is required for account `{}` because no default calendar is configured",
                account.id
            ));
        };
        Ok(calendar)
    }
}

fn parse_range(args: &CalendarArgs) -> Result<TimeRange, String> {
    Ok(TimeRange {
        min: parse_rfc3339_bound(args.time_min.as_deref(), "time_min")?,
        max: parse_rfc3339_bound(args.time_max.as_deref(), "time_max")?,
    })
}

fn normalized_limit(limit: Option<u32>) -> Result<usize, String> {
    let limit = limit.unwrap_or(DEFAULT_EVENT_LIMIT);
    if limit == 0 {
        return Err("limit must be a positive integer".to_owned());
    }
    let capped = if MAX_EVENT_LIMIT < limit {
        MAX_EVENT_LIMIT
    } else {
        limit
    };
    Ok(capped as usize)
}

fn required_arg<'a>(value: Option<&'a str>, name: &str) -> Result<&'a str, String> {
    match value {
        Some(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(format!("{name} is required")),
    }
}

fn format_event_line(account: &ValidatedAccount, calendar: &str, event: &BackendEvent) -> String {
    let status = event_status(event).unwrap_or("-");
    format!(
        "{} {} {} {} {} {} {} {}",
        safe_field(&account.id),
        safe_field(calendar),
        safe_field(event_id(event)),
        safe_field(event_start(event)),
        safe_field(event_end(event)),
        event_flags(event),
        safe_field(status),
        safe_field(event_summary(event))
    )
}

fn format_free_busy_line(
    account: &ValidatedAccount,
    calendar: &str,
    event: &BackendEvent,
) -> String {
    format!(
        "{} {} {} {} {} {}",
        safe_field(&account.id),
        safe_field(calendar),
        safe_field(event_id(event)),
        safe_field(event_start(event)),
        safe_field(event_end(event)),
        event_flags(event)
    )
}

fn format_event_detail(account: &ValidatedAccount, calendar: &str, event: &BackendEvent) -> String {
    let mut lines = vec![
        "format: key value".to_owned(),
        format!("account {}", safe_field(&account.id)),
        format!("calendar {}", safe_field(calendar)),
        format!("event_id {}", safe_field(event_id(event))),
        format!("start {}", safe_field(event_start(event))),
        format!("end {}", safe_field(event_end(event))),
        format!("flags {}", event_flags(event)),
        format!("summary {}", safe_field(event_summary(event))),
    ];
    if let Some(uid) = event_uid(event) {
        lines.push(format!("uid {}", safe_field(uid)));
    }
    if let Some(etag) = event_etag(event) {
        lines.push(format!("etag {}", safe_field(etag)));
    }
    if let Some(status) = event_status(event) {
        lines.push(format!("status {}", safe_field(status)));
    }
    if let Some(location) = event_location(event) {
        lines.push(format!("location {}", safe_field(location)));
    }
    if let Some(organizer) = event_organizer(event) {
        lines.push(format!("organizer {}", safe_field(organizer)));
    }
    let attendees = event_attendees(event);
    if !attendees.is_empty() {
        lines.push(format!("attendees {}", safe_field(&attendees.join(","))));
    }
    if let Some(description) = event_description(event) {
        lines.push(format!("description {}", safe_multiline(description)));
    }
    lines.join("\n")
}

fn event_id(event: &BackendEvent) -> &str {
    match event {
        BackendEvent::Ics(event) => &event.id,
        BackendEvent::Google(event) => &event.id,
    }
}

fn event_uid(event: &BackendEvent) -> Option<&str> {
    match event {
        BackendEvent::Ics(event) => Some(&event.uid),
        BackendEvent::Google(event) => event.i_cal_uid.as_deref(),
    }
}

fn event_etag(event: &BackendEvent) -> Option<&str> {
    match event {
        BackendEvent::Ics(_) => None,
        BackendEvent::Google(event) => event.etag.as_deref(),
    }
}

fn event_summary(event: &BackendEvent) -> &str {
    match event {
        BackendEvent::Ics(event) => &event.summary,
        BackendEvent::Google(event) => &event.summary,
    }
}

fn event_description(event: &BackendEvent) -> Option<&str> {
    match event {
        BackendEvent::Ics(event) => event.description.as_deref(),
        BackendEvent::Google(event) => event.description.as_deref(),
    }
}

fn event_location(event: &BackendEvent) -> Option<&str> {
    match event {
        BackendEvent::Ics(event) => event.location.as_deref(),
        BackendEvent::Google(event) => event.location.as_deref(),
    }
}

fn event_start(event: &BackendEvent) -> &str {
    match event {
        BackendEvent::Ics(event) => &event.start,
        BackendEvent::Google(event) => &event.start,
    }
}

fn event_end(event: &BackendEvent) -> &str {
    match event {
        BackendEvent::Ics(event) => &event.end,
        BackendEvent::Google(event) => &event.end,
    }
}

fn event_status(event: &BackendEvent) -> Option<&str> {
    match event {
        BackendEvent::Ics(event) => event.status.as_deref(),
        BackendEvent::Google(event) => event.status.as_deref(),
    }
}

fn event_organizer(event: &BackendEvent) -> Option<&str> {
    match event {
        BackendEvent::Ics(event) => event.organizer.as_deref(),
        BackendEvent::Google(event) => event.organizer.as_deref(),
    }
}

fn event_attendees(event: &BackendEvent) -> &[String] {
    match event {
        BackendEvent::Ics(event) => &event.attendees,
        BackendEvent::Google(event) => &event.attendees,
    }
}

fn event_flags(event: &BackendEvent) -> String {
    let mut flags = vec!["read_only"];
    match event {
        BackendEvent::Ics(event) => {
            if event.recurring {
                flags.push("recurring_unexpanded");
            }
            if event.time_unparsed {
                flags.push("time_unparsed");
            }
        }
        BackendEvent::Google(event) => {
            if event.recurring {
                flags.push("recurring");
            }
        }
    }
    flags.join(",")
}

fn command_name(command: CalendarCommand) -> &'static str {
    match command {
        CalendarCommand::ListAccounts => "list_accounts",
        CalendarCommand::ListCalendars => "list_calendars",
        CalendarCommand::ListEvents => "list_events",
        CalendarCommand::ReadEvent => "read_event",
        CalendarCommand::FreeBusy => "free_busy",
        CalendarCommand::CreateEvent => "create_event",
        CalendarCommand::UpdateEvent => "update_event",
        CalendarCommand::DeleteEvent => "delete_event",
        CalendarCommand::RespondInvite => "respond_invite",
    }
}

fn tool_result(invoke: ToolStarted, result: String) -> Event {
    Event::ToolResult(ToolResult {
        call_id: invoke.call_id,
        tool_name: invoke.tool_name,
        tool_type: tau_proto::ToolType::Function,
        result: CborValue::Text(result),
        kind: tau_proto::ToolResultKind::Final,
        display: None,
        originator: tau_proto::PromptOriginator::User,
    })
}

fn tool_error(invoke: ToolStarted, message: String) -> Event {
    Event::ToolError(ToolError {
        call_id: invoke.call_id,
        tool_name: invoke.tool_name,
        tool_type: tau_proto::ToolType::Function,
        message,
        details: None,
        display: None,
        originator: tau_proto::PromptOriginator::User,
    })
}

fn safe_field(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("_")
}

fn safe_multiline(value: &str) -> String {
    value
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::config::{CalendarAccountConfig, CalendarBackendConfig};

    #[test]
    fn list_accounts_reports_enabled_configured_accounts() {
        let cfg = CalendarExtensionConfig {
            enable: true,
            accounts: vec![CalendarAccountConfig {
                id: "work".to_owned(),
                enable: true,
                display_name: Some("Work Calendar".to_owned()),
                backend: Some(CalendarBackendConfig::Google {
                    client_id_secret: "google_client_id".to_owned(),
                    client_secret_secret: None,
                    refresh_token_secret: "google_refresh_token".to_owned(),
                    api_base: None,
                }),
                calendars: Default::default(),
                timezone: Some("UTC".to_owned()),
            }],
        };
        let config = cfg.validate().expect("valid calendar config");
        let engine = Engine {
            config,
            google: GoogleBackend::new(BTreeMap::new()),
            ics_feed: IcsFeedBackend::new(BTreeMap::new()),
        };

        assert_eq!(
            engine.list_accounts(),
            "format: id flags backend default_calendar timezone display_name\nwork enabled google - UTC Work_Calendar"
        );
    }

    #[test]
    fn google_event_details_include_etag_for_future_safe_writes() {
        // Google read responses expose ETags that callers must preserve once
        // write support exists. Keep the read-only detail format carrying it.
        let account = ValidatedAccount {
            id: "google".to_owned(),
            enable: true,
            display_name: None,
            backend: Some(ValidatedBackendConfig::Google {
                client_id_secret: "client".to_owned(),
                client_secret_secret: None,
                refresh_token_secret: "refresh".to_owned(),
                api_base: None,
            }),
            default_calendar: Some("primary".to_owned()),
            allowed_calendars: vec!["primary".to_owned()],
            timezone: Some("UTC".to_owned()),
        };
        let event = BackendEvent::Google(GoogleEvent {
            id: "evt".to_owned(),
            etag: Some("abc".to_owned()),
            i_cal_uid: Some("uid@example.com".to_owned()),
            summary: "Team Sync".to_owned(),
            description: Some("line 1\nline 2".to_owned()),
            location: Some("Room 1".to_owned()),
            start: "2026-05-28T12:00:00Z".to_owned(),
            end: "2026-05-28T13:00:00Z".to_owned(),
            status: Some("confirmed".to_owned()),
            organizer: Some("org@example.com".to_owned()),
            attendees: vec!["a@example.com".to_owned(), "b@example.com".to_owned()],
            recurring: true,
        });

        assert_eq!(
            format_event_detail(&account, "primary", &event),
            "format: key value\naccount google\ncalendar primary\nevent_id evt\nstart 2026-05-28T12:00:00Z\nend 2026-05-28T13:00:00Z\nflags read_only,recurring\nsummary Team_Sync\nuid uid@example.com\netag abc\nstatus confirmed\nlocation Room_1\norganizer org@example.com\nattendees a@example.com,b@example.com\ndescription line 1 line 2"
        );
    }

    #[test]
    fn duplicate_account_ids_are_rejected() {
        let cfg = CalendarExtensionConfig {
            enable: true,
            accounts: vec![
                CalendarAccountConfig {
                    id: "work".to_owned(),
                    ..Default::default()
                },
                CalendarAccountConfig {
                    id: "work".to_owned(),
                    ..Default::default()
                },
            ],
        };

        let err = match cfg.validate() {
            Ok(_) => panic!("duplicate ids should fail"),
            Err(err) => err,
        };
        assert!(err.contains("duplicate calendar account id"), "{err}");
    }

    #[test]
    fn ics_feed_requires_exactly_one_url_source() {
        let cfg = CalendarExtensionConfig {
            enable: true,
            accounts: vec![CalendarAccountConfig {
                id: "feed".to_owned(),
                backend: Some(CalendarBackendConfig::IcsFeed {
                    url_secret: None,
                    url: None,
                }),
                ..Default::default()
            }],
        };

        let err = match cfg.validate() {
            Ok(_) => panic!("missing feed source should fail"),
            Err(err) => err,
        };
        assert!(err.contains("requires exactly one"), "{err}");
    }
}
