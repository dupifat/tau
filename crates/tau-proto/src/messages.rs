//! Directional harness protocol messages.
//!
//! [`HarnessInputMessage`] is the set of messages the harness accepts from
//! peers. [`HarnessOutputMessage`] is the set of messages the harness sends to
//! peers. Events are never top-level protocol items: peer-authored events are
//! wrapped in [`HarnessInputMessage::Emit`], and harness deliveries are wrapped
//! in [`HarnessOutputMessage::Deliver`].
//!
//! Wire form: `{"message": "hello", "payload": {...}}` — flat, lower
//! snake_case names, distinct from [`crate::Event`]'s `{"event":
//! "tool.started", "payload": {...}}` shape.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{
    CborValue, ClientKind, Event, EventSelector, ExtensionName, InterceptionPriority,
    ToolDefinition,
};

// ---------------------------------------------------------------------------
// Lifecycle messages
// ---------------------------------------------------------------------------

/// Announcement sent by a participant after connecting.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol_version: u32,
    pub client_name: ExtensionName,
    pub client_kind: ClientKind,
}

/// Subscription request describing which events a participant wants.
///
/// Selectors describe event interest, not replay intent. The harness may send
/// selected durable catch-up to any peer, including extensions, with
/// [`EventDelivery::replay`] set to `true`. Side-effecting subscribers must
/// ignore replayed deliveries. This payload has no past-event opt-in field.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Subscribe {
    pub selectors: Vec<EventSelector>,
}

/// Interception request describing which event emissions a participant wants
/// to handle before they reach the event log and regular subscribers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Intercept {
    pub selectors: Vec<EventSelector>,
    pub priority: InterceptionPriority,
}

/// Readiness notification emitted after startup or handshake.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Ready {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Disconnect notification with an optional human-readable reason.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Disconnect {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Configuration handed to an extension at startup. Sent
/// point-to-point from the harness to the extension immediately
/// after the harness sees the extension's
/// [`Hello`](crate::Hello). Carries whatever the
/// `config: { … }` value was for that extension in `harness.yaml`,
/// or [`CborValue::Null`] / an empty map when no config was
/// provided. `state_dir` is the harness-assigned persistent state
/// directory for this extension instance, when the harness can provide
/// one.
///
/// `Eq` is not derivable because the underlying CBOR value can
/// contain floats; `PartialEq` is enough for tests.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Configure {
    /// Free-form extension configuration from harness settings.
    pub config: CborValue,
    /// Persistent directory reserved for this extension's runtime state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_dir: Option<PathBuf>,
    /// Secret values explicitly authorized for this extension.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secrets: BTreeMap<String, SecretValue>,
}

/// Secret text passed from the harness to one authorized extension.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretValue(String);

impl SecretValue {
    /// Wrap a resolved secret value for protocol transport.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the underlying secret text. Avoid logging this value.
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Reported by an extension when its
/// [`Configure`](Configure) value is malformed (or
/// otherwise unusable). The harness surfaces the message just like
/// a `harness.yaml` parse error so the user can see why their
/// per-extension config was rejected.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConfigError {
    pub message: String,
}

// ---------------------------------------------------------------------------
// Wire transport — event delivery
// ---------------------------------------------------------------------------

/// Wall-clock timestamp as microseconds since the UNIX epoch.
///
/// Stamped onto persisted session events and the JSONL debug log so
/// offline inspection can compute inter-event gaps, RPM bursts, and
/// correlations with provider-side cache misses. `u64` µs covers
/// ~584,000 years past 1970, so saturation is not a concern in
/// practice — callers still saturate on bogus clocks rather than
/// panic, keeping the persistence path infallible. A zero value
/// marks records written before this field existed
/// (`#[serde(default)]` on the carrying struct).
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    Eq,
    PartialEq,
    Ord,
    PartialOrd,
    Hash,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(transparent)]
pub struct UnixMicros(u64);

