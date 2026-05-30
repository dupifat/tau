use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::de::DeserializeOwned;
use tau_proto::{
    ActionError, ActionInvoke, ActionOutput, ActionResult, CborValue, Event, SecretValue,
    ToolDisplay, ToolDisplayStats, ToolDisplayStatus, ToolError, ToolResult, ToolStarted,
};
use time_tz::{OffsetResult, PrimitiveDateTimeExt};

use super::config::{
    CalendarExtensionConfig, DescriptionPolicy, PrivateEventsPolicy, ValidatedAccount,
    ValidatedBackendConfig, ValidatedConfig, ValidatedPolicy,
};
use super::google::{GoogleBackend, GoogleEvent, GoogleEventWrite};
use super::ics_feed::{IcsEvent, IcsFeedBackend, TimeRange};
use super::state::{CalendarChangeApproval, CalendarLogEntry, GooglePendingAuth, StateStore};
use super::tool::{
    CalendarCommand, CalendarRangeArgs, CreateEventArgs, DeleteEventArgs, ListCalendarsArgs,
    NoArgs, ReadEventArgs, RespondInviteArgs, ToolInvocation, UpdateEventArgs,
};

const LIST_ACCOUNTS_FORMAT: &str = "id flags backend default_calendar timezone display_name";
const LIST_CALENDARS_FORMAT: &str = "account calendar flags backend display_name";
const LIST_EVENTS_FORMAT: &str = "account calendar event_id start end flags status summary...";
const FREE_BUSY_FORMAT: &str = "account calendar event_id start end flags";
const EVENT_DETAIL_FORMAT: &str = "key value...";
const DEFAULT_EVENT_LIMIT: u32 = 50;
const MAX_EVENT_LIMIT: u32 = 100;
const CALENDAR_LOG_DEFAULT_LIMIT: usize = 20;
const CALENDAR_LOG_MAX_LIMIT: usize = 200;
const MAX_LOG_FIELD_CHARS: usize = 512;
const MAX_LOG_REASON_CHARS: usize = 512;
const MAX_DISPLAY_LINE_CHARS: usize = 256;
const MAX_EVENT_FIELD_CHARS: usize = 512;
const MAX_EVENT_DESCRIPTION_BYTES: usize = 64 * 1024;
const MAX_EVENT_DESCRIPTION_LINES: usize = 1000;
const MAX_ATTENDEE_CHARS: usize = 320;

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
    Configured(Box<Engine>),
    Rejected { reason: String },
}

struct Engine {
    config: ValidatedConfig,
    state: StateStore,
    google: GoogleBackend,
    ics_feed: IcsFeedBackend,
}

impl RuntimeState {
    /// Configure the calendar module from an already-decoded calendar config.
    pub fn configure_with_config(
        &mut self,
        cfg: CalendarExtensionConfig,
        state_dir: Option<PathBuf>,
        secrets: BTreeMap<String, SecretValue>,
    ) -> Result<(), String> {
        let result = cfg.validate().and_then(|config| {
            let state_dir = state_dir
                .ok_or_else(|| "calendar module requires Configure.state_dir".to_owned())?;
            Ok(Engine {
                config,
                state: StateStore::open(state_dir)?,
                google: GoogleBackend::new(secrets.clone()),
                ics_feed: IcsFeedBackend::new(secrets),
            })
        });
        match result {
            Ok(engine) => {
                self.config_state = ConfigState::Configured(Box::new(engine));
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
            ConfigState::Unconfigured => error_envelope(
                None,
                "configuration_error",
                "calendar module has not been configured",
            ),
            ConfigState::Rejected { reason } => error_envelope(
                None,
                "configuration_error",
                &format!("calendar module configuration was rejected: {reason}"),
            ),
        };
        finish_tool_result(invoke, result)
    }

    /// Dispatch a user `/calendar` action invocation.
    pub fn dispatch_action(&mut self, invoke: ActionInvoke) -> Event {
        let result = match &self.config_state {
            ConfigState::Configured(engine) => {
                engine.dispatch_action(&invoke.action_id, &invoke.argv)
            }
            ConfigState::Unconfigured => Err("calendar module has not been configured".to_owned()),
            ConfigState::Rejected { reason } => Err(format!(
                "calendar module configuration was rejected: {reason}"
            )),
        };
        match result {
            Ok(text) => action_result(invoke, text),
            Err(message) => action_error(invoke, message),
        }
    }
}

enum BackendEvent {
    Ics(IcsEvent),
    Google(GoogleEvent),
}

struct BackendEventPage {
    events: Vec<BackendEvent>,
    next_cursor: Option<String>,
    truncated: bool,
}

enum CalendarMutationResult {
    Event(Box<GoogleEvent>),
    Deleted,
}

struct ChangeArgs {
    account: Option<String>,
    calendar: Option<String>,
    event_id: Option<String>,
    etag: Option<String>,
    title: Option<String>,
    description: Option<String>,
    location: Option<String>,
    start: Option<String>,
    end: Option<String>,
    timezone: Option<String>,
    attendees: Option<Vec<String>>,
    response: Option<String>,
}

impl From<CreateEventArgs> for ChangeArgs {
    fn from(args: CreateEventArgs) -> Self {
        Self {
            account: args.account,
            calendar: args.calendar,
            event_id: None,
            etag: None,
            title: args.title,
            description: args.description,
            location: args.location,
            start: args.start,
            end: args.end,
            timezone: args.timezone,
            attendees: args.attendees,
            response: None,
        }
    }
}

impl From<UpdateEventArgs> for ChangeArgs {
    fn from(args: UpdateEventArgs) -> Self {
        Self {
            account: args.account,
            calendar: args.calendar,
            event_id: args.event_id,
            etag: args.etag,
            title: args.title,
            description: args.description,
            location: args.location,
            start: args.start,
            end: args.end,
            timezone: args.timezone,
            attendees: args.attendees,
            response: None,
        }
    }
}

impl From<DeleteEventArgs> for ChangeArgs {
    fn from(args: DeleteEventArgs) -> Self {
        Self {
            account: args.account,
            calendar: args.calendar,
            event_id: args.event_id,
            etag: args.etag,
            title: None,
            description: None,
            location: None,
            start: None,
            end: None,
            timezone: None,
            attendees: None,
            response: None,
        }
    }
}

impl From<RespondInviteArgs> for ChangeArgs {
    fn from(args: RespondInviteArgs) -> Self {
        Self {
            account: args.account,
            calendar: args.calendar,
            event_id: args.event_id,
            etag: args.etag,
            title: None,
            description: None,
            location: None,
            start: None,
            end: None,
            timezone: None,
            attendees: None,
            response: args.response,
        }
    }
}

fn parse_invocation_args<T>(invocation: &ToolInvocation) -> Result<T, String>
where
    T: DeserializeOwned,
{
    let empty_args;
    let args = match invocation.args.as_ref() {
        Some(args) => args,
        None => {
            empty_args = CborValue::Map(Vec::new());
            &empty_args
        }
    };
    args.deserialized()
        .map_err(|error| calendar_arg_parse_error(command_name(invocation.command), &error))
}

fn calendar_arg_parse_error(command: &str, error: &dyn std::fmt::Display) -> String {
    let message = error.to_string();
    if let Some(field) = extract_unknown_field(&message) {
        return format!("{command} does not accept `{field}`");
    }
    format!("invalid {command} args: {message}")
}

fn extract_unknown_field(message: &str) -> Option<&str> {
    let (_, rest) = message.split_once("unknown field `")?;
    let (field, _) = rest.split_once('`')?;
    Some(field)
}

impl CalendarMutationResult {
    fn event_id(&self) -> Option<&str> {
        match self {
            Self::Event(event) => Some(&event.id),
            Self::Deleted => None,
        }
    }
}

impl Engine {
    fn dispatch(&self, arguments: &CborValue) -> CborValue {
        let invocation: ToolInvocation = match arguments.deserialized() {
            Ok(invocation) => invocation,
            Err(error) => {
                return error_envelope(
                    None,
                    "invalid_input",
                    &format!("invalid calendar tool arguments: {error}"),
                );
            }
        };
        let command = invocation.command;
        let result = match command {
            CalendarCommand::ListAccounts => {
                parse_invocation_args::<NoArgs>(&invocation).map(|_args| self.list_accounts())
            }
            CalendarCommand::ListCalendars => {
                parse_invocation_args::<ListCalendarsArgs>(&invocation)
                    .and_then(|args| self.list_calendars(&args))
            }
            CalendarCommand::ListEvents => parse_invocation_args::<CalendarRangeArgs>(&invocation)
                .and_then(|args| self.list_events(&args)),
            CalendarCommand::ReadEvent => parse_invocation_args::<ReadEventArgs>(&invocation)
                .and_then(|args| self.read_event(&args)),
            CalendarCommand::FreeBusy => parse_invocation_args::<CalendarRangeArgs>(&invocation)
                .and_then(|args| self.free_busy(&args)),
            CalendarCommand::CreateEvent => parse_invocation_args::<CreateEventArgs>(&invocation)
                .and_then(|args| self.submit_change(command, ChangeArgs::from(args))),
            CalendarCommand::UpdateEvent => parse_invocation_args::<UpdateEventArgs>(&invocation)
                .and_then(|args| self.submit_change(command, ChangeArgs::from(args))),
            CalendarCommand::DeleteEvent => parse_invocation_args::<DeleteEventArgs>(&invocation)
                .and_then(|args| self.submit_change(command, ChangeArgs::from(args))),
            CalendarCommand::RespondInvite => {
                parse_invocation_args::<RespondInviteArgs>(&invocation)
                    .and_then(|args| self.submit_change(command, ChangeArgs::from(args)))
            }
        }
        .unwrap_or_else(|message| calendar_error_envelope(Some(command_name(command)), &message));
        self.append_calendar_log_for_invocation(&invocation, &result);
        result
    }

    fn dispatch_action(&self, action_id: &str, argv: &[String]) -> Result<String, String> {
        match action_id {
            "calendar.auth.google.start" => {
                require_one_arg(argv).and_then(|account| self.action_auth_google_start(account))
            }
            "calendar.auth.google.finish" => {
                require_one_arg(argv).and_then(|account| self.action_auth_google_finish(account))
            }
            "calendar.log.last" => {
                parse_log_limit(argv).and_then(|limit| self.action_log_last(limit))
            }
            "calendar.change.list" => {
                require_no_args(argv).and_then(|()| self.action_change_list())
            }
            "calendar.change.open" => {
                require_one_arg(argv).and_then(|id| self.action_change_open(id))
            }
            "calendar.change.approve" => self.action_change_approve_args(argv),
            "calendar.change.deny" => {
                require_change_ids(argv).and_then(|ids| self.action_change_deny_many(&ids))
            }
            _ => Err(format!("unknown calendar action `{action_id}`")),
        }
    }

    fn action_auth_google_start(&self, account_id: &str) -> Result<String, String> {
        let account = self.google_oauth_state_account(account_id)?;
        let started = self.google.start_device_auth(account)?;
        let pending = GooglePendingAuth::new(
            &account.id,
            &started.device_code,
            &started.user_code,
            &started.verification_uri,
            started.expires_in_secs,
            started.interval_secs,
        );
        self.state.save_pending_google_auth(&pending)?;
        Ok(format!(
            "Google Calendar authorization started for account {}.\nOpen this URL:\n{}\nEnter this code:\n{}\nThen run:\n/calendar auth google finish {}\nExpires in {} second(s). If authorization is still pending, wait at least {} second(s) before retrying finish.",
            safe_display_line(&account.id),
            safe_display_line(&started.verification_uri),
            safe_display_line(&started.user_code),
            safe_display_line(&account.id),
            started.expires_in_secs,
            started.interval_secs
        ))
    }

