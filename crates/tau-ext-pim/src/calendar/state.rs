use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const LOG_SCHEMA: u32 = 1;
const CHANGE_SCHEMA: u32 = 1;
const CHANGE_KIND: &str = "calendar_change";
const CHANGE_APPROVAL_KIND: &str = "calendar-change";
const MAX_CHANGE_FIELD_CHARS: usize = 512;
const MAX_DESCRIPTION_BYTES: usize = 64 * 1024;
const MAX_DESCRIPTION_LINES: usize = 1000;
const MAX_ATTENDEES_HARD: usize = 200;
const GOOGLE_AUTH_SCHEMA: u32 = 1;
const GOOGLE_AUTH_PENDING_SCHEMA: u32 = 1;
const MAX_GOOGLE_AUTH_FIELD_CHARS: usize = 4096;

/// One persisted calendar audit log entry.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct CalendarLogEntry {
    /// Log schema version.
    pub(crate) schema: u32,
    /// Wall-clock timestamp in milliseconds since the Unix epoch.
    pub(crate) ts_unix_ms: u64,
    /// Log entry kind, currently `tool`.
    pub(crate) kind: String,
    /// Calendar tool command name.
    pub(crate) command: String,
    /// Command outcome status.
    pub(crate) status: String,
    /// Configured account id when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) account: Option<String>,
    /// Calendar id when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) calendar: Option<String>,
    /// Event id when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) event_id: Option<String>,
    /// Lower RFC3339 time bound from the tool call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) time_min: Option<String>,
    /// Upper RFC3339 time bound from the tool call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) time_max: Option<String>,
    /// Requested row limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) limit: Option<u32>,
    /// Number of rows or records returned when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) item_count: Option<u64>,
    /// Sanitized error reason for failed commands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
}

impl CalendarLogEntry {
    /// Build a schema-v1 tool audit entry.
    pub(crate) fn tool(command: &str, status: &str) -> Self {
        Self {
            schema: LOG_SCHEMA,
            ts_unix_ms: current_unix_millis(),
            kind: "tool".to_owned(),
            command: command.to_owned(),
            status: status.to_owned(),
            account: None,
            calendar: None,
            event_id: None,
            time_min: None,
            time_max: None,
            limit: None,
            item_count: None,
            reason: None,
        }
    }
}

/// One persisted calendar mutation approval.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct CalendarChangeApproval {
    /// Approval schema version.
    pub(crate) schema: u32,
    /// Opaque stable approval ID.
    pub(crate) id: String,
    /// Approval kind.
    pub(crate) kind: String,
    /// Approval status: pending, sending, approved, or denied.
    pub(crate) status: String,
    /// Calendar tool command name.
    pub(crate) command: String,
    /// Configured account id.
    pub(crate) account: String,
    /// Calendar id.
    pub(crate) calendar: String,
    /// Target event id for update, delete, and invite response commands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) event_id: Option<String>,
    /// Provider ETag used for stale-write protection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) etag: Option<String>,
    /// Event title for create/update commands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    /// Event description for create/update commands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    /// Event location for create/update commands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) location: Option<String>,
    /// Event start as RFC3339 date-time or all-day date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) start: Option<String>,
    /// Event end as RFC3339 date-time or all-day exclusive date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) end: Option<String>,
    /// IANA timezone for date-time values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) timezone: Option<String>,
    /// Attendee email addresses. `None` means leave existing attendees
    /// unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) attendees: Option<Vec<String>>,
    /// Invitation response for respond_invite commands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) response: Option<String>,
    /// Reason this change requires approval.
    pub(crate) reason: String,
    /// Provider event id after a successful approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) result_event_id: Option<String>,
}

impl CalendarChangeApproval {
    /// Build a pending schema-v1 calendar change approval.
    pub(crate) fn pending(command: &str, account: &str, calendar: &str) -> Self {
        Self {
            schema: CHANGE_SCHEMA,
            id: String::new(),
            kind: CHANGE_KIND.to_owned(),
            status: "pending".to_owned(),
            command: command.to_owned(),
            account: account.to_owned(),
            calendar: calendar.to_owned(),
            event_id: None,
            etag: None,
            title: None,
            description: None,
            location: None,
            start: None,
            end: None,
            timezone: None,
            attendees: None,
            response: None,
            reason: "user_approval_required".to_owned(),
            result_event_id: None,
        }
    }
}