impl UnixMicros {
    #[must_use]
    pub fn new(v: u64) -> Self {
        Self(v)
    }

    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }

    /// Reads the current wall clock and returns a `UnixMicros`.
    /// Saturates on bogus clocks (pre-1970 or post-2554) instead of
    /// panicking, so callers on the durable-write path can stay
    /// infallible.
    #[must_use]
    pub fn now() -> Self {
        let micros = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        Self(micros)
    }
}

impl std::fmt::Display for UnixMicros {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A bus event delivered by the harness to one peer.
///
/// The protocol no longer has a bare top-level event lane. Every event the
/// harness sends to a peer is wrapped in [`HarnessOutputMessage::Deliver`] with
/// this payload so delivery metadata is explicitly harness-owned and
/// direction-specific.
///
/// `replay` distinguishes historical record from live occurrence: subscribe
/// time catch-up re-sends durable facts with `replay: true`. Consumers that
/// render state (UI transcripts) fold replay frames like live events;
/// consumers that perform side effects (sounds, tool execution, idle timers)
/// must skip them.
///
/// `recorded_at` is present for committed runtime deliveries and for durable
/// replay entries when a historical timestamp is meaningful. It is absent for
/// synthetic direct snapshots.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventDelivery {
    /// Inner bus fact delivered to the peer.
    pub event: Box<Event>,
    /// True when this delivery re-sends a durable historical fact to a late
    /// subscriber instead of announcing a live occurrence.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replay: bool,
    /// Runtime or historical append timestamp associated with the event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_at: Option<UnixMicros>,
}

impl EventDelivery {
    /// Creates a direct delivery for a synthesized current-state
    /// announcement (no meaningful append timestamp).
    #[must_use]
    pub fn direct(event: Event) -> Self {
        Self {
            event: Box::new(event),
            replay: false,
            recorded_at: None,
        }
    }

    /// Creates a delivery for a live committed runtime event.
    #[must_use]
    pub fn live(recorded_at: UnixMicros, event: Event) -> Self {
        Self {
            event: Box::new(event),
            replay: false,
            recorded_at: Some(recorded_at),
        }
    }

    /// Creates a replay delivery re-sending a durable historical fact with
    /// its persisted timestamp.
    #[must_use]
    pub fn replay(recorded_at: UnixMicros, event: Event) -> Self {
        Self {
            event: Box::new(event),
            replay: true,
            recorded_at: Some(recorded_at),
        }
    }

    /// Returns the inner delivered event.
    #[must_use]
    pub fn event(&self) -> &Event {
        &self.event
    }

    /// Returns true when this delivery re-sends a durable historical fact to
    /// a late subscriber instead of announcing a live occurrence.
    ///
    /// Replay frames describe the past. Consumers that render state (UI
    /// transcripts) fold them like live events; consumers that perform side
    /// effects (sounds, tool execution, idle timers) must skip them.
    #[must_use]
    pub fn is_replay(&self) -> bool {
        self.replay
    }

    /// Consumes this delivery and returns the inner event.
    #[must_use]
    pub fn into_event(self) -> Event {
        *self.event
    }

    /// Consumes this delivery and returns the event, the replay marker, and
    /// the append timestamp.
    #[must_use]
    pub fn into_parts(self) -> (Event, bool, Option<UnixMicros>) {
        (*self.event, self.replay, self.recorded_at)
    }
}

/// Extension/client request to emit one event with harness-owned delivery
/// metadata.
///
/// The inner `event` is the fact that subscribers see. `transient` controls
/// whether the harness writes eligible semantic facts to durable session or
/// agent event history; it is not part of the emitted fact itself.
///
/// `Emit` is strictly for peer → harness event emission. Harness → peer event
/// delivery uses [`HarnessOutputMessage::Deliver`] instead.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Emit {
    /// Event the peer asks the harness to publish.
    pub event: Box<Event>,
    /// True when the event should skip durable semantic logs.
    #[serde(default, skip_serializing_if = "core::ops::Not::not")]
    pub transient: bool,
}

