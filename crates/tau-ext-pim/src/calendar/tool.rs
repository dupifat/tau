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
    /// Invitation response: accepted, tentative, or declined.
    pub(crate) response: Option<String>,
}

/// Return the model-visible calendar tool specification.
pub fn calendar_tool_spec() -> ToolSpec {
    ToolSpec {
        name: tau_proto::ToolName::new(TOOL_NAME),
        model_visible_name: None,
        description: Some("Controlled calendar access. Use list_accounts, list_calendars, list_events, read_event, free_busy, create_event, update_event, delete_event, or respond_invite. Results use ok/command/status/data envelopes with line arrays and format fields. For event ranges pass start; end is optional and defaults to 7 days after start. Time values may be RFC3339 with offset, YYYY-MM-DD all-day dates, or local YYYY-MM-DDTHH:MM:SS interpreted in the configured calendar timezone. Existing event writes require event_id; ETags are handled internally.".to_owned()),
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
                    "description": "Command-specific arguments. account/calendar are optional when there is only one configured target.",
                    "properties": {
                        "account": {"type": "string", "description": "Configured calendar account id."},
                        "calendar": {"type": "string", "description": "Calendar id within the account."},
                        "event_id": {"type": "string", "description": "Backend event id."},
                        "limit": {"type": "integer", "minimum": 1, "maximum": 100},
                        "cursor": {"type": "string", "description": "Cursor returned as next_cursor by list_events or free_busy."},
                        "title": {"type": "string"},
                        "description": {"type": "string"},
                        "location": {"type": "string"},
                        "start": {"type": "string", "description": "Range or event start. Use RFC3339 with offset, YYYY-MM-DD, or local YYYY-MM-DDTHH:MM:SS."},
                        "end": {"type": "string", "description": "Range or event end. For update_event, pass end whenever start is passed."},
                        "attendees": {"type": "array", "items": {"type": "string"}},
                        "response": {"type": "string", "enum": ["accepted", "tentative", "declined"]}
                    },
                    "additionalProperties": false
                }
            },
            "required": ["command"],
            "additionalProperties": false,
            "allOf": [
                {
                    "if": {"properties": {"command": {"const": "list_accounts"}}, "required": ["command"]},
                    "then": {"properties": {"args": {"maxProperties": 0}}}
                },
                {
                    "if": {"properties": {"command": {"const": "list_calendars"}}, "required": ["command"]},
                    "then": {"properties": {"args": {"properties": {"account": {}}, "additionalProperties": false}}}
                },
                {
                    "if": {"properties": {"command": {"enum": ["list_events", "free_busy"]}}, "required": ["command"]},
                    "then": {
                        "required": ["args"],
                        "properties": {"args": {
                            "required": ["start"],
                            "properties": {"account": {}, "calendar": {}, "start": {}, "end": {}, "limit": {}, "cursor": {}},
                            "additionalProperties": false
                        }}
                    }
                },
                {
                    "if": {"properties": {"command": {"const": "read_event"}}, "required": ["command"]},
                    "then": {
                        "required": ["args"],
                        "properties": {"args": {
                            "required": ["event_id"],
                            "properties": {"account": {}, "calendar": {}, "event_id": {}},
                            "additionalProperties": false
                        }}
                    }
                },
                {
                    "if": {"properties": {"command": {"const": "create_event"}}, "required": ["command"]},
                    "then": {
                        "required": ["args"],
                        "properties": {"args": {
                            "required": ["title", "start"],
                            "properties": {"account": {}, "calendar": {}, "title": {}, "description": {}, "location": {}, "start": {}, "end": {}, "attendees": {}},
                            "additionalProperties": false
                        }}
                    }
                },
                {
                    "if": {"properties": {"command": {"const": "update_event"}}, "required": ["command"]},
                    "then": {
                        "required": ["args"],
                        "properties": {"args": {
                            "required": ["event_id"],
                            "anyOf": [{"required": ["title"]}, {"required": ["description"]}, {"required": ["location"]}, {"required": ["start"]}, {"required": ["end"]}, {"required": ["attendees"]}],
                            "dependentRequired": {"start": ["end"], "end": ["start"]},
                            "properties": {"account": {}, "calendar": {}, "event_id": {}, "title": {}, "description": {}, "location": {}, "start": {}, "end": {}, "attendees": {}},
                            "additionalProperties": false
                        }}
                    }
                },
                {
                    "if": {"properties": {"command": {"const": "delete_event"}}, "required": ["command"]},
                    "then": {
                        "required": ["args"],
                        "properties": {"args": {
                            "required": ["event_id"],
                            "properties": {"account": {}, "calendar": {}, "event_id": {}},
                            "additionalProperties": false
                        }}
                    }
                },
                {
                    "if": {"properties": {"command": {"const": "respond_invite"}}, "required": ["command"]},
                    "then": {
                        "required": ["args"],
                        "properties": {"args": {
                            "required": ["event_id", "response"],
                            "properties": {"account": {}, "calendar": {}, "event_id": {}, "response": {}},
                            "additionalProperties": false
                        }}
                    }
                }
            ]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_schema_hides_timezone_and_has_command_conditionals() {
        // Weak local models need command-specific schema constraints rather
        // than prose-only guidance. Keep timezone out of model-visible args.
        let schema = calendar_tool_spec().parameters.expect("parameters");
        let args_properties = schema
            .pointer("/properties/args/properties")
            .and_then(serde_json::Value::as_object)
            .expect("args properties");

        assert!(!args_properties.contains_key("timezone"));
        assert!(
            schema
                .get("allOf")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|rules| 7 < rules.len())
        );
    }
}
