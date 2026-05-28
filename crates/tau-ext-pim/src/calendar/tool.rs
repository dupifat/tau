use serde::Deserialize;
use tau_proto::{PromptFragment, PromptPriority, ToolSpec};

use super::TOOL_NAME;

/// Parsed calendar tool invocation.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ToolInvocation {
    /// Calendar command to run.
    pub(crate) command: CalendarCommand,
    /// Command arguments.
    #[serde(default)]
    pub(crate) args: CalendarArgs,
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

/// Calendar command arguments.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct CalendarArgs {
    /// Configured account id.
    pub(crate) account: Option<String>,
    /// Calendar id within the account.
    pub(crate) calendar: Option<String>,
    /// Backend event id.
    pub(crate) event_id: Option<String>,
    /// Backend event ETag or version for stale-write protection.
    pub(crate) etag: Option<String>,
    /// Inclusive lower RFC3339 time bound for list/free-busy commands.
    pub(crate) time_min: Option<String>,
    /// Exclusive upper RFC3339 time bound for list/free-busy commands.
    pub(crate) time_max: Option<String>,
    /// Maximum rows to return.
    pub(crate) limit: Option<u32>,
    /// Pagination cursor.
    pub(crate) cursor: Option<String>,
    /// Event title for create/update commands.
    pub(crate) title: Option<String>,
    /// Event description for create/update commands.
    pub(crate) description: Option<String>,
    /// Event location for create/update commands.
    pub(crate) location: Option<String>,
    /// Event start as RFC3339 date-time or all-day date.
    pub(crate) start: Option<String>,
    /// Event end as RFC3339 date-time or all-day exclusive date.
    pub(crate) end: Option<String>,
    /// IANA timezone for date-time values.
    pub(crate) timezone: Option<String>,
    /// Attendee email addresses.
    pub(crate) attendees: Option<Vec<String>>,
    /// Invitation response: accepted, tentative, or declined.
    pub(crate) response: Option<String>,
}

/// Return the model-visible calendar tool specification.
pub fn calendar_tool_spec() -> ToolSpec {
    ToolSpec {
        name: tau_proto::ToolName::new(TOOL_NAME),
        model_visible_name: None,
        description: Some("Controlled calendar access through configured accounts. Commands: list_accounts, list_calendars, list_events, read_event, free_busy, create_event, update_event, delete_event, respond_invite. Google calendar mutations are queued for user approval by default and require explicit account/calendar targets plus etag for existing events. ICS feed accounts are read-only. Use explicit RFC3339 timestamps or YYYY-MM-DD all-day dates and IANA timezones; do not pass natural-language dates.".to_owned()),
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
                    "description": "Command arguments. list_accounts takes no arguments. Other commands generally require account/calendar once more than one target is configured. Mutations for existing events require event_id and etag for stale-write protection.",
                    "properties": {
                        "account": {"type": "string", "description": "Configured calendar account id."},
                        "calendar": {"type": "string", "description": "Calendar id within the account."},
                        "event_id": {"type": "string", "description": "Backend event id."},
                        "etag": {"type": "string", "description": "Backend ETag or version for stale-write protection."},
                        "time_min": {"type": "string", "description": "Inclusive lower RFC3339 bound."},
                        "time_max": {"type": "string", "description": "Exclusive upper RFC3339 bound."},
                        "limit": {"type": "integer", "minimum": 1, "maximum": 100},
                        "cursor": {"type": "string"},
                        "title": {"type": "string"},
                        "description": {"type": "string"},
                        "location": {"type": "string"},
                        "start": {"type": "string", "description": "RFC3339 date-time or YYYY-MM-DD all-day start."},
                        "end": {"type": "string", "description": "RFC3339 date-time or YYYY-MM-DD all-day exclusive end."},
                        "timezone": {"type": "string"},
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