/// Stored OAuth authorization for one Google calendar account.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct GoogleStoredAuth {
    /// Stored auth schema version.
    pub(crate) schema: u32,
    /// Configured account id.
    pub(crate) account: String,
    /// Google OAuth refresh token.
    pub(crate) refresh_token: String,
}

impl GoogleStoredAuth {
    /// Build a stored OAuth auth record.
    pub(crate) fn new(account: &str, refresh_token: &str) -> Self {
        Self {
            schema: GOOGLE_AUTH_SCHEMA,
            account: account.to_owned(),
            refresh_token: refresh_token.to_owned(),
        }
    }
}

/// Pending Google device authorization request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct GooglePendingAuth {
    /// Pending auth schema version.
    pub(crate) schema: u32,
    /// Configured account id.
    pub(crate) account: String,
    /// Google OAuth device code. This is never shown back to the user.
    pub(crate) device_code: String,
    /// User-facing code entered on Google's verification page.
    pub(crate) user_code: String,
    /// Google verification URL for the user to open.
    pub(crate) verification_uri: String,
    /// Unix timestamp in milliseconds when this request expires.
    pub(crate) expires_at_unix_ms: u64,
    /// Suggested polling interval from Google.
    pub(crate) interval_secs: u64,
}

impl GooglePendingAuth {
    /// Build a pending Google device authorization record.
    pub(crate) fn new(
        account: &str,
        device_code: &str,
        user_code: &str,
        verification_uri: &str,
        expires_in_secs: u64,
        interval_secs: u64,
    ) -> Self {
        Self {
            schema: GOOGLE_AUTH_PENDING_SCHEMA,
            account: account.to_owned(),
            device_code: device_code.to_owned(),
            user_code: user_code.to_owned(),
            verification_uri: verification_uri.to_owned(),
            expires_at_unix_ms: current_unix_millis()
                .saturating_add(expires_in_secs.saturating_mul(1000)),
            interval_secs,
        }
    }

    /// Return true when this pending authorization has expired.
    pub(crate) fn expired(&self) -> bool {
        self.expires_at_unix_ms <= current_unix_millis()
    }
}

/// Calendar module persistent state under the extension state directory.
pub(crate) struct StateStore {
    state_dir: PathBuf,
}

impl StateStore {
    /// Open the calendar state area and create required private directories.
    pub(crate) fn open(state_dir: PathBuf) -> Result<Self, String> {
        create_private_dir_all(&state_dir)?;
        create_private_dir_all(&state_dir.join("logs"))?;
        create_private_dir_all(&state_dir.join("auth").join("google"))?;
        create_private_dir_all(&state_dir.join("auth").join("google-pending"))?;
        for status in ["pending", "sending", "approved", "denied"] {
            create_private_dir_all(
                &state_dir
                    .join("approvals")
                    .join(CHANGE_APPROVAL_KIND)
                    .join(status),
            )?;
        }
        Ok(Self { state_dir })
    }

    /// Append one calendar audit log entry.
    pub(crate) fn append_calendar_log(&self, entry: &CalendarLogEntry) -> Result<(), String> {
        let path = self.calendar_log_path();
        let mut bytes = serde_json::to_vec(entry).map_err(|error| error.to_string())?;
        bytes.push(b'\n');
        let mut file = open_private_append(&path)
            .map_err(|error| format!("failed to open {}: {error}", path.display()))?;
        file.write_all(&bytes)
            .map_err(|error| format!("failed to append {}: {error}", path.display()))?;
        file.sync_data()
            .map_err(|error| format!("failed to sync {}: {error}", path.display()))
    }

    /// Load recent calendar audit log entries in chronological order.
    pub(crate) fn recent_calendar_log(
        &self,
        limit: usize,
    ) -> Result<Vec<CalendarLogEntry>, String> {
        let path = self.calendar_log_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let bytes = read_sensitive_file(&path)
            .map_err(|error| format!("failed to open {}: {error}", path.display()))?;
        let mut entries = VecDeque::new();
        for line in BufReader::new(bytes.as_slice()).lines() {
            let line =
                line.map_err(|error| format!("failed to read {}: {error}", path.display()))?;
            if line.trim().is_empty() {
                continue;
            }
            let Ok(entry) = serde_json::from_str::<CalendarLogEntry>(&line) else {
                continue;
            };
            if entry.schema != LOG_SCHEMA {
                continue;
            }
            entries.push_back(entry);
            while limit < entries.len() {
                entries.pop_front();
            }
        }
        Ok(entries.into_iter().collect())
    }

