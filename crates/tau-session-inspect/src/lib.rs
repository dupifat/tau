//! Read-only session/policy inspection for CLI sub-commands and scripts.
//!
//! Operates entirely on `tau-core` types and the on-disk session/policy
//! format. Intentionally has no dependency on the harness daemon, so
//! `tau session show` / `tau policy list` / similar commands don't drag
//! in the agent, extension supervisor, or event-loop graph just to
//! render an events.jsonl.

use std::path::{Path, PathBuf};
use std::{fmt, io};

use tau_core::{PolicyStore, SessionEntry, SessionStore, SessionStoreError, SessionTree};
use tau_proto::{
    CborValue, ContentPart, ContextItem, EventSelector, ToolCallItem, ToolResultStatus,
};

/// Errors from the read-only inspection paths.
#[derive(Debug)]
pub enum InspectError {
    Io(io::Error),
    SessionStore(SessionStoreError),
}

impl fmt::Display for InspectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(source) => write!(f, "I/O error: {source}"),
            Self::SessionStore(source) => write!(f, "session store error: {source}"),
        }
    }
}

impl std::error::Error for InspectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(source) => Some(source),
            Self::SessionStore(source) => Some(source),
        }
    }
}

impl From<io::Error> for InspectError {
    fn from(source: io::Error) -> Self {
        Self::Io(source)
    }
}

impl From<SessionStoreError> for InspectError {
    fn from(source: SessionStoreError) -> Self {
        Self::SessionStore(source)
    }
}

/// Returns the default per-state directory: `$XDG_STATE_HOME/tau` (typically
/// `~/.local/state/tau` on Linux), or `.tau/state` if no state dir is
/// available.
#[must_use]
pub fn default_state_dir() -> PathBuf {
    tau_config::settings::state_dir().unwrap_or_else(|| PathBuf::from(".tau").join("state"))
}

/// Returns the default per-session storage root: `default_state_dir()` joined
/// with `sessions/`. Session subdirectories live one level deeper to keep the
/// state-dir top level reserved for tau-wide scalar files (`policy.cbor`,
/// `cli.json`, …).
#[must_use]
pub fn default_sessions_dir() -> PathBuf {
    tau_config::settings::sessions_dir_of(&default_state_dir())
}

#[must_use]
pub fn default_session_id() -> &'static str {
    "default"
}

pub fn open_session_store(path: impl AsRef<Path>) -> Result<SessionStore, InspectError> {
    SessionStore::open(path.as_ref()).map_err(InspectError::from)
}

pub fn open_policy_store(path: impl AsRef<Path>) -> Result<PolicyStore, InspectError> {
    PolicyStore::open(path.as_ref()).map_err(InspectError::from)
}

pub fn session_lines(
    path: impl AsRef<Path>,
    session_id: &str,
) -> Result<Vec<String>, InspectError> {
    let store = open_session_store(path)?;
    let Some(tree) = store.session(session_id) else {
        return Ok(vec![format!("session {session_id} not found")]);
    };
    Ok(tree
        .current_branch()
        .into_iter()
        .enumerate()
        .map(|(i, e)| format!("{}: {}", i + 1, format_session_entry(e)))
        .collect())
}

pub fn session_list_lines(path: impl AsRef<Path>) -> Result<Vec<String>, InspectError> {
    let store = open_session_store(path)?;
    let mut sessions = store.sessions();
    sessions.sort_by(|a, b| a.session_id().cmp(b.session_id()));
    if sessions.is_empty() {
        return Ok(vec!["no sessions".to_owned()]);
    }
    Ok(sessions
        .into_iter()
        .map(|s| {
            let branch = s.current_branch();
            format!(
                "{} ({} entries){}",
                s.session_id(),
                branch.len(),
                latest_agent_preview(s)
                    .map(|p| format!(": {p}"))
                    .unwrap_or_default()
            )
        })
        .collect())
}

