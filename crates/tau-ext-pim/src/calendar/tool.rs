use serde::Deserialize;
use tau_proto::{CborValue, PromptFragment, PromptPriority, ToolSpec};

use super::TOOL_NAME;

/// Parsed calendar tool invocation.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ToolInvocation {
    /// Calendar command to run.
    pub(crate) command: CalendarCommand,
    /// Raw command arguments, parsed into command-specific structs after the
    /// command is known.
    #[serde(default)]
    pub(crate) args: Option<CborValue>,
}

/// Calendar command names accepted by the model-visible tool.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CalendarCommand {
    /// List configured calendar accounts.
    ListAccounts,
    /// List calendars visible within an account.
    ListCalendars,
    /// List events in a bounded time range.
    ListEvents,
    /// Read one event by backend id.
    ReadEvent,
    /// Return busy blocks without event details.
    FreeBusy,
    /// Create a new event.
    CreateEvent,
    /// Update an existing event.
    UpdateEvent,
    /// Delete or cancel an event.
    DeleteEvent,
    /// Accept, tentatively accept, or decline an invitation.
    RespondInvite,
}

/// Empty argument object for commands that do not accept arguments.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct NoArgs {}

/// Arguments for listing calendars in an account.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ListCalendarsArgs {
    /// Configured account id.
    pub(crate) account: Option<String>,
}

/// Arguments for bounded calendar range reads.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct CalendarRangeArgs {
    /// Configured account id.
    pub(crate) account: Option<String>,
    /// Calendar id within the account.
    pub(crate) calendar: Option<String>,
    /// Inclusive lower RFC3339 time bound.
    pub(crate) start: Option<String>,
    /// Exclusive upper RFC3339 time bound.
    pub(crate) end: Option<String>,
    /// Maximum rows to return.
    pub(crate) limit: Option<u32>,
    /// Pagination cursor.
    pub(crate) cursor: Option<String>,
}

/// Arguments for reading one event by backend id.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ReadEventArgs {
    /// Configured account id.
    pub(crate) account: Option<String>,
    /// Calendar id within the account.
    pub(crate) calendar: Option<String>,
    /// Backend event id.
    pub(crate) event_id: Option<String>,
}

/// Arguments for creating an event.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct CreateEventArgs {
    /// Configured account id.
    pub(crate) account: Option<String>,
    /// Calendar id within the account.
    pub(crate) calendar: Option<String>,
    /// Event title.
    pub(crate) title: Option<String>,
    /// Event description.
    pub(crate) description: Option<String>,
    /// Event location.
    pub(crate) location: Option<String>,
    /// Event start as RFC3339 date-time or all-day date.
    pub(crate) start: Option<String>,
    /// Event end as RFC3339 date-time or all-day exclusive date.
    pub(crate) end: Option<String>,
    /// IANA timezone for date-time values.
    pub(crate) timezone: Option<String>,
    /// Attendee email addresses.
    pub(crate) attendees: Option<Vec<String>>,
}

/// Arguments for updating an existing event.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct UpdateEventArgs {
    /// Configured account id.
    pub(crate) account: Option<String>,
    /// Calendar id within the account.
    pub(crate) calendar: Option<String>,
    /// Backend event id.
    pub(crate) event_id: Option<String>,
    /// Backend event ETag or version for stale-write protection.
    pub(crate) etag: Option<String>,
    /// Event title.
    pub(crate) title: Option<String>,
    /// Event description.
    pub(crate) description: Option<String>,
    /// Event location.
    pub(crate) location: Option<String>,
    /// Event start as RFC3339 date-time or all-day date.
    pub(crate) start: Option<String>,
    /// Event end as RFC3339 date-time or all-day exclusive date.
    pub(crate) end: Option<String>,
    /// IANA timezone for date-time values.
    pub(crate) timezone: Option<String>,
    /// Attendee email addresses.
    pub(crate) attendees: Option<Vec<String>>,
}

/// Arguments for deleting an existing event.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct DeleteEventArgs {
    /// Configured account id.
    pub(crate) account: Option<String>,
    /// Calendar id within the account.
    pub(crate) calendar: Option<String>,
    /// Backend event id.
    pub(crate) event_id: Option<String>,
    /// Backend event ETag or version for stale-write protection.
    pub(crate) etag: Option<String>,
}