    /// Store a Google OAuth refresh token for one account.
    pub(crate) fn save_google_refresh_token(
        &self,
        account: &str,
        refresh_token: &str,
    ) -> Result<(), String> {
        let auth = GoogleStoredAuth::new(account, refresh_token);
        validate_google_stored_auth(&auth, Some(account))?;
        atomic_json_replace(&self.google_auth_path(account), &auth)
    }

    /// Load a stored Google OAuth refresh token for one account.
    pub(crate) fn google_refresh_token(&self, account: &str) -> Result<Option<String>, String> {
        let path = self.google_auth_path(account);
        if !path.exists() {
            return Ok(None);
        }
        let auth: GoogleStoredAuth = serde_json::from_slice(&read_sensitive_file(&path)?)
            .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
        validate_google_stored_auth(&auth, Some(account))?;
        Ok(Some(auth.refresh_token))
    }

    /// Store a pending Google device authorization request.
    pub(crate) fn save_pending_google_auth(
        &self,
        pending: &GooglePendingAuth,
    ) -> Result<(), String> {
        validate_google_pending_auth(pending, Some(&pending.account))?;
        atomic_json_replace(&self.google_pending_auth_path(&pending.account), pending)
    }

    /// Load a pending Google device authorization request.
    pub(crate) fn pending_google_auth(&self, account: &str) -> Result<GooglePendingAuth, String> {
        let path = self.google_pending_auth_path(account);
        if !path.exists() {
            return Err(format!(
                "no pending Google authorization for account `{account}`"
            ));
        }
        let pending: GooglePendingAuth = serde_json::from_slice(&read_sensitive_file(&path)?)
            .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
        validate_google_pending_auth(&pending, Some(account))?;
        Ok(pending)
    }

