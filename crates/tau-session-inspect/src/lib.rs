//! Read-only session/policy inspection for CLI sub-commands and scripts.
//!
//! Operates entirely on `tau-core` types and the on-disk session/policy
//! format. Intentionally has no dependency on the harness daemon, so
//! `tau session show` / `tau policy list` / similar commands don't drag
//! in the agent, extension supervisor, or event-loop graph just to
//! render an events.jsonl.

use std::path::{Path, PathBuf};
use std::{fmt, io};

use tau_core::{
    PolicyStore, SessionEntry, SessionStore, SessionStoreError, SessionTree, ToolActivityOutcome,
};
use tau_proto::{CborValue, EventSelector};

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
        SessionEntry::UserMessage { text } => format!("user: {text}"),
        SessionEntry::CompactedSummary { summary, .. } => {
            let preview = if summary.len() > 80 {
                format!("{}...", &summary[..80])
            } else {
                summary.clone()
            };
            format!("compacted: {preview}")
        }
        SessionEntry::AgentMessage { text, .. } => {
            // `text` is now Option<String> — tool-only turns with
            // reasoning persist as an AgentMessage with text=None.
            // Render the empty case as "agent: (no text)" so a session
            // inspector still surfaces the entry rather than a phantom
            // blank line.
            let body = text.as_deref().unwrap_or("(no text)");
            format!("agent: {body}")
        }
        SessionEntry::ToolActivity(a) => match &a.outcome {
            ToolActivityOutcome::Requested { arguments, .. } => {
                if a.tool_name.as_str() == "skill" {
                    let query = cbor_query_label(arguments, "query");
                    if query.is_empty() {
                        "tool.request skill".to_owned()
                    } else {
                        format!("tool.request skill {query}")
                    }
                } else {
                    format!("tool.request {}", a.tool_name)
                }
            }
            ToolActivityOutcome::Result { result } => {
                let text = cbor_to_text(result);
                let preview = if text.len() > 80 {
                    format!("{}...", &text[..80])
                } else {
                    text
                };
                format!("tool.result {} -> {preview}", a.tool_name)
            }
            ToolActivityOutcome::Error { message, .. } => {
                format!("tool.error {} -> {message}", a.tool_name)
            }
        },
    }
}

#[must_use]
pub fn latest_agent_preview(session: &SessionTree) -> Option<String> {
    session
        .current_branch()
        .into_iter()
        .rev()
        .find_map(|e| match e {
            SessionEntry::AgentMessage { text, .. } => text.clone(),
            SessionEntry::CompactedSummary { .. } => None,
            _ => None,
        })
}

fn cbor_query_label(map: &CborValue, key: &str) -> String {
    let CborValue::Map(entries) = map else {
        return String::new();
    };
    let Some(value) = entries.iter().find_map(|(k, v)| match k {
        CborValue::Text(k) if k == key => Some(v),
        _ => None,
    }) else {
        return String::new();
    };
    match value {
        CborValue::Text(s) => s.trim().to_owned(),
        CborValue::Array(items) => items
            .iter()
            .filter_map(|item| match item {
                CborValue::Text(s) => {
                    let s = s.trim();
                    (!s.is_empty()).then(|| s.to_owned())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
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