/// Arguments for responding to an invite.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct RespondInviteArgs {
    /// Configured account id.
    pub(crate) account: Option<String>,
    /// Calendar id within the account.
    pub(crate) calendar: Option<String>,
    /// Backend event id.
    pub(crate) event_id: Option<String>,
    /// Backend event ETag or version for stale-write protection.
    pub(crate) etag: Option<String>,
    /// Invitation response: accepted, tentative, or declined.
    pub(crate) response: Option<String>,
}

/// Return the model-visible calendar tool specification.
pub fn calendar_tool_spec() -> ToolSpec {
    ToolSpec {
        name: tau_proto::ToolName::new(TOOL_NAME),
        model_visible_name: None,
        description: Some("Controlled calendar access through configured accounts. Commands: list_accounts, list_calendars, list_events, read_event, free_busy, create_event, update_event, delete_event, respond_invite. Results use the email-style ok/command/status/data envelope; list/detail data includes a format field and sanitized line arrays. For list_events/free_busy, use start/end as event range filters; start is inclusive, end is exclusive, and omitted end defaults to start plus 7 days. Read bounds accept RFC3339 timestamps with offsets, YYYY-MM-DD dates, or local YYYY-MM-DDTHH:MM:SS values interpreted in the account timezone. Google calendar mutations are queued for user approval by default and require explicit account/calendar targets plus etag for existing events. ICS feed accounts are read-only. Use explicit timestamps or YYYY-MM-DD all-day dates; do not pass natural-language dates. For create_event, omit end only when the intended default duration is one hour for date-times or one day for all-day dates.".to_owned()),
        tool_type: tau_proto::ToolType::Function,
        parameters: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "enum": ["list_accounts", "list_calendars", "list_events", "read_event", "free_busy", "create_event", "update_event", "delete_event", "respond_invite"],
                    "description": "Calendar operation to perform."
                },
                "args": {
                    "type": "object",
                    "description": "Command arguments. list_accounts takes no arguments. Other commands generally require account/calendar once more than one target is configured. list_events and free_busy use start/end for range bounds; omitted end defaults to start plus 7 days, and cursor can be passed from the previous next_cursor. Mutations for existing events require event_id and etag for stale-write protection.",
                    "properties": {
                        "account": {"type": "string", "description": "Configured calendar account id."},
                        "calendar": {"type": "string", "description": "Calendar id within the account."},
                        "event_id": {"type": "string", "description": "Backend event id."},
                        "etag": {"type": "string", "description": "Backend ETag or version for stale-write protection."},
                        "limit": {"type": "integer", "minimum": 1, "maximum": 100},
                        "cursor": {"type": "string", "description": "Cursor returned as next_cursor by list_events or free_busy; pass it with the same account/calendar/range arguments."},
                        "title": {"type": "string"},
                        "description": {"type": "string"},
                        "location": {"type": "string"},
                        "start": {"type": "string", "description": "Event start for create/update, or inclusive lower bound for list_events/free_busy. Read bounds accept RFC3339 with offset, YYYY-MM-DD, or local YYYY-MM-DDTHH:MM:SS interpreted in the account timezone."},
                        "end": {"type": "string", "description": "Event end for create/update, or exclusive upper bound for list_events/free_busy. For list_events/free_busy, omitted end defaults to start plus 7 days. create_event may omit this to default to start plus one hour for date-times or plus one day for all-day dates."},
                        "timezone": {"type": "string", "description": "IANA timezone for create/update payloads. Read bounds should include offsets in start/end."},
                        "attendees": {"type": "array", "items": {"type": "string"}},
                        "response": {"type": "string", "enum": ["accepted", "tentative", "declined"]}
                    },
                    "additionalProperties": false
                }
            },
            "required": ["command"],
            "additionalProperties": false
        })),
        format: None,
        enabled_by_default: false,
        background_support: None,
    }
}

/// Return the prompt fragment that teaches the model calendar tool policy.
pub fn calendar_prompt_fragment() -> PromptFragment {
    PromptFragment::new(
        "calendar.instructions",
        PromptPriority::new(120),
        include_str!("prompts/calendar_instructions.md"),
    )
}