    /// Clear a pending Google device authorization request.
    pub(crate) fn clear_pending_google_auth(&self, account: &str) -> Result<(), String> {
        match fs::remove_file(self.google_pending_auth_path(account)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    /// Load pending calendar changes in deterministic order.
    pub(crate) fn list_pending_changes(&self) -> Result<Vec<CalendarChangeApproval>, String> {
        self.list_change_approvals("pending")
    }

    /// Load one pending calendar change by id.
    pub(crate) fn pending_change_by_id(&self, id: &str) -> Result<CalendarChangeApproval, String> {
        self.load_change_approval("pending", id)
    }

    /// Load one approved calendar change by id.
    pub(crate) fn approved_change_by_id(&self, id: &str) -> Result<CalendarChangeApproval, String> {
        self.load_change_approval("approved", id)
    }

    /// Return an existing pending calendar change approval or create it.
    pub(crate) fn pending_change(
        &self,
        request: &CalendarChangeApproval,
    ) -> Result<String, String> {
        let mut validated_request = request.clone();
        if validated_request.id.is_empty() {
            validated_request.id = "1".to_owned();
        }
        validate_calendar_change_approval(&validated_request, "pending", None)?;
        loop {
            for approval in self.list_pending_changes()? {
                if calendar_change_matches(&approval, request) {
                    return Ok(approval.id);
                }
            }
            let mut request = request.clone();
            request.id = self.next_change_id()?;
            let path = self.change_path("pending", &request.id)?;
            match atomic_json_create_new(&path, &request) {
                Ok(()) => return Ok(request.id),
                Err(CreateNewJsonError::AlreadyExists) => continue,
                Err(CreateNewJsonError::Other(message)) => return Err(message),
            }
        }
    }

    /// Return true if a pending calendar change exists.
    pub(crate) fn change_pending_exists(&self, id: &str) -> Result<bool, String> {
        Ok(self.change_path("pending", id)?.exists())
    }

    /// Return true if a sending calendar change exists.
    pub(crate) fn change_sending_exists(&self, id: &str) -> Result<bool, String> {
        Ok(self.change_path("sending", id)?.exists())
    }

    /// Claim one pending calendar change for execution.
    pub(crate) fn claim_change(&self, id: &str) -> Result<CalendarChangeApproval, String> {
        let approval = self.pending_change_by_id(id)?;
        let mut sending = approval.clone();
        sending.status = "sending".to_owned();
        let sending_path = self.change_path("sending", id)?;
        match atomic_json_create_new(&sending_path, &sending) {
            Ok(()) => {}
            Err(CreateNewJsonError::AlreadyExists) => {
                return Err(format!("calendar change `{id}` is already being applied"));
            }
            Err(CreateNewJsonError::Other(message)) => return Err(message),
        }
        let pending_path = self.change_path("pending", id)?;
        if let Err(error) = fs::remove_file(&pending_path) {
            let _ = fs::remove_file(&sending_path);
            return Err(error.to_string());
        }
        Ok(approval)
    }

    /// Mark a claimed calendar change as approved after successful execution.
    pub(crate) fn complete_change(
        &self,
        id: &str,
        result_event_id: Option<&str>,
    ) -> Result<(), String> {
        let mut approval = self.load_change_approval("sending", id)?;
        approval.status = "approved".to_owned();
        approval.result_event_id = result_event_id.map(safe_persisted_line);
        let approved_path = self.change_path("approved", id)?;
        match atomic_json_create_new(&approved_path, &approval) {
            Ok(()) | Err(CreateNewJsonError::AlreadyExists) => {}
            Err(CreateNewJsonError::Other(message)) => return Err(message),
        }
        fs::remove_file(self.change_path("sending", id)?).map_err(|error| error.to_string())
    }

    /// Deny a pending calendar change.
    pub(crate) fn deny_change(&self, id: &str) -> Result<(), String> {
        self.move_pending_change(id, "denied")
    }

    fn list_change_approvals(&self, status: &str) -> Result<Vec<CalendarChangeApproval>, String> {
        let dir = self.change_dir(status);
        let mut paths = fs::read_dir(&dir)
            .map_err(|error| format!("failed to read {}: {error}", dir.display()))?
            .map(|entry| {
                entry
                    .map(|entry| entry.path())
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        paths.sort();
        paths
            .into_iter()
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
            .map(|path| {
                let approval: CalendarChangeApproval =
                    serde_json::from_slice(&read_sensitive_file(&path)?)
                        .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
                validate_calendar_change_approval(&approval, status, None)?;
                Ok(approval)
            })
            .collect()
    }

    fn load_change_approval(
        &self,
        status: &str,
        id: &str,
    ) -> Result<CalendarChangeApproval, String> {
        let path = self.change_path(status, id)?;
        if !path.exists() {
            return Err(format!("calendar change `{id}` not found"));
        }
        let approval: CalendarChangeApproval = serde_json::from_slice(&read_sensitive_file(&path)?)
            .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
        validate_calendar_change_approval(&approval, status, Some(id))?;
        Ok(approval)
    }

    fn move_pending_change(&self, id: &str, new_status: &str) -> Result<(), String> {
        validate_change_id(id)?;
        let from = self.change_path("pending", id)?;
        let to = self.change_path(new_status, id)?;
        if from.exists() {
            let mut record: serde_json::Value =
                serde_json::from_slice(&read_sensitive_file(&from)?)
                    .map_err(|error| format!("failed to parse {}: {error}", from.display()))?;
            validate_change_record(&record, "pending", id)?;
            record["status"] = serde_json::Value::String(new_status.to_owned());
            match atomic_json_create_new(&to, &record) {
                Ok(()) | Err(CreateNewJsonError::AlreadyExists) => {}
                Err(CreateNewJsonError::Other(message)) => return Err(message),
            }
            match fs::remove_file(&from) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error.to_string()),
            }
        } else if to.exists() {
            Ok(())
        } else {
            Err(format!("calendar change `{id}` not found"))
        }
    }

    fn calendar_log_path(&self) -> PathBuf {
        self.state_dir.join("logs").join("calendar.jsonl")
    }

    fn google_auth_path(&self, account: &str) -> PathBuf {
        self.state_dir
            .join("auth")
            .join("google")
            .join(format!("{}.json", account_file_stem(account)))
    }

    fn google_pending_auth_path(&self, account: &str) -> PathBuf {
        self.state_dir
            .join("auth")
            .join("google-pending")
            .join(format!("{}.json", account_file_stem(account)))
    }

    fn change_dir(&self, status: &str) -> PathBuf {
        self.state_dir
            .join("approvals")
            .join(CHANGE_APPROVAL_KIND)
            .join(status)
    }

    fn change_path(&self, status: &str, id: &str) -> Result<PathBuf, String> {
        validate_change_id(id)?;
        Ok(self.change_dir(status).join(format!("{id}.json")))
    }

    fn next_change_id(&self) -> Result<String, String> {
        let mut max_id = 0_u64;
        for status in ["pending", "sending", "approved", "denied"] {
            let dir = self.change_dir(status);
            for entry in fs::read_dir(&dir)
                .map_err(|error| format!("failed to read {}: {error}", dir.display()))?
            {
                let path = entry.map_err(|error| error.to_string())?.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                    continue;
                };
                if let Ok(id) = stem.parse::<u64>()
                    && max_id < id
                {
                    max_id = id;
                }
            }
        }
        Ok((max_id + 1).to_string())
    }
}

fn validate_calendar_change_approval(
    approval: &CalendarChangeApproval,
    expected_status: &str,
    expected_id: Option<&str>,
) -> Result<(), String> {
    if approval.schema != CHANGE_SCHEMA {
        return Err(format!(
            "calendar change `{}` has unsupported schema",
            approval.id
        ));
    }
    validate_change_id(&approval.id)?;
    if let Some(expected_id) = expected_id
        && approval.id != expected_id
    {
        return Err(format!(
            "calendar change `{expected_id}` has mismatched embedded id"
        ));
    }
    if approval.kind != CHANGE_KIND {
        return Err(format!(
            "calendar change `{}` has mismatched embedded kind",
            approval.id
        ));
    }
    if approval.status != expected_status {
        return Err(format!(
            "calendar change `{}` has mismatched embedded status",
            approval.id
        ));
    }
    if !matches!(
        approval.command.as_str(),
        "create_event" | "update_event" | "delete_event" | "respond_invite"
    ) || !is_safe_persisted_line(&approval.command, MAX_CHANGE_FIELD_CHARS)
        || !is_safe_persisted_line(&approval.account, MAX_CHANGE_FIELD_CHARS)
        || !is_safe_persisted_line(&approval.calendar, MAX_CHANGE_FIELD_CHARS)
        || !is_safe_persisted_line(&approval.reason, MAX_CHANGE_FIELD_CHARS)
    {
        return Err(format!(
            "calendar change `{}` contains unsafe metadata",
            approval.id
        ));
    }
    validate_optional_line(approval.event_id.as_ref(), "event_id")?;
    validate_optional_line(approval.etag.as_ref(), "etag")?;
    validate_optional_line(approval.title.as_ref(), "title")?;
    validate_optional_multiline(approval.description.as_ref(), "description")?;
    validate_optional_line(approval.location.as_ref(), "location")?;
    validate_optional_line(approval.start.as_ref(), "start")?;
    validate_optional_line(approval.end.as_ref(), "end")?;
    validate_optional_line(approval.timezone.as_ref(), "timezone")?;
    validate_optional_line(approval.response.as_ref(), "response")?;
    validate_optional_line(approval.result_event_id.as_ref(), "result_event_id")?;
    if let Some(attendees) = &approval.attendees {
        if MAX_ATTENDEES_HARD < attendees.len() {
            return Err("calendar change contains too many attendees".to_owned());
        }
        for attendee in attendees {
            if !is_safe_persisted_line(attendee, MAX_CHANGE_FIELD_CHARS) {
                return Err("calendar change contains an unsafe attendee".to_owned());
            }
        }
    }
    Ok(())
}

fn validate_optional_line(value: Option<&String>, field: &str) -> Result<(), String> {
    if let Some(value) = value
        && !is_safe_persisted_line(value, MAX_CHANGE_FIELD_CHARS)
    {
        return Err(format!(
            "calendar change field `{field}` contains unsafe text"
        ));
    }
    Ok(())
}

fn validate_optional_multiline(value: Option<&String>, field: &str) -> Result<(), String> {
    if let Some(value) = value
        && !is_safe_persisted_multiline(value)
    {
        return Err(format!(
            "calendar change field `{field}` contains unsafe text"
        ));
    }
    Ok(())
}

fn calendar_change_matches(left: &CalendarChangeApproval, right: &CalendarChangeApproval) -> bool {
    left.command == right.command
        && left.account == right.account
        && left.calendar == right.calendar
        && left.event_id == right.event_id
        && left.etag == right.etag
        && left.title == right.title
        && left.description == right.description
        && left.location == right.location
        && left.start == right.start
        && left.end == right.end
        && left.timezone == right.timezone
        && left.attendees == right.attendees
        && left.response == right.response
}

fn validate_change_record(
    record: &serde_json::Value,
    expected_status: &str,
    id: &str,
) -> Result<(), String> {
    if record.get("schema").and_then(serde_json::Value::as_u64) != Some(u64::from(CHANGE_SCHEMA)) {
        return Err(format!("calendar change `{id}` has unsupported schema"));
    }
    let field = |name: &str| record.get(name).and_then(serde_json::Value::as_str);
    if field("id") != Some(id) {
        return Err(format!("calendar change `{id}` has mismatched embedded id"));
    }
    if field("kind") != Some(CHANGE_KIND) {
        return Err(format!(
            "calendar change `{id}` has mismatched embedded kind"
        ));
    }
    if field("status") != Some(expected_status) {
        return Err(format!(
            "calendar change `{id}` has mismatched embedded status"
        ));
    }
    Ok(())
}

fn validate_google_stored_auth(
    auth: &GoogleStoredAuth,
    expected_account: Option<&str>,
) -> Result<(), String> {
    if auth.schema != GOOGLE_AUTH_SCHEMA {
        return Err("Google auth has unsupported schema".to_owned());
    }
    validate_google_auth_account(&auth.account, expected_account)?;
    if !is_safe_persisted_line(&auth.refresh_token, MAX_GOOGLE_AUTH_FIELD_CHARS) {
        return Err("Google auth refresh token contains unsafe text".to_owned());
    }
    Ok(())
}

fn validate_google_pending_auth(
    pending: &GooglePendingAuth,
    expected_account: Option<&str>,
) -> Result<(), String> {
    if pending.schema != GOOGLE_AUTH_PENDING_SCHEMA {
        return Err("pending Google auth has unsupported schema".to_owned());
    }
    validate_google_auth_account(&pending.account, expected_account)?;
    for (field, value) in [
        ("device_code", pending.device_code.as_str()),
        ("user_code", pending.user_code.as_str()),
        ("verification_uri", pending.verification_uri.as_str()),
    ] {
        if !is_safe_persisted_line(value, MAX_GOOGLE_AUTH_FIELD_CHARS) {
            return Err(format!(
                "pending Google auth field `{field}` contains unsafe text"
            ));
        }
    }
    if pending.expires_at_unix_ms == 0 || pending.interval_secs == 0 {
        return Err("pending Google auth timing is invalid".to_owned());
    }
    Ok(())
}

fn validate_google_auth_account(
    account: &str,
    expected_account: Option<&str>,
) -> Result<(), String> {
    if account.trim().is_empty() || !is_safe_persisted_line(account, MAX_CHANGE_FIELD_CHARS) {
        return Err("Google auth account id is invalid".to_owned());
    }
    if let Some(expected_account) = expected_account
        && account != expected_account
    {
        return Err("Google auth account id mismatch".to_owned());
    }
    Ok(())
}

fn account_file_stem(account: &str) -> String {
    let digest = blake3::hash(account.as_bytes());
    digest.to_hex()[..16].to_owned()
}

fn validate_change_id(id: &str) -> Result<(), String> {
    let Ok(value) = id.parse::<u64>() else {
        return Err(format!("invalid calendar change id `{id}`"));
    };
    if value == 0 || id.contains(['/', '\\', '\0']) || !id.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("invalid calendar change id `{id}`"));
    }
    Ok(())
}