    fn action_auth_google_finish(&self, account_id: &str) -> Result<String, String> {
        let account = self.google_oauth_state_account(account_id)?;
        let pending = self.state.pending_google_auth(&account.id)?;
        if pending.expired() {
            self.state.clear_pending_google_auth(&account.id)?;
            return Err(format!(
                "Google authorization for account `{}` expired; run `/calendar auth google start {}` again",
                safe_display_line(&account.id),
                safe_display_line(&account.id)
            ));
        }
        let finished = self
            .google
            .finish_device_auth(account, &pending.device_code)?;
        self.state
            .save_google_refresh_token(&account.id, &finished.refresh_token)?;
        self.state.clear_pending_google_auth(&account.id)?;
        if let Some(access_token) = finished.access_token
            && let Err(message) = self.google.prime_access_token_cache(
                &account.id,
                access_token,
                finished.expires_in_secs,
            )
        {
            tracing::warn!(target: crate::LOG_TARGET, error = %message, "failed to prime Google Calendar access token cache");
        }
        Ok(format!(
            "Google Calendar authorization stored for account {}.",
            safe_display_line(&account.id)
        ))
    }

    fn action_log_last(&self, limit: usize) -> Result<String, String> {
        let entries = self.state.recent_calendar_log(limit)?;
        if entries.is_empty() {
            return Ok("No calendar log entries.".to_owned());
        }
        let mut lines = vec![format!("Last {} calendar log entry(s):", entries.len())];
        for entry in entries.iter().rev() {
            lines.push(format_calendar_log_entry(entry));
        }
        Ok(lines.join("\n"))
    }

    fn action_change_list(&self) -> Result<String, String> {
        let changes = self.state.list_pending_changes()?;
        if changes.is_empty() {
            return Ok("No pending calendar changes.".to_owned());
        }
        let mut lines = vec![format!("{} pending calendar change(s):", changes.len())];
        for change in changes {
            lines.push(format_change_summary(&change));
        }
        Ok(lines.join("\n"))
    }

    fn action_change_open(&self, id: &str) -> Result<String, String> {
        let change = self.state.pending_change_by_id(id)?;
        Ok(format_change_detail(&change))
    }

    fn action_change_approve_args(&self, argv: &[String]) -> Result<String, String> {
        if require_all_arg(argv)? {
            let ids = self
                .state
                .list_pending_changes()?
                .into_iter()
                .map(|change| change.id)
                .collect::<Vec<_>>();
            if ids.is_empty() {
                return Ok("No pending calendar changes to approve.".to_owned());
            }
            return self.action_change_approve_many(&ids);
        }
        require_change_ids(argv).and_then(|ids| self.action_change_approve_many(&ids))
    }

    fn action_change_approve_many(&self, ids: &[String]) -> Result<String, String> {
        if ids.len() == 1 {
            return self.action_change_approve(&ids[0]);
        }
        let mut lines = vec![format!("Approving {} calendar change(s):", ids.len())];
        let mut errors = Vec::new();
        for id in ids {
            match self.action_change_approve(id) {
                Ok(message) => lines.push(message),
                Err(error) => {
                    lines.push(format!(
                        "Failed calendar change {id}: {}",
                        safe_display_line(&error)
                    ));
                    errors.push(id.clone());
                }
            }
        }
        let output = lines.join("\n");
        if errors.is_empty() {
            Ok(output)
        } else {
            Err(output)
        }
    }

    fn action_change_approve(&self, id: &str) -> Result<String, String> {
        if self.state.change_pending_exists(id)? {
            let pending = self.state.pending_change_by_id(id)?;
            self.validate_persisted_change(&pending)?;
            let change = self.state.claim_change(id)?;
            self.validate_persisted_change(&change)?;
            let result = match self.execute_change(&change) {
                Ok(result) => result,
                Err(error) => {
                    return match self.state.release_claimed_change(id) {
                        Ok(()) => Err(error),
                        Err(recovery_error) => Err(format!(
                            "{error}; additionally failed to restore approval to pending: {recovery_error}"
                        )),
                    };
                }
            };
            let result_event_id = result.event_id();
            return match self.state.complete_change(id, result_event_id) {
                Ok(()) => Ok(format_mutation_result("Applied", id, &change, &result)),
                Err(error) => Ok(format!(
                    "Applied calendar change {id}, but failed to record approval: {}",
                    safe_display_line(&error)
                )),
            };
        }
        if self.state.change_sending_exists(id)? {
            return Err(format!(
                "Calendar change {id} is already being applied or needs manual recovery."
            ));
        }
        let approved = self.state.approved_change_by_id(id)?;
        Ok(format!(
            "Calendar change {id} is already approved/applied. command={} account={} calendar={}",
            safe_display_line(&approved.command),
            safe_display_line(&approved.account),
            safe_display_line(&approved.calendar)
        ))
    }

    fn action_change_deny_many(&self, ids: &[String]) -> Result<String, String> {
        if ids.len() == 1 {
            return self.action_change_deny(&ids[0]);
        }
        let mut lines = vec![format!("Denying {} calendar change(s):", ids.len())];
        let mut errors = Vec::new();
        for id in ids {
            match self.action_change_deny(id) {
                Ok(message) => lines.push(message),
                Err(error) => {
                    lines.push(format!(
                        "Failed calendar change {id}: {}",
                        safe_display_line(&error)
                    ));
                    errors.push(id.clone());
                }
            }
        }
        let output = lines.join("\n");
        if errors.is_empty() {
            Ok(output)
        } else {
            Err(output)
        }
    }

    fn action_change_deny(&self, id: &str) -> Result<String, String> {
        self.state.deny_change(id)?;
        Ok(format!("Denied calendar change {id}."))
    }

    fn append_calendar_log_for_invocation(&self, invocation: &ToolInvocation, result: &CborValue) {
        let Some(entry) = self.calendar_log_entry(invocation, result) else {
            return;
        };
        if let Err(message) = self.state.append_calendar_log(&entry) {
            tracing::warn!(target: crate::LOG_TARGET, error = %message, "failed to append calendar log");
        }
    }

    fn calendar_log_entry(
        &self,
        invocation: &ToolInvocation,
        result: &CborValue,
    ) -> Option<CalendarLogEntry> {
        if invocation.command == CalendarCommand::ListAccounts {
            return None;
        }
        let args = invocation.args.as_ref();
        let status = calendar_log_status(result);
        let account = self.log_account(args);
        let mut entry = CalendarLogEntry::tool(command_name(invocation.command), &status);
        entry.account = account
            .as_deref()
            .map(|value| safe_log_value(value, MAX_LOG_FIELD_CHARS));
        entry.calendar = self
            .log_calendar(args, account.as_deref())
            .map(|value| safe_log_value(&value, MAX_LOG_FIELD_CHARS));
        entry.event_id = args
            .and_then(|args| cbor_text_field(args, "event_id"))
            .map(|value| safe_log_value(value, MAX_LOG_FIELD_CHARS));
        entry.start = args
            .and_then(|args| cbor_text_field(args, "start"))
            .map(|value| safe_log_value(value, MAX_LOG_FIELD_CHARS));
        entry.end = args
            .and_then(|args| cbor_text_field(args, "end"))
            .map(|value| safe_log_value(value, MAX_LOG_FIELD_CHARS));
        entry.limit = args.and_then(|args| cbor_u32_field(args, "limit"));
        entry.item_count = calendar_log_item_count(invocation.command, result);
        entry.reason = calendar_log_error_message(result)
            .map(|reason| safe_log_value(reason, MAX_LOG_REASON_CHARS));
        Some(entry)
    }

    fn log_account(&self, args: Option<&CborValue>) -> Option<String> {
        if let Some(account) = args.and_then(|args| cbor_text_field(args, "account")) {
            return Some(account.to_owned());
        }
        let mut accounts = self
            .config
            .account_order
            .iter()
            .filter_map(|id| self.config.accounts.get(id))
            .filter(|account| account.enable);
        let first = accounts.next()?;
        if accounts.next().is_none() {
            Some(first.id.clone())
        } else {
            None
        }
    }

    fn log_calendar(&self, args: Option<&CborValue>, account_id: Option<&str>) -> Option<String> {
        args.and_then(|args| cbor_text_field(args, "calendar"))
            .map(str::to_owned)
            .or_else(|| {
                account_id
                    .and_then(|id| self.config.accounts.get(id))
                    .and_then(default_calendar_id_for_account)
                    .map(str::to_owned)
            })
    }

    fn list_accounts(&self) -> CborValue {
        let mut rows = Vec::new();
        if self.config.enable {
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
                rows.push(format!(
                    "{} {} {} {} {} {}",
                    safe_field(&account.id),
                    "enabled",
                    safe_field(account.backend_kind()),
                    safe_field(default_calendar),
                    safe_field(timezone),
                    safe_field(display_name)
                ));
            }
        }
        ok_envelope(
            "list_accounts",
            "ok",
            cbor_map(vec![
                ("format", CborValue::Text(LIST_ACCOUNTS_FORMAT.to_owned())),
                ("accounts", line_array(rows)),
            ]),
        )
    }