impl Emit {
    /// Creates a durable-by-default emit request.
    #[must_use]
    pub fn new(event: Event) -> Self {
        Self {
            event: Box::new(event),
            transient: false,
        }
    }

    /// Creates an emit request with explicit transient metadata.
    #[must_use]
    pub fn with_transient(event: Event, transient: bool) -> Self {
        Self {
            event: Box::new(event),
            transient,
        }
    }

    /// Consumes this request and returns the inner event plus transient flag.
    #[must_use]
    pub fn into_parts(self) -> (Event, bool) {
        (*self.event, self.transient)
    }
}

/// Directed harness → interceptor message carrying an event emission that has
/// not reached the event log yet. The interceptor must reply with an
/// [`InterceptReply`]; until it does, the harness suspends draining of any
/// further publishes that would themselves be subject to interception.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InterceptRequest {
    /// Event being offered to the interceptor.
    pub event: Box<Event>,
    /// Original transient metadata from the publish request.
    #[serde(default, skip_serializing_if = "core::ops::Not::not")]
    pub transient: bool,
}

/// What an interceptor wants the harness to do with the event it was given.
///
/// `Pass(None)` republishes the original event unchanged (the common no-op
/// case). `Pass(Some(event))` substitutes a possibly-mutated version that flows
/// on through any remaining interceptors and then to subscribers. `Drop`
/// discards the event entirely — but the harness may override `Drop` for events
/// the publisher marked `must_pass`, `tracing::warn!`-ing and falling back to
/// the original.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum InterceptAction {
    Pass(Option<Box<Event>>),
    Drop,
}

/// Interceptor → harness response to an [`InterceptRequest`]. Exactly one reply
/// per request; out-of-order or duplicate replies are a programming error and
/// the harness logs + falls back to the original event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InterceptReply {
    pub action: InterceptAction,
}

/// Best-effort request for a materialized full `agent.prompt_created` payload
/// by id.
///
/// Prompt-created payloads are transient delivery objects; harnesses are not
/// required to retain them after live delivery. A missing prompt is reported as
/// `None` in [`AgentPromptCreatedResult::prompt`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GetAgentPromptCreated {
    /// Request correlation id echoed by [`AgentPromptCreatedResult`].
    pub request_id: String,
    /// Session containing the requested prompt.
    pub session_id: crate::SessionId,
    /// Prompt to materialize.
    pub agent_prompt_id: crate::AgentPromptId,
}

/// Response to [`GetAgentPromptCreated`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentPromptCreatedResult {
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<crate::AgentPromptCreated>,
}

/// Request that the harness render the effective system prompt for one role.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GetRenderedSystemPrompt {
    /// Request correlation id echoed by [`RenderedSystemPromptResult`].
    pub request_id: String,
    /// Role name whose resolved prompt should be rendered.
    pub role: String,
}

/// Response to [`GetRenderedSystemPrompt`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RenderedSystemPromptResult {
    /// Request correlation id copied from the request.
    pub request_id: String,
    /// Rendered prompt when the role exists and template rendering succeeds.
    /// Exactly one of `prompt` and `error` should be present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Human-readable failure when the role is unknown or rendering fails.
    /// Exactly one of `prompt` and `error` should be present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Request that the harness report the effective tools for one role.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GetRenderedToolDefinitions {
    /// Request correlation id echoed by [`RenderedToolDefinitionsResult`].
    pub request_id: String,
    /// Role name whose resolved tool list should be reported.
    pub role: String,
}

/// Response to [`GetRenderedToolDefinitions`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RenderedToolDefinitionsResult {
    /// Request correlation id copied from the request.
    pub request_id: String,
    /// Effective provider-facing tool definitions for the requested role.
    /// Exactly one of `tools` and `error` should be present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
    /// Human-readable failure when the role is unknown.
    /// Exactly one of `tools` and `error` should be present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// Extension data RPC
// ---------------------------------------------------------------------------