fn safe_persisted_line(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_control() || is_unsafe_format_control(ch) {
                ' '
            } else {
                ch
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_safe_persisted_line(value: &str, max_chars: usize) -> bool {
    value.chars().count() <= max_chars
        && !value
            .chars()
            .any(|ch| ch.is_control() || is_unsafe_format_control(ch))
}

fn is_safe_persisted_multiline(value: &str) -> bool {
    value.len() <= MAX_DESCRIPTION_BYTES
        && value.lines().count() <= MAX_DESCRIPTION_LINES
        && !value.chars().any(|ch| {
            (ch.is_control() && ch != '\n' && ch != '\r' && ch != '\t')
                || is_unsafe_format_control(ch)
        })
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

#[derive(Debug)]
enum CreateNewJsonError {
    AlreadyExists,
    Other(String),
}

fn current_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn create_private_dir_all(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|error| error.to_string())?;
    chmod_private_dir(path)
}

fn read_sensitive_file(path: &Path) -> Result<Vec<u8>, String> {
    chmod_private_file(path)?;
    fs::read(path).map_err(|error| error.to_string())
}

fn open_private_append(path: &Path) -> Result<fs::File, std::io::Error> {
    let mut options = fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(path)?;
    chmod_private_file_handle(&file)?;
    Ok(file)
}

fn create_private_file(path: &Path) -> Result<fs::File, std::io::Error> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(path)?;
    chmod_private_file_handle(&file)?;
    Ok(file)
}

#[cfg(unix)]
fn chmod_private_dir(path: &Path) -> Result<(), String> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("failed to chmod {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn chmod_private_dir(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn chmod_private_file(path: &Path) -> Result<(), String> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("failed to chmod {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn chmod_private_file(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn chmod_private_file_handle(file: &fs::File) -> Result<(), std::io::Error> {
    file.set_permissions(fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn chmod_private_file_handle(_file: &fs::File) -> Result<(), std::io::Error> {
    Ok(())
}

fn temp_json_path(parent: &Path, path: &Path) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    parent.join(format!(
        ".{}.tmp-{}-{nonce}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("state"),
        std::process::id()
    ))
}

fn write_json_temp<T: Serialize>(path: &Path, value: &T) -> Result<PathBuf, String> {
    let parent = path
        .parent()
        .ok_or_else(|| "state path has no parent".to_owned())?;
    create_private_dir_all(parent)?;
    let tmp = temp_json_path(parent, path);
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    {
        let mut file = create_private_file(&tmp).map_err(|error| error.to_string())?;
        file.write_all(&bytes).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
    }
    Ok(tmp)
}

fn atomic_json_create_new<T: Serialize>(path: &Path, value: &T) -> Result<(), CreateNewJsonError> {
    let tmp = write_json_temp(path, value).map_err(CreateNewJsonError::Other)?;
    match fs::hard_link(&tmp, path) {
        Ok(()) => {
            let _ = fs::remove_file(&tmp);
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = fs::remove_file(&tmp);
            Err(CreateNewJsonError::AlreadyExists)
        }
        Err(error) => {
            let _ = fs::remove_file(&tmp);
            Err(CreateNewJsonError::Other(error.to_string()))
        }
    }
}

fn atomic_json_replace<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let tmp = write_json_temp(path, value)?;
    fs::rename(&tmp, path).map_err(|error| {
        let _ = fs::remove_file(&tmp);
        format!(
            "failed to rename {} into {}: {error}",
            tmp.display(),
            path.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn file_mode(path: &Path) -> u32 {
        fs::metadata(path).expect("metadata").permissions().mode() & 0o777
    }

    #[test]
    fn recent_calendar_log_ignores_invalid_entries_and_keeps_limit() {
        // Logs can be manually truncated or edited during debugging. Invalid
        // lines should not break `/calendar log last`, and the tail limit should
        // still be enforced.
        let temp = tempfile::TempDir::new().expect("tempdir");
        let state = StateStore::open(temp.path().join("state")).expect("state");
        state
            .append_calendar_log(&CalendarLogEntry::tool("list_events", "ok"))
            .expect("append first");
        fs::write(
            temp.path().join("state/logs/calendar.jsonl"),
            b"not-json\n{\"schema\":2}\n{\"schema\":1,\"ts_unix_ms\":2,\"kind\":\"tool\",\"command\":\"read_event\",\"status\":\"ok\"}\n{\"schema\":1,\"ts_unix_ms\":3,\"kind\":\"tool\",\"command\":\"free_busy\",\"status\":\"ok\"}\n",
        )
        .expect("rewrite log");

        let entries = state.recent_calendar_log(1).expect("recent log");

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].command, "free_busy");
    }

    #[test]
    fn pending_calendar_changes_are_deduplicated_and_private() {
        // Calendar mutations can notify attendees, so they are persisted for
        // explicit user review and identical repeated tool calls reuse one id.
        let temp = tempfile::TempDir::new().expect("tempdir");
        let state = StateStore::open(temp.path().join("state")).expect("state");
        let mut change = CalendarChangeApproval::pending("create_event", "google", "primary");
        change.title = Some("Team sync".to_owned());
        change.start = Some("2026-05-28T12:00:00Z".to_owned());
        change.end = Some("2026-05-28T13:00:00Z".to_owned());

        let first = state.pending_change(&change).expect("pending change");
        let second = state.pending_change(&change).expect("same pending change");

        assert_eq!(first, second);
        let pending = state.list_pending_changes().expect("pending list");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].title.as_deref(), Some("Team sync"));
        #[cfg(unix)]
        assert_eq!(
            file_mode(
                &temp
                    .path()
                    .join("state/approvals/calendar-change/pending/1.json")
            ),
            0o600
        );
    }

    #[test]
    fn google_auth_tokens_and_pending_requests_are_private() {
        // Google refresh tokens and device codes are secrets. Persist them only
        // under owner-only files named by a hash of the account id.
        let temp = tempfile::TempDir::new().expect("tempdir");
        let state = StateStore::open(temp.path().join("state")).expect("state");

        state
            .save_google_refresh_token("work/account", "refresh-token")
            .expect("save refresh token");
        assert_eq!(
            state
                .google_refresh_token("work/account")
                .expect("load refresh token")
                .as_deref(),
            Some("refresh-token")
        );
        let auth_path = state.google_auth_path("work/account");
        let auth_file = auth_path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("auth filename");
        assert!(!auth_file.contains("work"));
        assert!(!auth_file.contains('/'));
        #[cfg(unix)]
        assert_eq!(file_mode(&auth_path), 0o600);

        let pending = GooglePendingAuth::new(
            "work/account",
            "device-code",
            "USER-CODE",
            "https://example.test/device",
            600,
            5,
        );
        state
            .save_pending_google_auth(&pending)
            .expect("save pending auth");
        assert_eq!(
            state
                .pending_google_auth("work/account")
                .expect("load pending auth")
                .device_code,
            "device-code"
        );
        let pending_path = state.google_pending_auth_path("work/account");
        #[cfg(unix)]
        assert_eq!(file_mode(&pending_path), 0o600);
        state
            .clear_pending_google_auth("work/account")
            .expect("clear pending auth");
        assert!(state.pending_google_auth("work/account").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn calendar_log_files_are_owner_only() {
        // Calendar logs contain schedule metadata. Existing permissive paths
        // must be tightened on both append and read paths.
        let temp = tempfile::TempDir::new().expect("tempdir");
        let state_dir = temp.path().join("state");
        fs::create_dir_all(state_dir.join("logs")).expect("mkdir");
        fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o755)).expect("chmod state");
        let log_path = state_dir.join("logs/calendar.jsonl");
        fs::write(&log_path, b"").expect("log");
        fs::set_permissions(&log_path, fs::Permissions::from_mode(0o644)).expect("chmod log");

        let state = StateStore::open(state_dir.clone()).expect("state");
        state
            .append_calendar_log(&CalendarLogEntry::tool("list_events", "ok"))
            .expect("append log");
        assert_eq!(file_mode(&state_dir), 0o700);
        assert_eq!(file_mode(&state_dir.join("logs")), 0o700);
        assert_eq!(file_mode(&log_path), 0o600);

        fs::set_permissions(&log_path, fs::Permissions::from_mode(0o644)).expect("chmod log");
        let entries = state.recent_calendar_log(1).expect("recent log");
        assert_eq!(entries.len(), 1);
        assert_eq!(file_mode(&log_path), 0o600);
    }
}