    fn list_calendars(&self, args: &ListCalendarsArgs) -> Result<CborValue, String> {
        let mut rows = Vec::new();
        if self.config.enable {
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
                            rows.push(format!(
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
                        let stored_refresh_token = self.google_refresh_token(account)?;
                        for calendar in self
                            .google
                            .list_calendars(account, stored_refresh_token.as_deref())?
                        {
                            let flags = if calendar.read_only {
                                "read_only"
                            } else {
                                "writable"
                            };
                            rows.push(format!(
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
        }
        Ok(ok_envelope(
            "list_calendars",
            "ok",
            cbor_map(vec![
                ("format", CborValue::Text(LIST_CALENDARS_FORMAT.to_owned())),
                ("calendars", line_array(rows)),
            ]),
        ))
    }

    fn list_events(&self, args: &CalendarRangeArgs) -> Result<CborValue, String> {
        let limit = normalized_limit(args.limit)?;
        let account = self.single_account(args.account.as_deref())?;
        let calendar = self.calendar_arg(account, args.calendar.as_deref())?;
        let range = parse_range(args, account)?;
        let page =
            self.events_for_account(account, calendar, range, limit, args.cursor.as_deref())?;
        let mut rows = Vec::new();
        for event in &page.events {
            rows.push(format_event_line(
                &self.config.policy,
                account,
                calendar,
                event,
            ));
        }
        Ok(ok_envelope(
            "list_events",
            "ok",
            cbor_map(vec![
                ("format", CborValue::Text(LIST_EVENTS_FORMAT.to_owned())),
                ("events", line_array(rows)),
                ("next_cursor", optional_text(page.next_cursor)),
                ("truncated", CborValue::Bool(page.truncated)),
            ]),
        ))
    }

    fn read_event(&self, args: &ReadEventArgs) -> Result<CborValue, String> {
        let event_id = required_arg(args.event_id.as_deref(), "event_id")?;
        let account = self.single_account(args.account.as_deref())?;
        let calendar = self.calendar_arg(account, args.calendar.as_deref())?;
        let event = match &account.backend {
            Some(ValidatedBackendConfig::IcsFeed { .. }) => {
                BackendEvent::Ics(self.ics_feed.read_event(account, calendar, event_id)?)
            }
            Some(ValidatedBackendConfig::Google { .. }) => {
                let stored_refresh_token = self.google_refresh_token(account)?;
                BackendEvent::Google(self.google.read_event(
                    account,
                    stored_refresh_token.as_deref(),
                    calendar,
                    event_id,
                )?)
            }
            Some(ValidatedBackendConfig::Caldav { .. }) | None => {
                return Err(format!(
                    "calendar account `{}` backend `{}` does not support read_event yet",
                    account.id,
                    account.backend_kind()
                ));
            }
        };
        Ok(ok_envelope(
            "read_event",
            "ok",
            cbor_map(vec![
                ("format", CborValue::Text(EVENT_DETAIL_FORMAT.to_owned())),
                (
                    "event",
                    line_array(format_event_detail(
                        &self.config.policy,
                        account,
                        calendar,
                        &event,
                    )),
                ),
            ]),
        ))
    }

    fn free_busy(&self, args: &CalendarRangeArgs) -> Result<CborValue, String> {
        let limit = normalized_limit(args.limit)?;
        let account = self.single_account(args.account.as_deref())?;
        let calendar = self.calendar_arg(account, args.calendar.as_deref())?;
        let range = parse_range(args, account)?;
        let page =
            self.events_for_account(account, calendar, range, limit, args.cursor.as_deref())?;
        let mut rows = Vec::new();
        for event in &page.events {
            rows.push(format_free_busy_line(
                &self.config.policy,
                account,
                calendar,
                event,
            ));
        }
        Ok(ok_envelope(
            "free_busy",
            "ok",
            cbor_map(vec![
                ("format", CborValue::Text(FREE_BUSY_FORMAT.to_owned())),
                ("busy", line_array(rows)),
                ("next_cursor", optional_text(page.next_cursor)),
                ("truncated", CborValue::Bool(page.truncated)),
            ]),
        ))
    }

    fn submit_change(
        &self,
        command: CalendarCommand,
        args: ChangeArgs,
    ) -> Result<CborValue, String> {
        let change = self.build_change(command, args)?;
        self.validate_change_etag_is_current(&change)?;
        if self.config.policy.write.require_approval {
            let id = self.state.pending_change(&change)?;
            return Ok(format_change_queued(&id, &change));
        }
        let result = self.execute_change(&change)?;
        Ok(format_mutation_result_envelope("direct", &change, &result))
    }

    fn build_change(
        &self,
        command: CalendarCommand,
        args: ChangeArgs,
    ) -> Result<CalendarChangeApproval, String> {
        let account = self.single_account(args.account.as_deref())?;
        let calendar = self.calendar_arg(account, args.calendar.as_deref())?;
        self.ensure_calendar_allowed(account, calendar)?;
        if !matches!(
            &account.backend,
            Some(ValidatedBackendConfig::Google { .. })
        ) {
            return Err(format!(
                "calendar account `{}` backend `{}` does not support calendar writes",
                account.id,
                account.backend_kind()
            ));
        }
        let mut change =
            CalendarChangeApproval::pending(command_name(command), &account.id, calendar);
        match command {
            CalendarCommand::CreateEvent => {
                change.title = Some(required_text(args.title.as_deref(), "title")?);
                let (start, end) =
                    create_event_time_pair(args.start.as_deref(), args.end.as_deref())?;
                change.start = Some(start);
                change.end = Some(end);
                change.description = optional_description(args.description.as_deref())?;
                change.location = optional_line(args.location.as_deref(), "location", true)?;
                change.timezone = optional_timezone(args.timezone.as_deref())?;
                if change.timezone.is_some() && change.start.is_none() {
                    return Err("timezone updates require start and end".to_owned());
                }
                change.attendees = optional_attendees(
                    args.attendees.as_deref(),
                    self.config.policy.write.max_attendees,
                )?;
            }
            CalendarCommand::UpdateEvent => {
                change.event_id = Some(required_text(args.event_id.as_deref(), "event_id")?);
                change.etag = Some(required_text(args.etag.as_deref(), "etag")?);
                change.title = optional_line(args.title.as_deref(), "title", false)?;
                change.description = optional_description(args.description.as_deref())?;
                change.location = optional_line(args.location.as_deref(), "location", true)?;
                match (args.start.as_deref(), args.end.as_deref()) {
                    (Some(_), Some(_)) => {
                        let (start, end) =
                            required_time_pair(args.start.as_deref(), args.end.as_deref())?;
                        change.start = Some(start);
                        change.end = Some(end);
                    }
                    (None, None) => {}
                    _ => return Err("start and end must be provided together".to_owned()),
                }
                change.timezone = optional_timezone(args.timezone.as_deref())?;
                change.attendees = optional_attendees(
                    args.attendees.as_deref(),
                    self.config.policy.write.max_attendees,
                )?;
                if !change_has_update_payload(&change) {
                    return Err("update_event requires at least one field to update".to_owned());
                }
                if args.response.is_some() {
                    return Err("response is only valid for respond_invite".to_owned());
                }
            }
            CalendarCommand::DeleteEvent => {
                change.event_id = Some(required_text(args.event_id.as_deref(), "event_id")?);
                change.etag = Some(required_text(args.etag.as_deref(), "etag")?);
            }
            CalendarCommand::RespondInvite => {
                change.event_id = Some(required_text(args.event_id.as_deref(), "event_id")?);
                change.etag = Some(required_text(args.etag.as_deref(), "etag")?);
                change.response = Some(required_response(args.response.as_deref())?);
            }
            CalendarCommand::ListAccounts
            | CalendarCommand::ListCalendars
            | CalendarCommand::ListEvents
            | CalendarCommand::ReadEvent
            | CalendarCommand::FreeBusy => unreachable!("read commands are not calendar changes"),
        }
        Ok(change)
    }

    fn validate_change_etag_is_current(
        &self,
        change: &CalendarChangeApproval,
    ) -> Result<(), String> {
        if !matches!(
            change.command.as_str(),
            "update_event" | "delete_event" | "respond_invite"
        ) {
            return Ok(());
        }
        let account = self.account_by_id(&change.account)?;
        let Some(ValidatedBackendConfig::Google { .. }) = &account.backend else {
            return Ok(());
        };
        let event_id = required_change_field(change.event_id.as_deref(), "event_id")?;
        let requested_etag = required_change_field(change.etag.as_deref(), "etag")?;
        if requested_etag == "*" {
            return Err(
                "Google calendar writes require the exact current event etag; `*` is not accepted"
                    .to_owned(),
            );
        }
        let stored_refresh_token = self.google_refresh_token(account)?;
        let current = self.google.read_event(
            account,
            stored_refresh_token.as_deref(),
            &change.calendar,
            event_id,
        )?;
        let current_etag = current
            .etag
            .as_deref()
            .ok_or_else(|| "Google Calendar event response was missing etag".to_owned())?;
        if google_etag_compare_value(requested_etag) == google_etag_compare_value(current_etag) {
            return Ok(());
        }
        Err(format!(
            "stale Google Calendar etag for event `{}`: requested `{}`, current `{}`; re-read the event and retry",
            safe_display_line(event_id),
            safe_display_line(requested_etag),
            safe_display_line(current_etag)
        ))
    }

    fn validate_persisted_change(&self, change: &CalendarChangeApproval) -> Result<(), String> {
        let account = self.account_by_id(&change.account)?;
        self.ensure_calendar_allowed(account, &change.calendar)?;
        if !matches!(
            &account.backend,
            Some(ValidatedBackendConfig::Google { .. })
        ) {
            return Err(format!(
                "calendar account `{}` backend `{}` does not support calendar writes",
                account.id,
                account.backend_kind()
            ));
        }
        if let Some(attendees) = &change.attendees
            && self.config.policy.write.max_attendees < attendees.len()
        {
            return Err("calendar change has too many attendees for current policy".to_owned());
        }
        validate_change_shape(change)
    }

    fn execute_change(
        &self,
        change: &CalendarChangeApproval,
    ) -> Result<CalendarMutationResult, String> {
        let account = self.account_by_id(&change.account)?;
        self.ensure_calendar_allowed(account, &change.calendar)?;
        let stored_refresh_token = self.google_refresh_token(account)?;
        match change.command.as_str() {
            "create_event" => {
                let event = self.google.create_event(
                    account,
                    stored_refresh_token.as_deref(),
                    &change.calendar,
                    &google_write_from_change(change),
                )?;
                Ok(CalendarMutationResult::Event(Box::new(event)))
            }
            "update_event" => {
                let event_id = required_change_field(change.event_id.as_deref(), "event_id")?;
                let etag = required_change_field(change.etag.as_deref(), "etag")?;
                let event = self.google.update_event(
                    account,
                    stored_refresh_token.as_deref(),
                    &change.calendar,
                    event_id,
                    etag,
                    &google_write_from_change(change),
                )?;
                Ok(CalendarMutationResult::Event(Box::new(event)))
            }
            "delete_event" => {
                let event_id = required_change_field(change.event_id.as_deref(), "event_id")?;
                let etag = required_change_field(change.etag.as_deref(), "etag")?;
                self.google.delete_event(
                    account,
                    stored_refresh_token.as_deref(),
                    &change.calendar,
                    event_id,
                    etag,
                )?;
                Ok(CalendarMutationResult::Deleted)
            }
            "respond_invite" => {
                let event_id = required_change_field(change.event_id.as_deref(), "event_id")?;
                let etag = required_change_field(change.etag.as_deref(), "etag")?;
                let response = required_change_field(change.response.as_deref(), "response")?;
                let event = self.google.respond_invite(
                    account,
                    stored_refresh_token.as_deref(),
                    &change.calendar,
                    event_id,
                    etag,
                    response,
                )?;
                Ok(CalendarMutationResult::Event(Box::new(event)))
            }
            other => Err(format!("unsupported calendar change command `{other}`")),
        }
    }

    fn events_for_account(
        &self,
        account: &ValidatedAccount,
        calendar: &str,
        range: TimeRange,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<BackendEventPage, String> {
        match &account.backend {
            Some(ValidatedBackendConfig::IcsFeed { .. }) => {
                let page = self
                    .ics_feed
                    .list_events_page(account, calendar, range, limit, cursor)?;
                Ok(BackendEventPage {
                    events: page.events.into_iter().map(BackendEvent::Ics).collect(),
                    next_cursor: page.next_cursor,
                    truncated: page.truncated,
                })
            }
            Some(ValidatedBackendConfig::Google { .. }) => {
                let stored_refresh_token = self.google_refresh_token(account)?;
                let page = self.google.list_events_page(
                    account,
                    stored_refresh_token.as_deref(),
                    calendar,
                    range,
                    limit,
                    cursor,
                )?;
                let truncated = page.next_cursor.is_some();
                Ok(BackendEventPage {
                    events: page.events.into_iter().map(BackendEvent::Google).collect(),
                    next_cursor: page.next_cursor,
                    truncated,
                })
            }
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
        let Some(calendar) = default_calendar_id_for_account(account) else {
            return Err(format!(
                "calendar is required for account `{}` because no default calendar is configured",
                account.id
            ));
        };
        Ok(calendar)
    }

    fn ensure_calendar_allowed(
        &self,
        account: &ValidatedAccount,
        calendar: &str,
    ) -> Result<(), String> {
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

    fn google_oauth_state_account(&self, account_id: &str) -> Result<&ValidatedAccount, String> {
        let account = self.account_by_id(account_id)?;
        match &account.backend {
            Some(ValidatedBackendConfig::Google {
                refresh_token_secret: None,
                ..
            }) => Ok(account),
            Some(ValidatedBackendConfig::Google { .. }) => Err(format!(
                "calendar account `{}` already uses refresh_token_secret; remove it before using `/calendar auth google`",
                account.id
            )),
            _ => Err(format!(
                "calendar account `{}` backend `{}` is not google",
                account.id,
                account.backend_kind()
            )),
        }
    }

    fn google_refresh_token(&self, account: &ValidatedAccount) -> Result<Option<String>, String> {
        match &account.backend {
            Some(ValidatedBackendConfig::Google {
                refresh_token_secret: Some(_),
                ..
            }) => Ok(None),
            Some(ValidatedBackendConfig::Google {
                refresh_token_secret: None,
                ..
            }) => self
                .state
                .google_refresh_token(&account.id)?
                .map(Some)
                .ok_or_else(|| {
                    format!(
                        "Google calendar account `{}` is not authorized; run `/calendar auth google start {}` and then `/calendar auth google finish {}`",
                        account.id, account.id, account.id
                    )
                }),
            _ => Err(format!(
                "calendar account `{}` backend `{}` is not google",
                account.id,
                account.backend_kind()
            )),
        }
    }
}

fn default_calendar_id_for_account(account: &ValidatedAccount) -> Option<&str> {
    account.default_calendar.as_deref().or_else(|| {
        account
            .allowed_calendars
            .first()
            .map(String::as_str)
            .or(match &account.backend {
                Some(ValidatedBackendConfig::IcsFeed { .. }) => Some("main"),
                Some(ValidatedBackendConfig::Google { .. })
                | Some(ValidatedBackendConfig::Caldav { .. })
                | None => None,
            })
    })
}

fn parse_range(args: &CalendarRangeArgs, account: &ValidatedAccount) -> Result<TimeRange, String> {
    let start = parse_read_bound(
        required_arg(args.start.as_deref(), "start")?,
        "start",
        account.timezone.as_deref(),
    )?;
    let end = match args.end.as_deref() {
        Some(end) if !end.trim().is_empty() => {
            parse_read_bound(end, "end", account.timezone.as_deref())?
        }
        _ => default_read_end_bound(
            required_arg(args.start.as_deref(), "start")?,
            start,
            account.timezone.as_deref(),
        )?,
    };
    if !is_datetime_before(start, end) {
        return Err("start must be before end".to_owned());
    }
    Ok(TimeRange {
        min: Some(start),
        max: Some(end),
    })
}

fn parse_read_bound(
    value: &str,
    field: &str,
    account_timezone: Option<&str>,
) -> Result<time::OffsetDateTime, String> {
    if let Ok(value) =
        time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
    {
        return Ok(value);
    }
    let local = parse_local_read_bound(value, field)?;
    local_read_bound_to_utc(local, field, account_timezone)
}

fn default_read_end_bound(
    start_value: &str,
    start_utc: time::OffsetDateTime,
    account_timezone: Option<&str>,
) -> Result<time::OffsetDateTime, String> {
    if time::OffsetDateTime::parse(start_value, &time::format_description::well_known::Rfc3339)
        .is_ok()
    {
        return start_utc
            .checked_add(time::Duration::days(7))
            .ok_or_else(|| "default end is out of range".to_owned());
    }
    let local = parse_local_read_bound(start_value, "start")?
        .checked_add(time::Duration::days(7))
        .ok_or_else(|| "default end is out of range".to_owned())?;
    local_read_bound_to_utc(local, "end", account_timezone)
}

fn parse_local_read_bound(value: &str, field: &str) -> Result<time::PrimitiveDateTime, String> {
    if let Some(date) = parse_tool_date(value) {
        return Ok(date.with_time(time::Time::MIDNIGHT));
    }
    time::PrimitiveDateTime::parse(
        value,
        time::macros::format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]"),
    )
    .map_err(|error| {
        format!(
            "{field} must be RFC3339 with offset, YYYY-MM-DD, or local YYYY-MM-DDTHH:MM:SS: {error}"
        )
    })
}

fn local_read_bound_to_utc(
    local: time::PrimitiveDateTime,
    field: &str,
    account_timezone: Option<&str>,
) -> Result<time::OffsetDateTime, String> {
    let timezone_name = account_timezone.ok_or_else(|| {
        format!("{field} has no UTC offset and account timezone is not configured")
    })?;
    let timezone = time_tz::timezones::get_by_name(timezone_name)
        .ok_or_else(|| format!("account timezone `{timezone_name}` is not recognized"))?;
    match local.assume_timezone(timezone) {
        OffsetResult::Some(value) => Ok(value),
        OffsetResult::Ambiguous(_, _) => Err(format!(
            "{field} is ambiguous in timezone `{timezone_name}`"
        )),
        OffsetResult::None => Err(format!("{field} is invalid in timezone `{timezone_name}`")),
    }
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

fn required_text(value: Option<&str>, name: &str) -> Result<String, String> {
    let value = required_arg(value, name)?;
    validate_line(value, name, false)?;
    Ok(value.to_owned())
}

fn google_etag_compare_value(etag: &str) -> String {
    if etag.starts_with('"') || etag.starts_with("W/\"") {
        etag.to_owned()
    } else {
        format!("\"{etag}\"")
    }
}

fn required_change_field<'a>(value: Option<&'a str>, name: &str) -> Result<&'a str, String> {
    match value {
        Some(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(format!("calendar change is missing {name}")),
    }
}

fn optional_line(
    value: Option<&str>,
    name: &str,
    allow_empty: bool,
) -> Result<Option<String>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    validate_line(value, name, allow_empty)?;
    Ok(Some(value.to_owned()))
}

fn optional_description(value: Option<&str>) -> Result<Option<String>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if MAX_EVENT_DESCRIPTION_BYTES < value.len()
        || MAX_EVENT_DESCRIPTION_LINES < value.lines().count()
        || value.chars().any(|ch| {
            (ch.is_control() && ch != '\n' && ch != '\r' && ch != '\t')
                || is_unsafe_format_control(ch)
        })
    {
        return Err("description is too large or contains unsafe characters".to_owned());
    }
    Ok(Some(value.to_owned()))
}

fn optional_timezone(value: Option<&str>) -> Result<Option<String>, String> {
    optional_line(value, "timezone", false)
}

fn validate_line(value: &str, name: &str, allow_empty: bool) -> Result<(), String> {
    if (!allow_empty && value.trim().is_empty())
        || MAX_EVENT_FIELD_CHARS < value.chars().count()
        || value
            .chars()
            .any(|ch| ch.is_control() || is_unsafe_format_control(ch))
    {
        return Err(format!("{name} is too large or contains unsafe characters"));
    }
    Ok(())
}

fn optional_attendees(
    value: Option<&[String]>,
    max_attendees: usize,
) -> Result<Option<Vec<String>>, String> {
    let Some(attendees) = value else {
        return Ok(None);
    };
    if max_attendees < attendees.len() {
        return Err(format!("too many attendees; maximum is {max_attendees}"));
    }
    for attendee in attendees {
        validate_attendee(attendee)?;
    }
    Ok(Some(attendees.to_vec()))
}

fn validate_attendee(value: &str) -> Result<(), String> {
    if value.trim().is_empty()
        || MAX_ATTENDEE_CHARS < value.chars().count()
        || value.matches('@').count() != 1
        || value
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace() || is_unsafe_format_control(ch))
    {
        return Err("attendee email address is invalid".to_owned());
    }
    Ok(())
}

fn create_event_time_pair(
    start: Option<&str>,
    end: Option<&str>,
) -> Result<(String, String), String> {
    let start = required_text(start, "start")?;
    let end = match end {
        Some(end) if !end.trim().is_empty() => end.to_owned(),
        _ => default_create_event_end(&start)?,
    };
    validate_time_pair(&start, &end)?;
    Ok((start, end))
}

fn default_create_event_end(start: &str) -> Result<String, String> {
    if let Some(date) = parse_tool_date(start) {
        let end = date
            .next_day()
            .ok_or_else(|| "default event end is out of range".to_owned())?;
        return Ok(end.to_string());
    }
    let start = time::OffsetDateTime::parse(start, &time::format_description::well_known::Rfc3339)
        .map_err(|error| format!("start must be RFC3339 or YYYY-MM-DD: {error}"))?;
    let end = start
        .checked_add(time::Duration::hours(1))
        .ok_or_else(|| "default event end is out of range".to_owned())?;
    end.format(&time::format_description::well_known::Rfc3339)
        .map_err(|error| format!("default event end could not be formatted: {error}"))
}

fn required_time_pair(start: Option<&str>, end: Option<&str>) -> Result<(String, String), String> {
    let start = required_text(start, "start")?;
    let end = required_text(end, "end")?;
    validate_time_pair(&start, &end)?;
    Ok((start, end))
}

fn validate_time_pair(start: &str, end: &str) -> Result<(), String> {
    let start_date = parse_tool_date(start);
    let end_date = parse_tool_date(end);
    match (start_date, end_date) {
        (Some(start), Some(end)) => {
            if !is_date_before(start, end) {
                return Err("event start must be before event end".to_owned());
            }
            Ok(())
        }
        (None, None) => {
            let start =
                time::OffsetDateTime::parse(start, &time::format_description::well_known::Rfc3339)
                    .map_err(|error| format!("start must be RFC3339 or YYYY-MM-DD: {error}"))?;
            let end =
                time::OffsetDateTime::parse(end, &time::format_description::well_known::Rfc3339)
                    .map_err(|error| format!("end must be RFC3339 or YYYY-MM-DD: {error}"))?;
            if !is_datetime_before(start, end) {
                return Err("event start must be before event end".to_owned());
            }
            Ok(())
        }
        _ => Err(
            "event start and end must both be all-day dates or both be RFC3339 date-times"
                .to_owned(),
        ),
    }
}

fn parse_tool_date(value: &str) -> Option<time::Date> {
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || !bytes[..4].iter().all(u8::is_ascii_digit)
        || !bytes[5..7].iter().all(u8::is_ascii_digit)
        || !bytes[8..].iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    let year = value[0..4].parse::<i32>().ok()?;
    let month = time::Month::try_from(value[5..7].parse::<u8>().ok()?).ok()?;
    let day = value[8..10].parse::<u8>().ok()?;
    time::Date::from_calendar_date(year, month, day).ok()
}

fn is_date_before(left: time::Date, right: time::Date) -> bool {
    left < right
}

fn is_datetime_before(left: time::OffsetDateTime, right: time::OffsetDateTime) -> bool {
    left < right
}

fn required_response(value: Option<&str>) -> Result<String, String> {
    let response = required_arg(value, "response")?;
    if !matches!(response, "accepted" | "tentative" | "declined") {
        return Err("response must be accepted, tentative, or declined".to_owned());
    }
    Ok(response.to_owned())
}

fn change_has_update_payload(change: &CalendarChangeApproval) -> bool {
    change.title.is_some()
        || change.description.is_some()
        || change.location.is_some()
        || change.start.is_some()
        || change.end.is_some()
        || change.attendees.is_some()
}

fn validate_change_shape(change: &CalendarChangeApproval) -> Result<(), String> {
    match change.command.as_str() {
        "create_event" => {
            required_change_field(change.title.as_deref(), "title")?;
            let start = required_change_field(change.start.as_deref(), "start")?;
            let end = required_change_field(change.end.as_deref(), "end")?;
            validate_time_pair(start, end)
        }
        "update_event" => {
            required_change_field(change.event_id.as_deref(), "event_id")?;
            required_change_field(change.etag.as_deref(), "etag")?;
            if !change_has_update_payload(change) {
                return Err("update_event requires at least one field to update".to_owned());
            }
            if let (Some(start), Some(end)) = (change.start.as_deref(), change.end.as_deref()) {
                validate_time_pair(start, end)?;
            } else if change.start.is_some() || change.end.is_some() || change.timezone.is_some() {
                return Err("start and end must be provided together for time updates".to_owned());
            }
            Ok(())
        }
        "delete_event" => {
            required_change_field(change.event_id.as_deref(), "event_id")?;
            required_change_field(change.etag.as_deref(), "etag")?;
            Ok(())
        }
        "respond_invite" => {
            required_change_field(change.event_id.as_deref(), "event_id")?;
            required_change_field(change.etag.as_deref(), "etag")?;
            required_response(change.response.as_deref()).map(|_| ())
        }
        other => Err(format!("unsupported calendar change command `{other}`")),
    }
}

fn google_write_from_change(change: &CalendarChangeApproval) -> GoogleEventWrite<'_> {
    GoogleEventWrite {
        title: change.title.as_deref(),
        description: change.description.as_deref(),
        location: change.location.as_deref(),
        start: change.start.as_deref(),
        end: change.end.as_deref(),
        timezone: change.timezone.as_deref(),
        attendees: change.attendees.as_deref(),
    }
}

fn parse_log_limit(argv: &[String]) -> Result<usize, String> {
    let limit = match argv {
        [] => CALENDAR_LOG_DEFAULT_LIMIT,
        [value] if !value.trim().is_empty() => value
            .parse::<usize>()
            .map_err(|_| "log limit must be a positive integer".to_owned())?,
        [_] => return Err("log limit must not be empty".to_owned()),
        _ => return Err("too many action arguments".to_owned()),
    };
    if limit == 0 {
        return Err("log limit must be a positive integer".to_owned());
    }
    Ok(if CALENDAR_LOG_MAX_LIMIT < limit {
        CALENDAR_LOG_MAX_LIMIT
    } else {
        limit
    })
}

fn require_no_args(argv: &[String]) -> Result<(), String> {
    if argv.is_empty() {
        Ok(())
    } else {
        Err("this calendar action does not accept arguments".to_owned())
    }
}

fn require_one_arg(argv: &[String]) -> Result<&str, String> {
    match argv {
        [value] if !value.trim().is_empty() => Ok(value),
        [_] => Err("action argument must not be empty".to_owned()),
        [] => Err("missing required action argument".to_owned()),
        _ => Err("too many action arguments".to_owned()),
    }
}

fn require_all_arg(argv: &[String]) -> Result<bool, String> {
    let values = argv
        .iter()
        .flat_map(|value| value.split_whitespace())
        .collect::<Vec<_>>();
    if values == ["all"] {
        return Ok(true);
    }
    if values.contains(&"all") {
        return Err("`all` must be the only action argument".to_owned());
    }
    Ok(false)
}

fn require_change_ids(argv: &[String]) -> Result<Vec<String>, String> {
    let raw = require_one_arg(argv)?;
    let ids = raw
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if ids.is_empty() {
        return Err("missing required action argument".to_owned());
    }
    for id in &ids {
        if id.parse::<u64>().ok().filter(|value| 0 < *value).is_none()
            || !id.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(format!("invalid calendar change id `{id}`"));
        }
    }
    Ok(ids)
}

fn calendar_log_item_count(command: CalendarCommand, result: &CborValue) -> Option<u64> {
    if cbor_bool_field(result, "ok") != Some(true) {
        return None;
    }
    let data = cbor_field(result, "data")?;
    match command {
        CalendarCommand::ListCalendars => cbor_array_len(data, "calendars"),
        CalendarCommand::ListEvents => cbor_array_len(data, "events"),
        CalendarCommand::FreeBusy => cbor_array_len(data, "busy"),
        CalendarCommand::ReadEvent => Some(1),
        CalendarCommand::ListAccounts
        | CalendarCommand::CreateEvent
        | CalendarCommand::UpdateEvent
        | CalendarCommand::DeleteEvent
        | CalendarCommand::RespondInvite => None,
    }
}

fn format_calendar_log_entry(entry: &CalendarLogEntry) -> String {
    let mut fields = vec![
        format!("ts={}", entry.ts_unix_ms),
        format!("kind={}", safe_display_line(&entry.kind)),
        format!("command={}", safe_display_line(&entry.command)),
        format!("status={}", safe_display_line(&entry.status)),
    ];
    push_log_field(&mut fields, "account", entry.account.as_deref());
    push_log_field(&mut fields, "calendar", entry.calendar.as_deref());
    push_log_field(&mut fields, "event_id", entry.event_id.as_deref());
    push_log_field(&mut fields, "start", entry.start.as_deref());
    push_log_field(&mut fields, "end", entry.end.as_deref());
    if let Some(limit) = entry.limit {
        fields.push(format!("limit={limit}"));
    }
    if let Some(item_count) = entry.item_count {
        fields.push(format!("items={item_count}"));
    }
    push_log_field(&mut fields, "reason", entry.reason.as_deref());
    fields.join(" ")
}

fn push_log_field(fields: &mut Vec<String>, name: &str, value: Option<&str>) {
    if let Some(value) = value {
        fields.push(format!("{name}={}", safe_display_line(value)));
    }
}

fn format_event_line(
    policy: &ValidatedPolicy,
    account: &ValidatedAccount,
    calendar: &str,
    event: &BackendEvent,
) -> String {
    let status = event_status(event).unwrap_or("-");
    format!(
        "{} {} {} {} {} {} {} {}",
        safe_field(&account.id),
        safe_field(calendar),
        safe_field(event_id(event)),
        safe_field(event_start(event)),
        safe_field(event_end(event)),
        event_flags(policy, event),
        safe_field(status),
        safe_field(event_summary_for_policy(policy, event))
    )
}

fn format_free_busy_line(
    policy: &ValidatedPolicy,
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
        event_flags(policy, event)
    )
}

fn format_event_detail(
    policy: &ValidatedPolicy,
    account: &ValidatedAccount,
    calendar: &str,
    event: &BackendEvent,
) -> Vec<String> {
    let mut lines = vec![
        format!("account {}", safe_field(&account.id)),
        format!("calendar {}", safe_field(calendar)),
        format!("event_id {}", safe_field(event_id(event))),
        format!("start {}", safe_field(event_start(event))),
        format!("end {}", safe_field(event_end(event))),
        format!("flags {}", event_flags(policy, event)),
        format!(
            "summary {}",
            safe_field(event_summary_for_policy(policy, event))
        ),
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
    if !event_private_busy_only(policy, event) {
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
        if let Some(description) = event_description_for_policy(policy, event) {
            lines.push(format!("description {}", safe_multiline(description)));
        }
    }
    lines
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

fn event_summary_for_policy<'a>(policy: &ValidatedPolicy, event: &'a BackendEvent) -> &'a str {
    if event_private_busy_only(policy, event) {
        "(private)"
    } else {
        event_summary(event)
    }
}

fn event_description_for_policy<'a>(
    policy: &ValidatedPolicy,
    event: &'a BackendEvent,
) -> Option<&'a str> {
    if event_private_busy_only(policy, event) {
        return None;
    }
    match policy.read.descriptions {
        DescriptionPolicy::Always => match event {
            BackendEvent::Ics(event) => event.description.as_deref(),
            BackendEvent::Google(event) => event.description.as_deref(),
        },
        DescriptionPolicy::ApprovedOnly | DescriptionPolicy::Omit => None,
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

fn event_is_private(event: &BackendEvent) -> bool {
    match event {
        BackendEvent::Ics(event) => event.private,
        BackendEvent::Google(event) => event.visibility.as_deref() == Some("private"),
    }
}

fn event_private_busy_only(policy: &ValidatedPolicy, event: &BackendEvent) -> bool {
    policy.read.private_events == PrivateEventsPolicy::BusyOnly && event_is_private(event)
}

fn event_flags(policy: &ValidatedPolicy, event: &BackendEvent) -> String {
    let mut flags = vec!["read_only"];
    if event_private_busy_only(policy, event) {
        flags.push("private_busy_only");
    } else if event_is_private(event) {
        flags.push("private");
    }
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
            if event.transparency.as_deref() == Some("transparent") {
                flags.push("transparent");
            }
            if let Some(response) = &event.self_response_status
                && response == "declined"
            {
                flags.push("self_declined");
            }
        }
    }
    flags.join(",")
}