/// Harness-owned storage scope for extension data RPC requests.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionDataScope {
    /// Session-local data under `<session_data_dir>/ext/data/<ext-name>`.
    Session,
    /// User-persistent data under `~/.local/state/tau/ext/<ext-name>`.
    User,
    /// User cache data under `~/.cache/tau/ext/<ext-name>`.
    Cache,
}

/// Extension request for harness-mediated file access inside its data roots.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExtensionDataRequest {
    /// Request correlation id echoed by [`ExtensionDataResult`].
    pub request_id: String,
    /// Storage scope to access.
    pub scope: ExtensionDataScope,
    /// File operation to perform.
    pub op: ExtensionDataRequestOp,
}

/// File operation requested by an extension data RPC.
///
/// The harness may reject operations with
/// [`ExtensionDataErrorKind::QuotaExceeded`] when file contents or directory
/// listings exceed harness-owned resource limits.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ExtensionDataRequestOp {
    /// Read one whole file at a sanitized relative path, subject to the
    /// harness file-size quota.
    ReadFile { path: String },
    /// Write one whole file at a sanitized relative path, atomically replacing
    /// any old content and subject to the harness file-size quota.
    WriteFile { path: String, contents: Vec<u8> },
    /// Create one whole file at a sanitized relative path, failing when the
    /// file already exists and subject to the harness file-size quota.
    CreateFile { path: String, contents: Vec<u8> },
    /// Append bytes to one file at a sanitized relative path, creating it when
    /// missing and subject to the harness file-size quota.
    AppendFile { path: String, contents: Vec<u8> },
    /// Delete one file at a sanitized relative path. Missing files succeed.
    DeleteFile { path: String },
    /// Rename one file at sanitized relative paths. The destination must not
    /// already exist.
    RenameFile { from: String, to: String },
    /// List direct children of a sanitized relative directory path, subject to
    /// the harness directory-entry quota.
    ListFiles { path: String },
}
/// Harness response to an [`ExtensionDataRequest`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExtensionDataResult {
    /// Request correlation id copied from the request.
    pub request_id: String,
    /// Operation result or human-readable error.
    pub result: ExtensionDataResultPayload,
}

/// Result payload for an extension data RPC.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ExtensionDataResultPayload {
    /// Operation succeeded.
    Ok { value: ExtensionDataValue },
    /// Operation failed.
    Error {
        /// Machine-readable error kind.
        #[serde(default = "default_extension_data_error_kind")]
        kind: ExtensionDataErrorKind,
        /// Human-readable error details.
        message: String,
    },
}

/// Successful value returned by an extension data RPC.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ExtensionDataValue {
    /// Whole file contents from a read request.
    ReadFile { contents: Vec<u8> },
    /// Empty success marker for a write request.
    WriteFile,
    /// Empty success marker for a create request.
    CreateFile,
    /// Empty success marker for an append request.
    AppendFile,
    /// Empty success marker for a delete request.
    DeleteFile,
    /// Empty success marker for a rename request.
    RenameFile,
    /// Direct child entries from a list request.
    ListFiles { entries: Vec<ExtensionDataEntry> },
}

/// Machine-readable extension data RPC error kind.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionDataErrorKind {
    /// Requested path or ancestor does not exist.
    NotFound,
    /// Exclusive create requested for an existing file.
    AlreadyExists,
    /// Path failed extension data path validation.
    InvalidPath,
    /// Requested file operation targeted a directory or non-file.
    NotFile,
    /// Requested directory operation targeted a file or non-directory.
    NotDir,
    /// Permission denied by the operating system.
    Permission,
    /// Operation exceeded a harness-enforced resource quota.
    QuotaExceeded,
    /// Any other I/O or harness-side error.
    Io,
}

fn default_extension_data_error_kind() -> ExtensionDataErrorKind {
    ExtensionDataErrorKind::Io
}
/// One direct child returned by an extension data list request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExtensionDataEntry {
    /// Sanitized path relative to the requested scope root.
    pub path: String,
    /// True when this entry is a directory.
    pub is_dir: bool,
    /// File size in bytes for files. Directories use `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub len: Option<u64>,
}