pub fn policy_lines(path: impl AsRef<Path>) -> Result<Vec<String>, InspectError> {
    let store = open_policy_store(path)?;
    let mut approvals = store.approvals().to_vec();
    approvals.sort_by(|a, b| a.connection_name.cmp(&b.connection_name));
    if approvals.is_empty() {
        return Ok(vec!["no policy approvals".to_owned()]);
    }
    Ok(approvals
        .into_iter()
        .map(|a| {
            let sels = a
                .selectors
                .iter()
                .map(|s| match s {
                    EventSelector::Exact(n) => n.to_string(),
                    EventSelector::Prefix(p) => format!("{p}*"),
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "{} [{:?}] -> {sels}",
                a.connection_name, a.connection_origin
            )
        })
        .collect())
}

/// Pretty-print one session entry for line-oriented inspection output
/// (`tau session show`, `/tree`, debug log).
#[must_use]
pub fn format_session_entry(entry: &SessionEntry) -> String {
    match entry {
        SessionEntry::UserInput { items } => {
            format!("user: {}", first_message_text(items).unwrap_or_default())
        }
        SessionEntry::Compaction { replacement_window } => {
            format!("compacted: {} item(s)", replacement_window.len())
        }
        SessionEntry::AssistantResponse { output_items, .. } => {
            let body =
                assistant_output_preview(output_items).unwrap_or_else(|| "(no text)".to_owned());
            format!("agent: {body}")
        }
        SessionEntry::ToolResults { items } => {
            if items.is_empty() {
                return "tool.result (empty)".to_owned();
            };
            items
                .iter()
                .map(format_tool_result_item)
                .collect::<Vec<_>>()
                .join("; ")
        }
    }
}

fn format_tool_result_item(item: &tau_proto::ToolResultItem) -> String {
    match &item.status {
        ToolResultStatus::Success => {
            let preview = truncate_chars(&cbor_to_text(&item.output), 80);
            format!("tool.result {} -> {preview}", item.call_id)
        }
        ToolResultStatus::Error { message } => {
            format!("tool.error {} -> {message}", item.call_id)
        }
        ToolResultStatus::Cancelled { reason } => {
            format!("tool.cancelled {} -> {reason}", item.call_id)
        }
    }
}

#[must_use]
pub fn latest_agent_preview(session: &SessionTree) -> Option<String> {
    session
        .current_branch()
        .into_iter()
        .rev()
        .find_map(|e| match e {
            SessionEntry::AssistantResponse { output_items, .. } => {
                assistant_output_preview(output_items)
            }
            SessionEntry::Compaction { .. } => None,
            _ => None,
        })
}

fn assistant_output_preview(items: &[ContextItem]) -> Option<String> {
    let parts = items
        .iter()
        .filter_map(|item| match item {
            ContextItem::Message(_) => first_message_text(std::slice::from_ref(item)),
            ContextItem::ToolCall(call) => Some(tool_call_preview(call)),
            _ => None,
        })
        .collect::<Vec<_>>();
    (!parts.is_empty()).then_some(parts.join(" "))
}

fn tool_call_preview(call: &ToolCallItem) -> String {
    let args = match call.arguments {
        CborValue::Map(ref entries) => entries.iter().find_map(|(key, value)| match (key, value) {
            (CborValue::Text(key), CborValue::Text(value))
                if matches!(key.as_str(), "path" | "pattern" | "command" | "task_name") =>
            {
                Some(value.clone())
            }
            _ => None,
        }),
        _ => None,
    };
    match args {
        Some(args) if !args.is_empty() => format!("tool.call {} {args}", call.name),
        _ => format!("tool.call {}", call.name),
    }
}

fn first_message_text(items: &[ContextItem]) -> Option<String> {
    items.iter().find_map(|item| match item {
        ContextItem::Message(message) => {
            let mut text = String::new();
            for part in &message.content {
                let ContentPart::Text { text: part } = part;
                text.push_str(part);
            }
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    })
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let preview: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{preview}...")
    } else {
        preview
    }
}

/// Convert a CBOR value to human-readable text for tool-result previews.
fn cbor_to_text(v: &CborValue) -> String {
    match v {
        CborValue::Null => String::new(),
        CborValue::Bool(b) => b.to_string(),
        CborValue::Integer(i) => {
            let n: i128 = (*i).into();
            n.to_string()
        }
        CborValue::Float(f) => f.to_string(),
        CborValue::Text(s) => s.clone(),
        CborValue::Bytes(b) => format!("<{} bytes>", b.len()),
        CborValue::Array(arr) => arr.iter().map(cbor_to_text).collect::<Vec<_>>().join("\n"),
        CborValue::Map(entries) => {
            let mut parts = Vec::new();
            for (k, val) in entries {
                let key = match k {
                    CborValue::Text(s) => s.clone(),
                    other => cbor_to_text(other),
                };
                let value = cbor_to_text(val);
                if value.contains('\n') {
                    parts.push(format!("{key}:\n{value}"));
                } else {
                    parts.push(format!("{key}: {value}"));
                }
            }
            parts.join("\n")
        }
        CborValue::Tag(_, inner) => cbor_to_text(inner),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use tau_proto::{ContextRole, MessageItem, ToolType};

    use super::*;

    fn assistant_message(text: impl Into<String>) -> ContextItem {
        ContextItem::Message(MessageItem {
            role: ContextRole::Assistant,
            content: vec![ContentPart::Text { text: text.into() }],
            phase: None,
        })
    }

    #[test]
    fn assistant_preview_represents_multiple_messages_and_tool_calls_in_order() {
        let output_items = vec![
            assistant_message("first"),
            ContextItem::ToolCall(ToolCallItem {
                call_id: "call-1".into(),
                name: tau_proto::ToolName::new("read"),
                tool_type: ToolType::Function,
                arguments: CborValue::Map(vec![(
                    CborValue::Text("path".into()),
                    CborValue::Text("src/main.rs".into()),
                )]),
            }),
            assistant_message("second"),
        ];

        assert_eq!(
            assistant_output_preview(&output_items).as_deref(),
            Some("first tool.call read src/main.rs second")
        );
        assert_eq!(
            format_session_entry(&SessionEntry::AssistantResponse {
                provider_response_id: None,
                backend: None,
                output_items,
                usage: None,
            }),
            "agent: first tool.call read src/main.rs second"
        );
    }

    #[test]
    fn tool_results_preview_includes_every_result_in_round() {
        let entry = SessionEntry::ToolResults {
            items: vec![
                tau_proto::ToolResultItem {
                    call_id: "call-1".into(),
                    tool_type: ToolType::Function,
                    status: ToolResultStatus::Success,
                    output: CborValue::Text("ok".into()),
                },
                tau_proto::ToolResultItem {
                    call_id: "call-2".into(),
                    tool_type: ToolType::Function,
                    status: ToolResultStatus::Error {
                        message: "failed".into(),
                    },
                    output: CborValue::Null,
                },
            ],
        };

        assert_eq!(
            format_session_entry(&entry),
            "tool.result call-1 -> ok; tool.error call-2 -> failed"
        );
    }
}