fn format_change_queued(id: &str, change: &CalendarChangeApproval) -> CborValue {
    ok_envelope(
        &change.command,
        "approval_required",
        cbor_map(vec![
            (
                "message",
                CborValue::Text("Calendar change pending approval.".to_owned()),
            ),
            ("approval_id", CborValue::Text(safe_display_line(id))),
            (
                "account",
                CborValue::Text(safe_display_line(&change.account)),
            ),
            (
                "calendar",
                CborValue::Text(safe_display_line(&change.calendar)),
            ),
        ]),
    )
}

fn format_change_summary(change: &CalendarChangeApproval) -> String {
    let title = change.title.as_deref().unwrap_or("-");
    let event_id = change.event_id.as_deref().unwrap_or("-");
    let start = change.start.as_deref().unwrap_or("-");
    format!(
        "{} command={} account={} calendar={} event_id={} start={} title={}",
        safe_display_line(&change.id),
        safe_display_line(&change.command),
        safe_display_line(&change.account),
        safe_display_line(&change.calendar),
        safe_display_line(event_id),
        safe_display_line(start),
        safe_display_line(title)
    )
}

fn format_change_detail(change: &CalendarChangeApproval) -> String {
    let mut lines = vec![
        format!("Calendar change {}", safe_display_line(&change.id)),
        format!("status: {}", safe_display_line(&change.status)),
        format!("command: {}", safe_display_line(&change.command)),
        format!("account: {}", safe_display_line(&change.account)),
        format!("calendar: {}", safe_display_line(&change.calendar)),
        format!("reason: {}", safe_display_line(&change.reason)),
    ];
    push_change_detail(&mut lines, "event_id", change.event_id.as_deref());
    push_change_detail(&mut lines, "etag", change.etag.as_deref());
    push_change_detail(&mut lines, "title", change.title.as_deref());
    push_change_detail(&mut lines, "location", change.location.as_deref());
    push_change_detail(&mut lines, "start", change.start.as_deref());
    push_change_detail(&mut lines, "end", change.end.as_deref());
    push_change_detail(&mut lines, "timezone", change.timezone.as_deref());
    if let Some(attendees) = &change.attendees {
        lines.push(format!(
            "attendees: {}",
            safe_display_line(&attendees.join(", "))
        ));
    }
    push_change_detail(&mut lines, "response", change.response.as_deref());
    if let Some(description) = &change.description {
        lines.push(format!("description:\n{}", safe_display_text(description)));
    }
    lines.join("\n")
}