// ---------------------------------------------------------------------------
// Directional protocol envelopes
// ---------------------------------------------------------------------------

/// Messages the harness accepts from connected peers (UI clients and
/// extensions).
///
/// Wire form is `{"message": "<flat_name>", "payload": {...}}`. Event
/// emission is represented by [`HarnessInputMessage::Emit`]; a bare serialized
/// [`Event`] is not a valid harness input message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "message", content = "payload", rename_all = "snake_case")]
pub enum HarnessInputMessage {
    Hello(Hello),
    Subscribe(Subscribe),
    Intercept(Intercept),
    Ready(Ready),
    Disconnect(Disconnect),
    ConfigError(ConfigError),
    Emit(Emit),
    InterceptReply(InterceptReply),
    GetAgentPromptCreated(GetAgentPromptCreated),
    GetRenderedSystemPrompt(GetRenderedSystemPrompt),
    GetRenderedToolDefinitions(GetRenderedToolDefinitions),
    ExtensionDataRequest(ExtensionDataRequest),
}

impl HarnessInputMessage {
    /// Wraps an event emission request with durable-by-default metadata.
    #[must_use]
    pub fn emit(event: Event) -> Self {
        Self::Emit(Emit::new(event))
    }

    /// Wraps an event emission request with explicit transient metadata.
    #[must_use]
    pub fn emit_with_transient(event: Event, transient: bool) -> Self {
        Self::Emit(Emit::with_transient(event, transient))
    }
}

/// Messages the harness sends to connected peers (UI clients and extensions).
///
/// Event delivery is represented by [`HarnessOutputMessage::Deliver`]. The
/// output direction intentionally has no `Emit` variant: peers emit events to
/// the harness, while the harness delivers events to peers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "message", content = "payload", rename_all = "snake_case")]
pub enum HarnessOutputMessage {
    Configure(Configure),
    Disconnect(Disconnect),
    Deliver(EventDelivery),
    InterceptRequest(InterceptRequest),
    AgentPromptCreatedResult(Box<AgentPromptCreatedResult>),
    RenderedSystemPromptResult(Box<RenderedSystemPromptResult>),
    RenderedToolDefinitionsResult(Box<RenderedToolDefinitionsResult>),
    ExtensionDataResult(Box<ExtensionDataResult>),
}

impl HarnessOutputMessage {
    /// Wraps an event for direct delivery of a synthesized current-state
    /// announcement.
    #[must_use]
    pub fn deliver(event: Event) -> Self {
        Self::Deliver(EventDelivery::direct(event))
    }

    /// Wraps a live committed runtime event for delivery.
    #[must_use]
    pub fn deliver_live(recorded_at: UnixMicros, event: Event) -> Self {
        Self::Deliver(EventDelivery::live(recorded_at, event))
    }

    /// Wraps a historical event for replay-marked delivery.
    #[must_use]
    pub fn deliver_replay(recorded_at: UnixMicros, event: Event) -> Self {
        Self::Deliver(EventDelivery::replay(recorded_at, event))
    }

    /// Returns delivery metadata when this output message carries an event.
    #[must_use]
    pub fn as_delivery(&self) -> Option<&EventDelivery> {
        match self {
            Self::Deliver(delivery) => Some(delivery),
            _ => None,
        }
    }

    /// Returns the delivered event when this output message carries one.
    #[must_use]
    pub fn delivered_event(&self) -> Option<&Event> {
        self.as_delivery().map(EventDelivery::event)
    }

    /// Consumes this output message and returns its delivery payload, if any.
    #[must_use]
    pub fn into_delivery(self) -> Option<EventDelivery> {
        match self {
            Self::Deliver(delivery) => Some(delivery),
            _ => None,
        }
    }

    /// Consumes this output message and returns its delivered event, if any.
    #[must_use]
    pub fn into_delivered_event(self) -> Option<Event> {
        self.into_delivery().map(EventDelivery::into_event)
    }
}