fn push_change_detail(lines: &mut Vec<String>, name: &str, value: Option<&str>) {
    if let Some(value) = value {
        lines.push(format!("{name}: {}", safe_display_line(value)));
    }
}

fn format_mutation_result(
    verb: &str,
    id: &str,
    change: &CalendarChangeApproval,
    result: &CalendarMutationResult,
) -> String {
    match result {
        CalendarMutationResult::Event(event) => format!(
            "{verb} calendar change {id}. command={} account={} calendar={} event_id={} etag={}",
            safe_display_line(&change.command),
            safe_display_line(&change.account),
            safe_display_line(&change.calendar),
            safe_display_line(&event.id),
            safe_display_line(event.etag.as_deref().unwrap_or("-"))
        ),
        CalendarMutationResult::Deleted => format!(
            "{verb} calendar change {id}. command={} account={} calendar={} event_id={}",
            safe_display_line(&change.command),
            safe_display_line(&change.account),
            safe_display_line(&change.calendar),
            safe_display_line(change.event_id.as_deref().unwrap_or("-"))
        ),
    }
}

fn format_mutation_result_envelope(
    id: &str,
    change: &CalendarChangeApproval,
    result: &CalendarMutationResult,
) -> CborValue {
    let mut entries = vec![
        (
            "message",
            CborValue::Text(format!(
                "Calendar change {}.",
                mutation_result_status(&change.command, result)
            )),
        ),
        ("change_id", CborValue::Text(safe_display_line(id))),
        (
            "account",
            CborValue::Text(safe_display_line(&change.account)),
        ),
        (
            "calendar",
            CborValue::Text(safe_display_line(&change.calendar)),
        ),
    ];
    match result {
        CalendarMutationResult::Event(event) => {
            entries.push(("event_id", CborValue::Text(safe_display_line(&event.id))));
            if let Some(etag) = &event.etag {
                entries.push(("etag", CborValue::Text(safe_display_line(etag))));
            }
        }
        CalendarMutationResult::Deleted => {
            entries.push((
                "event_id",
                CborValue::Text(safe_display_line(change.event_id.as_deref().unwrap_or("-"))),
            ));
        }
    }
    ok_envelope(
        &change.command,
        mutation_result_status(&change.command, result),
        cbor_map(entries),
    )
}

fn mutation_result_status(command: &str, result: &CalendarMutationResult) -> &'static str {
    match (command, result) {
        (_, CalendarMutationResult::Deleted) => "deleted",
        ("create_event", CalendarMutationResult::Event(_)) => "created",
        ("update_event", CalendarMutationResult::Event(_)) => "updated",
        ("respond_invite", CalendarMutationResult::Event(_)) => "responded",
        _ => "applied",
    }
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

fn finish_tool_result(invoke: ToolStarted, result: CborValue) -> Event {
    if cbor_bool_field(&result, "ok") == Some(false) {
        return tool_error(invoke, result);
    }
    let display = success_display(&result);
    Event::ToolResult(ToolResult {
        call_id: invoke.call_id,
        tool_name: invoke.tool_name,
        tool_type: tau_proto::ToolType::Function,
        result,
        kind: tau_proto::ToolResultKind::Final,
        display: Some(display),
        originator: tau_proto::PromptOriginator::User,
    })
}

fn tool_error(invoke: ToolStarted, details: CborValue) -> Event {
    let message = calendar_error_message(&details);
    let display = error_display(&invoke.arguments, &details, &message);
    Event::ToolError(ToolError {
        call_id: invoke.call_id,
        tool_name: invoke.tool_name,
        tool_type: tau_proto::ToolType::Function,
        message: message.clone(),
        details: Some(details),
        display: Some(display),
        originator: tau_proto::PromptOriginator::User,
    })
}

fn action_result(invoke: ActionInvoke, text: String) -> Event {
    Event::ActionResult(ActionResult {
        invocation_id: invoke.invocation_id,
        action_id: invoke.action_id,
        output: ActionOutput::Text { text },
    })
}

fn action_error(invoke: ActionInvoke, message: String) -> Event {
    Event::ActionError(ActionError {
        invocation_id: invoke.invocation_id,
        action_id: invoke.action_id,
        message,
        details: None,
    })
}

fn ok_envelope(command: &str, status: &str, data: CborValue) -> CborValue {
    cbor_map(vec![
        ("ok", CborValue::Bool(true)),
        ("command", CborValue::Text(command.to_owned())),
        ("status", CborValue::Text(status.to_owned())),
        ("data", data),
    ])
}

fn error_envelope(command: Option<&str>, code: &str, message: &str) -> CborValue {
    cbor_map(vec![
        ("ok", CborValue::Bool(false)),
        (
            "command",
            command
                .map(|command| CborValue::Text(command.to_owned()))
                .unwrap_or(CborValue::Null),
        ),
        (
            "error",
            cbor_map(vec![
                ("code", CborValue::Text(code.to_owned())),
                ("message", CborValue::Text(safe_display_line(message))),
                ("details", cbor_map(Vec::new())),
            ]),
        ),
    ])
}

fn calendar_error_envelope(command: Option<&str>, message: &str) -> CborValue {
    error_envelope(command, calendar_error_code(message), message)
}

fn calendar_error_code(message: &str) -> &'static str {
    if (message.starts_with("Google calendar account") && message.contains("not authorized"))
        || (message.starts_with("refreshing Google access token")
            && (message.contains("invalid_grant")
                || message.contains("invalid_client")
                || message.contains("unauthorized_client")))
    {
        "auth_error"
    } else if message.starts_with("Google Calendar API")
        || message.starts_with("Google token response")
        || message.starts_with("Google create event response")
        || message.starts_with("Google update event response")
        || message.starts_with("Google invite response")
        || message.starts_with("refreshing Google access token")
        || message.starts_with("fetching iCalendar feed")
        || message.starts_with("reading iCalendar feed")
        || message.starts_with("iCalendar feed returned HTTP")
    {
        "network_error"
    } else if message.starts_with("Google calendar secret")
        || message.contains("missing url_secret")
        || message.contains("has not been configured")
    {
        "configuration_error"
    } else if message.starts_with("serializing Google Calendar request") {
        "internal_error"
    } else {
        "invalid_input"
    }
}

fn calendar_error_message(details: &CborValue) -> String {
    let message = cbor_nested_text_field(details, "error", "message")
        .unwrap_or("invalid calendar tool request");
    let Some(code) = cbor_nested_text_field(details, "error", "code") else {
        return message.to_owned();
    };
    match cbor_text_field(details, "command") {
        Some(command) => format!(
            "calendar {} failed ({code}): {message}",
            safe_display_line(command)
        ),
        None => format!("calendar failed ({code}): {message}"),
    }
}

fn calendar_log_status(result: &CborValue) -> String {
    cbor_text_field(result, "status")
        .or_else(|| cbor_nested_text_field(result, "error", "code"))
        .unwrap_or("unknown")
        .to_owned()
}

fn calendar_log_error_message(result: &CborValue) -> Option<&str> {
    if cbor_bool_field(result, "ok") == Some(false) {
        cbor_nested_text_field(result, "error", "message")
    } else {
        None
    }
}

fn success_display(result: &CborValue) -> ToolDisplay {
    let command = cbor_text_field(result, "command").unwrap_or("calendar");
    let status_text = cbor_text_field(result, "status").unwrap_or("ok");
    let data = cbor_field(result, "data");
    ToolDisplay {
        args: calendar_display_args(command, data).unwrap_or_default(),
        stats: calendar_display_stats(command, data),
        info_chips: calendar_display_info(command, data),
        status: ToolDisplayStatus::Success,
        status_text: status_text.to_owned(),
        ..Default::default()
    }
}

fn error_display(arguments: &CborValue, details: &CborValue, message: &str) -> ToolDisplay {
    let command = cbor_text_field(details, "command").unwrap_or("calendar");
    ToolDisplay {
        args: invocation_display_args(arguments).unwrap_or_else(|| safe_display_line(command)),
        status: ToolDisplayStatus::Error,
        status_text: message.to_owned(),
        ..Default::default()
    }
}

fn calendar_display_args(command: &str, _data: Option<&CborValue>) -> Option<String> {
    Some(safe_display_line(command))
}

fn calendar_display_stats(command: &str, data: Option<&CborValue>) -> ToolDisplayStats {
    let Some(data) = data else {
        return ToolDisplayStats::default();
    };
    match command {
        "list_accounts" => line_array_stats(data, "accounts"),
        "list_calendars" => line_array_stats(data, "calendars"),
        "list_events" => line_array_stats(data, "events"),
        "read_event" => line_array_stats(data, "event"),
        "free_busy" => line_array_stats(data, "busy"),
        _ => ToolDisplayStats::default(),
    }
}

fn calendar_display_info(command: &str, data: Option<&CborValue>) -> Vec<String> {
    let Some(data) = data else {
        return Vec::new();
    };
    let mut chips = Vec::new();
    match command {
        "list_accounts" => push_count_chip(&mut chips, cbor_array_len(data, "accounts"), "account"),
        "list_calendars" => {
            push_count_chip(&mut chips, cbor_array_len(data, "calendars"), "calendar")
        }
        "list_events" => push_count_chip(&mut chips, cbor_array_len(data, "events"), "event"),
        "free_busy" => push_count_chip(&mut chips, cbor_array_len(data, "busy"), "busy block"),
        _ => {}
    }
    chips
}

fn invocation_display_args(arguments: &CborValue) -> Option<String> {
    let command = cbor_text_field(arguments, "command")?;
    Some(safe_display_line(command))
}

fn line_array(rows: Vec<String>) -> CborValue {
    CborValue::Array(rows.into_iter().map(CborValue::Text).collect())
}

fn optional_text(value: Option<String>) -> CborValue {
    value.map(CborValue::Text).unwrap_or(CborValue::Null)
}

fn line_array_stats(data: &CborValue, field: &str) -> ToolDisplayStats {
    let Some(lines) = cbor_array_field(data, field) else {
        return ToolDisplayStats::default();
    };
    let line_count = lines.len() as u64;
    let byte_count = lines
        .iter()
        .filter_map(|line| match line {
            CborValue::Text(text) => Some(text.len() as u64),
            _ => None,
        })
        .sum();
    ToolDisplayStats {
        matches: Some(line_count),
        lines: (0 < line_count).then_some(line_count),
        bytes: (0 < line_count).then_some(byte_count),
    }
}

fn push_count_chip(chips: &mut Vec<String>, count: Option<u64>, singular: &str) {
    let Some(count) = count else {
        return;
    };
    let suffix = if count == 1 {
        singular.to_owned()
    } else {
        format!("{singular}s")
    };
    chips.push(format!("{count} {suffix}"));
}

fn cbor_map(entries: Vec<(&str, CborValue)>) -> CborValue {
    CborValue::Map(
        entries
            .into_iter()
            .map(|(key, value)| (CborValue::Text(key.to_owned()), value))
            .collect(),
    )
}

fn cbor_field<'a>(value: &'a CborValue, field: &str) -> Option<&'a CborValue> {
    let CborValue::Map(entries) = value else {
        return None;
    };
    entries.iter().find_map(|(key, value)| match key {
        CborValue::Text(key) if key == field => Some(value),
        _ => None,
    })
}

fn cbor_text_field<'a>(value: &'a CborValue, field: &str) -> Option<&'a str> {
    match cbor_field(value, field) {
        Some(CborValue::Text(value)) => Some(value.as_str()),
        _ => None,
    }
}

fn cbor_bool_field(value: &CborValue, field: &str) -> Option<bool> {
    match cbor_field(value, field) {
        Some(CborValue::Bool(value)) => Some(*value),
        _ => None,
    }
}

fn cbor_u32_field(value: &CborValue, field: &str) -> Option<u32> {
    match cbor_field(value, field) {
        Some(CborValue::Integer(value)) => u32::try_from(i128::from(*value)).ok(),
        _ => None,
    }
}

fn cbor_array_len(value: &CborValue, field: &str) -> Option<u64> {
    cbor_array_field(value, field).map(|values| values.len() as u64)
}

fn cbor_array_field<'a>(value: &'a CborValue, field: &str) -> Option<&'a [CborValue]> {
    match cbor_field(value, field) {
        Some(CborValue::Array(values)) => Some(values),
        _ => None,
    }
}

fn cbor_nested_text_field<'a>(value: &'a CborValue, outer: &str, inner: &str) -> Option<&'a str> {
    let nested = cbor_field(value, outer)?;
    cbor_text_field(nested, inner)
}

fn safe_field(value: &str) -> String {
    let field = value
        .chars()
        .map(|c| {
            if c.is_control() || is_unsafe_format_control(c) {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("_");
    let field = truncate_chars(&field, MAX_DISPLAY_LINE_CHARS);
    if field.is_empty() {
        "-".to_owned()
    } else {
        field
    }
}

fn safe_multiline(value: &str) -> String {
    let collapsed = value
        .chars()
        .map(|c| {
            if c == '\n' || c == '\r' || c.is_control() || is_unsafe_format_control(c) {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let collapsed = truncate_chars(&collapsed, MAX_EVENT_FIELD_CHARS);
    if collapsed.is_empty() {
        "-".to_owned()
    } else {
        collapsed
    }
}

fn safe_display_line(value: &str) -> String {
    safe_log_value(value, MAX_DISPLAY_LINE_CHARS)
}

fn safe_display_text(value: &str) -> String {
    let mut out = String::new();
    for (index, ch) in value.chars().enumerate() {
        if MAX_EVENT_FIELD_CHARS < index + 1 {
            out.push('…');
            break;
        }
        push_escaped_char(&mut out, ch, true);
    }
    out
}

fn safe_log_value(value: &str, max_chars: usize) -> String {
    let collapsed = value
        .chars()
        .map(|c| {
            if c.is_control() || is_unsafe_format_control(c) {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    truncate_chars(&collapsed, max_chars)
}

fn is_unsafe_format_control(ch: char) -> bool {
    matches!(
        ch,
        '\u{061c}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{feff}'
    )
}

fn push_escaped_char(out: &mut String, ch: char, multiline: bool) {
    match ch {
        '\n' if multiline => out.push('\n'),
        '\n' => out.push_str("\\n"),
        '\r' => out.push_str("\\r"),
        '\t' => out.push_str("\\t"),
        '\u{1b}' => out.push_str("\\e"),
        '\u{7f}' => out.push_str("\\x7f"),
        ch if (ch as u32) <= 0x1f || (0x80..=0x9f).contains(&(ch as u32)) => {
            out.push_str(&format!("\\u{{{:04x}}}", ch as u32));
        }
        ch if is_unsafe_format_control(ch) => {
            out.push_str(&format!("\\u{{{:04x}}}", ch as u32));
        }
        ch => out.push(ch),
    }
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (index, c) in value.chars().enumerate() {
        if max_chars < index + 1 {
            out.push_str("...");
            break;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::config::{
        CalendarAccountConfig, CalendarBackendConfig, CalendarSelectionConfig, ValidatedReadPolicy,
        ValidatedWritePolicy,
    };

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
                    refresh_token_secret: Some("google_refresh_token".to_owned()),
                    api_base: None,
                }),
                calendars: Default::default(),
                timezone: Some("UTC".to_owned()),
            }],
            ..Default::default()
        };
        let config = cfg.validate().expect("valid calendar config");
        let temp = tempfile::TempDir::new().expect("tempdir");
        let engine = Engine {
            config,
            state: StateStore::open(temp.path().join("state")).expect("state"),
            google: GoogleBackend::new(BTreeMap::new()),
            ics_feed: IcsFeedBackend::new(BTreeMap::new()),
        };

        let output = engine.list_accounts();
        let data = cbor_field(&output, "data").expect("data");

        assert_eq!(cbor_text_field(&output, "command"), Some("list_accounts"));
        assert_eq!(cbor_text_field(&output, "status"), Some("ok"));
        assert_eq!(cbor_text_field(data, "format"), Some(LIST_ACCOUNTS_FORMAT));
        assert_eq!(
            line_payload(data, "accounts"),
            "work enabled google - UTC Work_Calendar"
        );
    }

    #[test]
    fn calendar_log_records_tool_reads_and_action_lists_them() {
        // Calendar entries contain sensitive schedule metadata. Tool reads need
        // an audit trail that the user can review without exposing event bodies.
        let temp = tempfile::TempDir::new().expect("tempdir");
        let engine = test_engine(temp.path());

        let output = engine.dispatch(&command_args(
            "list_calendars",
            vec![("account", CborValue::Text("feed".to_owned()))],
        ));
        let data = cbor_field(&output, "data").expect("data");
        assert_eq!(cbor_text_field(data, "format"), Some(LIST_CALENDARS_FORMAT));

        let log = engine.action_log_last(10).expect("log output");

        assert!(log.contains("Last 1 calendar log entry(s):"), "{log}");
        assert!(log.contains("kind=tool"), "{log}");
        assert!(log.contains("command=list_calendars"), "{log}");
        assert!(log.contains("status=ok"), "{log}");
        assert!(log.contains("account=feed"), "{log}");
        assert!(log.contains("items=1"), "{log}");
    }

    #[test]
    fn list_events_uses_start_end_range_names_and_rejects_old_names() {
        // Range reads now use the same `start`/`end` names as event payloads,
        // parsed through a command-specific struct. The old time_min/time_max
        // names must fail instead of being accepted as a second vocabulary.
        let invocation = ToolInvocation {
            command: CalendarCommand::ListEvents,
            args: Some(cbor_map(vec![
                ("account", CborValue::Text("feed".to_owned())),
                ("calendar", CborValue::Text("main".to_owned())),
                (
                    "start",
                    CborValue::Text("2026-05-29T00:00:00-07:00".to_owned()),
                ),
                (
                    "end",
                    CborValue::Text("2026-05-30T00:00:00-07:00".to_owned()),
                ),
            ])),
        };
        let args = parse_invocation_args::<CalendarRangeArgs>(&invocation).expect("range args");
        assert_eq!(args.start.as_deref(), Some("2026-05-29T00:00:00-07:00"));
        assert_eq!(args.end.as_deref(), Some("2026-05-30T00:00:00-07:00"));

        let temp = tempfile::TempDir::new().expect("tempdir");
        let engine = test_engine(temp.path());
        let output = engine.dispatch(&command_args(
            "list_events",
            vec![
                ("account", CborValue::Text("feed".to_owned())),
                ("calendar", CborValue::Text("main".to_owned())),
                (
                    "time_min",
                    CborValue::Text("2026-05-29T00:00:00Z".to_owned()),
                ),
            ],
        ));

        assert_eq!(cbor_bool_field(&output, "ok"), Some(false));
        assert_eq!(cbor_text_field(&output, "command"), Some("list_events"));
        let message = cbor_nested_text_field(&output, "error", "message").expect("message");
        assert_eq!(message, "list_events does not accept `time_min`");
    }

    #[test]
    fn free_busy_rejects_event_payload_fields_instead_of_ignoring_them() {
        // free_busy has a command-specific range args struct, so payload fields
        // like title fail during serde parsing instead of being silently ignored.
        let temp = tempfile::TempDir::new().expect("tempdir");
        let engine = test_engine(temp.path());

        let output = engine.dispatch(&command_args(
            "free_busy",
            vec![
                ("account", CborValue::Text("feed".to_owned())),
                ("calendar", CborValue::Text("main".to_owned())),
                ("title", CborValue::Text("tau test party".to_owned())),
            ],
        ));

        assert_eq!(cbor_bool_field(&output, "ok"), Some(false));
        assert_eq!(cbor_text_field(&output, "command"), Some("free_busy"));
        let message = cbor_nested_text_field(&output, "error", "message").expect("message");
        assert_eq!(message, "free_busy does not accept `title`");
    }

    #[test]
    fn calendar_range_args_accept_local_bounds_and_default_end() {
        // Agents often know the date but omit an offset. Range reads should
        // interpret local date/date-time values in the account timezone and
        // stay bounded even when `end` is omitted.
        let temp = tempfile::TempDir::new().expect("tempdir");
        let engine = test_engine(temp.path());
        let account = engine.config.accounts.get("feed").expect("account");

        let range = parse_range(
            &CalendarRangeArgs {
                start: Some("2026-05-30T12:34:56".to_owned()),
                ..Default::default()
            },
            account,
        )
        .expect("local datetime range");
        assert_eq!(
            range
                .min
                .expect("min")
                .format(&time::format_description::well_known::Rfc3339)
                .expect("format min"),
            "2026-05-30T12:34:56Z"
        );
        assert_eq!(
            range
                .max
                .expect("max")
                .format(&time::format_description::well_known::Rfc3339)
                .expect("format max"),
            "2026-06-06T12:34:56Z"
        );

        let range = parse_range(
            &CalendarRangeArgs {
                start: Some("2026-05-30".to_owned()),
                end: Some("2026-05-31".to_owned()),
                ..Default::default()
            },
            account,
        )
        .expect("local date range");
        assert_eq!(
            range
                .min
                .expect("min")
                .format(&time::format_description::well_known::Rfc3339)
                .expect("format min"),
            "2026-05-30T00:00:00Z"
        );
        assert_eq!(
            range
                .max
                .expect("max")
                .format(&time::format_description::well_known::Rfc3339)
                .expect("format max"),
            "2026-05-31T00:00:00Z"
        );

        let la_start =
            parse_read_bound("2026-05-30T00:00:00", "start", Some("America/Los_Angeles"))
                .expect("la local start");
        assert_eq!(
            la_start
                .format(&time::format_description::well_known::Rfc3339)
                .expect("format la start"),
            "2026-05-30T00:00:00-07:00"
        );

        let la_fall_start =
            parse_read_bound("2026-10-31T00:00:00", "start", Some("America/Los_Angeles"))
                .expect("la fall start");
        let la_fall_end = default_read_end_bound(
            "2026-10-31T00:00:00",
            la_fall_start,
            Some("America/Los_Angeles"),
        )
        .expect("la fall default end");
        assert_eq!(
            la_fall_end
                .format(&time::format_description::well_known::Rfc3339)
                .expect("format la fall end"),
            "2026-11-07T00:00:00-08:00"
        );
    }

    #[test]
    fn calendar_range_args_require_start() {
        // Missing start used to create unbounded reads. Calendar reads should
        // now always have an explicit lower bound.
        let temp = tempfile::TempDir::new().expect("tempdir");
        let engine = test_engine(temp.path());
        let account = engine.config.accounts.get("feed").expect("account");

        let err = parse_range(&CalendarRangeArgs::default(), account).expect_err("missing start");
        assert_eq!(err, "start is required");
    }

    #[test]
    fn calendar_log_records_failed_write_attempts_without_payloads() {
        // Write commands are still unsupported, but attempts should be visible
        // in the audit log before mutation approval plumbing is added.
        let temp = tempfile::TempDir::new().expect("tempdir");
        let engine = test_engine(temp.path());

        let err = engine.dispatch(&command_args(
            "create_event",
            vec![
                ("account", CborValue::Text("feed".to_owned())),
                ("calendar", CborValue::Text("main".to_owned())),
                ("title", CborValue::Text("private title".to_owned())),
            ],
        ));
        assert_eq!(cbor_bool_field(&err, "ok"), Some(false));
        let err_text = calendar_error_message(&err);
        assert!(
            err_text.contains("does not support calendar writes"),
            "{err_text}"
        );

        let log = engine.action_log_last(10).expect("log output");

        assert!(log.contains("command=create_event"), "{log}");
        assert!(log.contains("status=invalid_input"), "{log}");
        assert!(log.contains("account=feed"), "{log}");
        assert!(log.contains("calendar=main"), "{log}");
        assert!(!log.contains("private title"), "{log}");
    }

    #[test]
    fn calendar_approve_all_accepts_empty_pending_list() {
        // `/calendar change approve all` should be a valid convenience command
        // even when there is nothing queued.
        let temp = tempfile::TempDir::new().expect("tempdir");
        let engine = test_engine(temp.path());

        let output = engine
            .action_change_approve_args(&["all".to_owned()])
            .expect("approve all");

        assert_eq!(output, "No pending calendar changes to approve.");
    }

    #[test]
    fn google_writes_queue_pending_calendar_changes() {
        // Calendar writes can send attendee notifications or alter the user's
        // schedule, so the default policy persists a pending change for review
        // instead of calling Google immediately.
        let temp = tempfile::TempDir::new().expect("tempdir");
        let cfg = CalendarExtensionConfig {
            enable: true,
            accounts: vec![CalendarAccountConfig {
                id: "google".to_owned(),
                enable: true,
                backend: Some(CalendarBackendConfig::Google {
                    client_id_secret: "client".to_owned(),
                    client_secret_secret: None,
                    refresh_token_secret: Some("refresh".to_owned()),
                    api_base: None,
                }),
                calendars: CalendarSelectionConfig {
                    default: Some("primary".to_owned()),
                    allow: vec!["primary".to_owned()],
                },
                ..Default::default()
            }],
            ..Default::default()
        };
        let engine = Engine {
            config: cfg.validate().expect("valid config"),
            state: StateStore::open(temp.path().join("state")).expect("state"),
            google: GoogleBackend::new(BTreeMap::new()),
            ics_feed: IcsFeedBackend::new(BTreeMap::new()),
        };

        let output = engine.dispatch(&command_args(
            "create_event",
            vec![
                ("account", CborValue::Text("google".to_owned())),
                ("calendar", CborValue::Text("primary".to_owned())),
                ("title", CborValue::Text("Team Sync".to_owned())),
                ("start", CborValue::Text("2026-05-28T12:00:00Z".to_owned())),
                ("end", CborValue::Text("2026-05-28T13:00:00Z".to_owned())),
                (
                    "attendees",
                    CborValue::Array(vec![CborValue::Text("a@example.com".to_owned())]),
                ),
            ],
        ));
        let data = cbor_field(&output, "data").expect("data");

        assert_eq!(
            cbor_text_field(&output, "status"),
            Some("approval_required")
        );
        assert_eq!(cbor_text_field(data, "approval_id"), Some("1"));
        let list = engine.action_change_list().expect("change list");
        assert!(list.contains("command=create_event"), "{list}");
        assert!(list.contains("title=Team Sync"), "{list}");
        let open = engine.action_change_open("1").expect("change open");
        assert!(open.contains("attendees: a@example.com"), "{open}");
        assert_eq!(
            engine.action_change_deny("1"),
            Ok("Denied calendar change 1.".to_owned())
        );
    }

    #[test]
    fn google_etag_compare_value_accepts_agent_stripped_quotes() {
        // The request-time freshness check compares Google API ETags with the
        // common agent-written form where the wire quotes were stripped.
        assert_eq!(
            google_etag_compare_value("3560073119029470"),
            google_etag_compare_value("\"3560073119029470\"")
        );
        assert_ne!(
            google_etag_compare_value("3560073119029470"),
            google_etag_compare_value("\"3560073119029471\"")
        );
    }

    #[test]
    fn create_event_defaults_missing_end() {
        // Small local models often omit `end` even when they identified a
        // concrete start. Queueing a safe default prevents an avoidable retry
        // loop while keeping the pending change visible for user approval.
        let (start, end) = create_event_time_pair(Some("2026-05-28T12:00:00Z"), None)
            .expect("default date-time end");
        assert_eq!(start, "2026-05-28T12:00:00Z");
        assert_eq!(end, "2026-05-28T13:00:00Z");

        let (start, end) =
            create_event_time_pair(Some("2026-05-28"), None).expect("default all-day end");
        assert_eq!(start, "2026-05-28");
        assert_eq!(end, "2026-05-29");
    }

    #[test]
    fn google_create_event_queues_pending_change_with_default_end() {
        // Calendar writes are still queued for approval; this only fills in a
        // low-risk default duration when the model omits `end`.
        let temp = tempfile::TempDir::new().expect("tempdir");
        let cfg = CalendarExtensionConfig {
            enable: true,
            accounts: vec![CalendarAccountConfig {
                id: "google".to_owned(),
                enable: true,
                backend: Some(CalendarBackendConfig::Google {
                    client_id_secret: "client".to_owned(),
                    client_secret_secret: None,
                    refresh_token_secret: Some("refresh".to_owned()),
                    api_base: None,
                }),
                calendars: CalendarSelectionConfig {
                    default: Some("primary".to_owned()),
                    allow: vec!["primary".to_owned()],
                },
                ..Default::default()
            }],
            ..Default::default()
        };
        let engine = Engine {
            config: cfg.validate().expect("valid config"),
            state: StateStore::open(temp.path().join("state")).expect("state"),
            google: GoogleBackend::new(BTreeMap::new()),
            ics_feed: IcsFeedBackend::new(BTreeMap::new()),
        };

        let output = engine.dispatch(&command_args(
            "create_event",
            vec![
                ("title", CborValue::Text("Team Sync".to_owned())),
                ("start", CborValue::Text("2026-05-28T12:00:00Z".to_owned())),
            ],
        ));
        let data = cbor_field(&output, "data").expect("data");

        assert_eq!(
            cbor_text_field(&output, "status"),
            Some("approval_required")
        );
        assert_eq!(cbor_text_field(data, "approval_id"), Some("1"));
        let open = engine.action_change_open("1").expect("change open");
        assert!(open.contains("start: 2026-05-28T12:00:00Z"), "{open}");
        assert!(open.contains("end: 2026-05-28T13:00:00Z"), "{open}");
    }

    #[test]
    fn google_reads_without_stored_auth_report_auth_error() {
        // Accounts that opt into action-owned OAuth should fail before any
        // network call until `/calendar auth google` stores a refresh token.
        let temp = tempfile::TempDir::new().expect("tempdir");
        let cfg = CalendarExtensionConfig {
            enable: true,
            accounts: vec![CalendarAccountConfig {
                id: "google".to_owned(),
                enable: true,
                backend: Some(CalendarBackendConfig::Google {
                    client_id_secret: "client".to_owned(),
                    client_secret_secret: None,
                    refresh_token_secret: None,
                    api_base: None,
                }),
                calendars: CalendarSelectionConfig {
                    default: Some("primary".to_owned()),
                    allow: vec!["primary".to_owned()],
                },
                ..Default::default()
            }],
            ..Default::default()
        };
        let engine = Engine {
            config: cfg.validate().expect("valid config"),
            state: StateStore::open(temp.path().join("state")).expect("state"),
            google: GoogleBackend::new(BTreeMap::new()),
            ics_feed: IcsFeedBackend::new(BTreeMap::new()),
        };

        let output = engine.dispatch(&command_args(
            "list_calendars",
            vec![("account", CborValue::Text("google".to_owned()))],
        ));

        assert_eq!(cbor_bool_field(&output, "ok"), Some(false));
        assert_eq!(
            cbor_nested_text_field(&output, "error", "code"),
            Some("auth_error")
        );
        assert!(
            calendar_error_message(&output).contains("/calendar auth google start google"),
            "{}",
            calendar_error_message(&output)
        );
    }

    #[test]
    fn private_event_details_are_busy_only_by_default() {
        // Provider-private events should not leak summaries or descriptions to
        // the model unless policy explicitly opts into details.
        let account = ValidatedAccount {
            id: "google".to_owned(),
            enable: true,
            display_name: None,
            backend: Some(ValidatedBackendConfig::Google {
                client_id_secret: "client".to_owned(),
                client_secret_secret: None,
                refresh_token_secret: Some("refresh".to_owned()),
                api_base: None,
            }),
            default_calendar: Some("primary".to_owned()),
            allowed_calendars: vec!["primary".to_owned()],
            timezone: Some("UTC".to_owned()),
        };
        let event = BackendEvent::Google(GoogleEvent {
            id: "evt".to_owned(),
            etag: Some("abc".to_owned()),
            i_cal_uid: None,
            summary: "Private title".to_owned(),
            description: Some("private body".to_owned()),
            location: Some("Secret room".to_owned()),
            start: "2026-05-28T12:00:00Z".to_owned(),
            end: "2026-05-28T13:00:00Z".to_owned(),
            status: Some("confirmed".to_owned()),
            visibility: Some("private".to_owned()),
            transparency: None,
            organizer: Some("org@example.com".to_owned()),
            attendees: vec!["a@example.com".to_owned()],
            self_response_status: None,
            recurring: false,
        });
        let policy = ValidatedPolicy {
            read: ValidatedReadPolicy {
                private_events: PrivateEventsPolicy::BusyOnly,
                descriptions: DescriptionPolicy::ApprovedOnly,
            },
            write: ValidatedWritePolicy {
                require_approval: true,
                max_attendees: 50,
            },
        };

        let detail = format_event_detail(&policy, &account, "primary", &event).join("\n");

        assert!(detail.contains("summary (private)"), "{detail}");
        assert!(
            detail.contains("flags read_only,private_busy_only"),
            "{detail}"
        );
        assert!(!detail.contains("Private title"), "{detail}");
        assert!(!detail.contains("private body"), "{detail}");
        assert!(!detail.contains("Secret room"), "{detail}");
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
                refresh_token_secret: Some("refresh".to_owned()),
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
            visibility: None,
            transparency: None,
            organizer: Some("org@example.com".to_owned()),
            attendees: vec!["a@example.com".to_owned(), "b@example.com".to_owned()],
            self_response_status: None,
            recurring: true,
        });
        let policy = ValidatedPolicy {
            read: ValidatedReadPolicy {
                private_events: PrivateEventsPolicy::BusyOnly,
                descriptions: DescriptionPolicy::Always,
            },
            write: ValidatedWritePolicy {
                require_approval: true,
                max_attendees: 50,
            },
        };

        assert_eq!(
            format_event_detail(&policy, &account, "primary", &event).join("\n"),
            "account google\ncalendar primary\nevent_id evt\nstart 2026-05-28T12:00:00Z\nend 2026-05-28T13:00:00Z\nflags read_only,recurring\nsummary Team_Sync\nuid uid@example.com\netag abc\nstatus confirmed\nlocation Room_1\norganizer org@example.com\nattendees a@example.com,b@example.com\ndescription line 1 line 2"
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
            ..Default::default()
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
            ..Default::default()
        };

        let err = match cfg.validate() {
            Ok(_) => panic!("missing feed source should fail"),
            Err(err) => err,
        };
        assert!(err.contains("requires exactly one"), "{err}");
    }

    fn test_engine(root: &std::path::Path) -> Engine {
        let cfg = CalendarExtensionConfig {
            enable: true,
            accounts: vec![CalendarAccountConfig {
                id: "feed".to_owned(),
                enable: true,
                display_name: Some("Feed".to_owned()),
                backend: Some(CalendarBackendConfig::IcsFeed {
                    url_secret: None,
                    url: Some("https://example.test/calendar.ics".to_owned()),
                }),
                calendars: CalendarSelectionConfig {
                    default: Some("main".to_owned()),
                    allow: vec!["main".to_owned()],
                },
                timezone: Some("UTC".to_owned()),
            }],
            ..Default::default()
        };
        Engine {
            config: cfg.validate().expect("valid config"),
            state: StateStore::open(root.join("state")).expect("state"),
            google: GoogleBackend::new(BTreeMap::new()),
            ics_feed: IcsFeedBackend::new(BTreeMap::new()),
        }
    }

    fn command_args(command: &str, args: Vec<(&str, CborValue)>) -> CborValue {
        cbor_map(vec![
            ("command", CborValue::Text(command.to_owned())),
            ("args", cbor_map(args)),
        ])
    }

    fn cbor_map(entries: Vec<(&str, CborValue)>) -> CborValue {
        CborValue::Map(
            entries
                .into_iter()
                .map(|(key, value)| (CborValue::Text(key.to_owned()), value))
                .collect(),
        )
    }

    fn line_payload(data: &CborValue, field: &str) -> String {
        cbor_array_field(data, field)
            .expect("line array")
            .iter()
            .map(|value| match value {
                CborValue::Text(value) => value.as_str(),
                _ => panic!("line array contains non-text value"),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
