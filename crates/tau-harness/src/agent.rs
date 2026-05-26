//! Per-agent runtime state tracked by the harness.
//!
//! An [`Agent`] is one live prompt/tool execution context loaded into the
//! current harness session. The durable transcript lives in `tau-core`'s
//! `AgentTree`; this module stores the harness-owned runtime state layered on
//! top of that transcript: the selected branch head, queued prompts, turn
//! lifecycle, tool progress, and side-agent ancestry used for routing.
//!
//! The harness multiplexes incoming agent and tool events back to the owning
//! agent via two id maps it owns:
//! `prompt_agents: HashMap<AgentPromptId, AgentId>` and
//! `tool_agents: HashMap<ToolCallId, AgentId>`.

use std::collections::VecDeque;

use tau_core::NodeId;
use tau_proto::{
    AgentId, AgentPromptId, ConnectionId, ModelId, ModelParams, PromptMessageClass,
    PromptOriginator, ProviderBackend, SessionId, ToolCallId, ToolChoice, ToolDefinition,
    ToolDisplayStats, ToolExecutionMode,
};

use crate::dedup::ResultDedupMap;

/// Hash the per-request inputs whose drift would invalidate a Codex
/// chain (`previous_response_id`). System prompt, tool list, and model
/// params each appear on the wire on every turn; if they differ from
/// the prior turn the server's reasoning continuity can decohere
/// silently. Used by both [`ChainAnchor::request_fingerprint`] (set
/// when the anchor is minted) and the anchor-validity check before
/// sending the next prompt.
///
/// `tool_choice` is hashed too. It is serialized on the wire by both
/// Responses and Chat Completions backends; carrying a
/// `previous_response_id` across a `tool_choice` flip sends a request
/// whose non-input fields no longer match the anchored response and
/// can silently fall off the provider cache. Non-tool extension side
/// queries therefore preserve `ToolChoice::Auto` and the harness
/// enforces "no tool execution" locally instead of mutating the wire
/// request.
///
/// Domain-separated by a NUL byte between fields so e.g. a system
/// prompt ending in `"]"` can't be confused with the start of the
/// tools JSON. Field serialization failures (impossibly rare on these
/// types) collapse to empty bytes, which just means a mismatch and a
/// safe full-replay fallback.
#[cfg(test)]
pub(crate) fn compute_chain_fingerprint(
    system_prompt: &str,
    tools: &[ToolDefinition],
    model_params: &ModelParams,
    tool_choice: ToolChoice,
) -> [u8; 32] {
    compute_chain_fingerprint_detail(system_prompt, tools, model_params, tool_choice).digest
}

pub(crate) fn compute_chain_fingerprint_detail(
    system_prompt: &str,
    tools: &[ToolDefinition],
    model_params: &ModelParams,
    tool_choice: ToolChoice,
) -> ChainFingerprintDetail {
    let tools_json = serde_json::to_vec(tools).unwrap_or_default();
    let params_json = serde_json::to_vec(model_params).unwrap_or_default();
    let tool_choice_json = serde_json::to_vec(&tool_choice).unwrap_or_default();

    let mut hasher = blake3::Hasher::new();
    hasher.update(system_prompt.as_bytes());
    hasher.update(b"\0tools:");
    hasher.update(&tools_json);
    hasher.update(b"\0params:");
    hasher.update(&params_json);
    hasher.update(b"\0tool_choice:");
    hasher.update(&tool_choice_json);

    ChainFingerprintDetail {
        digest: *hasher.finalize().as_bytes(),
        parts: ChainFingerprintParts {
            system_prompt: hash_bytes(system_prompt.as_bytes()),
            tools: hash_bytes(&tools_json),
            model_params: hash_bytes(&params_json),
            tool_choice: hash_bytes(&tool_choice_json),
        },
    }
}

fn hash_bytes(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ChainFingerprintDetail {
    pub(crate) digest: [u8; 32],
    pub(crate) parts: ChainFingerprintParts,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ChainFingerprintParts {
    pub(crate) system_prompt: [u8; 32],
    pub(crate) tools: [u8; 32],
    pub(crate) model_params: [u8; 32],
    pub(crate) tool_choice: [u8; 32],
}

/// Per-agent turn state. There is no global execution slot — each loaded agent
/// tracks whether its next prompt can be dispatched.
#[derive(Clone, Debug, Default)]
pub(crate) enum AgentTurnState {
    #[default]
    Idle,
    Compacting,
    AgentThinking {
        #[allow(dead_code)]
        agent_prompt_id: AgentPromptId,
    },
    ToolsRunning {
        remaining_calls: Vec<ToolCallId>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PendingCancel {
    pub(crate) reason: String,
}

/// One loaded agent tracked by the harness.
///
/// The user's main interactive agent is always present while the harness runs.
/// Additional agents may be loaded for extension side work, delegated tasks, or
/// compaction flows, and can later be removed from live runtime state while
/// leaving their durable transcripts intact.
#[derive(Debug)]
pub(crate) struct Agent {
    /// Owning agent id. Duplicates the key in the harness's agent map, but
    /// pinning it on the struct itself
    /// lets future code carry a `&Agent` without also threading
    /// the id through every call site.
    #[allow(dead_code)]
    pub(crate) id: AgentId,
    pub(crate) session_id: SessionId,
    pub(crate) originator: PromptOriginator,
    /// Local cursor — where the *next* transcript event for this agent
    /// should be parented in the owning agent tree. The tree's own `head`
    /// is whichever loaded agent appended last; this field is what
    /// `publish_for_agent` snaps the tree head back to before
    /// emitting an event for this agent.
    pub(crate) head: Option<NodeId>,
    /// For [`PromptOriginator::Extension`] agents: the
    /// connection id of the extension that issued the
    /// [`tau_proto::StartAgentRequest`], so the harness knows where to
    /// route the matching [`tau_proto::StartAgentResult`].
    pub(crate) source_connection: Option<ConnectionId>,
    /// Agent prompt id of the prompt currently in flight for this agent, or
    /// `None` if nothing is pending.
    pub(crate) in_flight_prompt: Option<AgentPromptId>,
    /// Per-agent prompt queue: prompts waiting to be dispatched once this
    /// agent's `turn_state` returns to `Idle`. Other loaded agents dispatch
    /// independently; the provider extension
    /// serializes its own consumption of `AgentPromptCreated`.
    pub(crate) pending_prompts: VecDeque<PendingPrompt>,
    /// Pending user/control-plane request to stop this conversation at
    /// the next stable turn boundary. Stored like queued prompts so
    /// races between provider responses and UI cancel events are
    /// resolved by the conversation state machine instead of by the UI
    /// boundary.
    pub(crate) pending_cancel: Option<PendingCancel>,
    /// Most recent materialized prompt emitted for this conversation.
    /// The next prompt can reference its message prefix instead of
    /// repeating the full conversation history.
    pub(crate) last_prompt_id: Option<AgentPromptId>,
    /// Correlation tag carried in by a [`tau_proto::UiPromptSubmitted`]
    /// and copied onto the next [`tau_proto::AgentPromptCreated`] this
    /// conversation emits. Cleared once consumed. Currently only set
    /// for the synchronous dispatch path; queued prompts drop the tag,
    /// since the queue stores text only.
    pub(crate) next_ctx_id: Option<String>,
    pub(crate) turn_state: AgentTurnState,
    /// For side agents spawned by a tool-implementing extension
    /// (currently just `delegate`): the parent agent's tool call id
    /// that this conversation is fulfilling. Lets the harness emit
    /// [`tau_proto::DelegateProgress`] under that call id as the
    /// sub-agent runs. `None` for the default agent and for
    /// non-tool ext-queries (e.g. notifications' idle summary).
    pub(crate) parent_tool_call_id: Option<ToolCallId>,
    /// Direct parent agent resolved when this side agent is
    /// spawned. Kept alongside `parent_tool_call_id` because the tool-call
    /// routing map can be cleared before teardown needs to hand background
    /// completions back to the parent.
    pub(crate) parent_agent_id: Option<AgentId>,
    /// Display name supplied by the parent agent for the delegated
    /// task, surfaced in the UI alongside `parent_tool_call_id`. Only
    /// set when `parent_tool_call_id` is.
    pub(crate) task_name: Option<String>,
    /// Line and byte stats for the user-provided delegate prompt.
    /// Excludes any hidden prefix added by the delegate extension.
    pub(crate) delegate_input_stats: ToolDisplayStats,
    /// Agent role used for this conversation. `None` means the conversation
    /// follows the harness's globally selected interactive role.
    pub(crate) role: Option<String>,
    /// Stable id assigned when this conversation first starts an agent turn.
    pub(crate) agent_id: Option<String>,
    /// Scheduling mode requested for this delegate side agent. `None`
    /// for the default agent and non-tool side agents.
    pub(crate) delegate_execution_mode: Option<ToolExecutionMode>,
    /// Number of tool calls currently in flight on this conversation.
    pub(crate) tools_in_flight: u32,
    /// Cumulative tool calls this conversation has started (in-flight
    /// + completed). Used as the `total` in `DelegateProgress`.
    pub(crate) tools_total: u32,
    /// Most recent input-token count this agent's agent
    /// reported on a finished response. Used for `DelegateProgress`.
    pub(crate) context_input_tokens: Option<u64>,
    /// Most recent percent-of-context-window this conversation's
    /// agent has used. Computed from `context_input_tokens` and the
    /// model's window size; `None` when the window is unknown.
    pub(crate) context_percent_used: Option<u8>,
    /// Stateful-chain anchor for backends that support it (currently
    /// the OpenAI Codex Responses API). Set when an agent reports a
    /// `response_id` on the previous finished turn; consumed by the
    /// next `send_prompt_to_agent_for` as a hint that the upstream
    /// call can chain off the prior turn instead of replaying the
    /// full transcript. `None` initially, after the selected role
    /// resolves to a different model, or after an edit / error
    /// invalidates the chain.
    pub(crate) chain_anchor: Option<ChainAnchor>,
    /// Per-conversation map from tool-result-content hash to the first
    /// `call_id` on this branch that produced that content. Consulted
    /// at intake of every `ToolResult` / `ToolError` to collapse a
    /// duplicate's payload into a short pointer that refers back to
    /// the original. Branch-scoped: rebuilt from
    /// [`Agent::head`] whenever the cursor moves
    /// non-linearly. See `crate::dedup` for the full rationale.
    pub(crate) result_dedup: ResultDedupMap,
}

/// See [`Agent::chain_anchor`].
#[derive(Clone, Debug)]
pub(crate) struct ChainAnchor {
    /// `response.id` returned by the provider on the most recent
    /// successful turn for this conversation.
    pub(crate) response_id: String,
    /// The agent's tree cursor at the moment the anchor was
    /// captured (after the finished response was folded). The chain
    /// is valid only while the current `head` descends from this
    /// node — if a `UiNavigateTree` jumps to a different branch, the
    /// next send detects the mismatch and drops the anchor.
    pub(crate) head: Option<NodeId>,
    /// Model id that produced `response_id`. Switching models busts
    /// the chain even if the tree position is unchanged.
    pub(crate) model: ModelId,
    /// Number of assembled `ConversationMessage`s in the conversation
    /// at the moment the anchor was captured. The next send slices
    /// `messages[message_count..]` to get the new content the upstream
    /// API hasn't seen yet.
    pub(crate) message_count: usize,
    /// Backend that produced `response_id`. The next prompt reuses
    /// this verbatim in `previous_response_candidate` so the agent can
    /// decide whether the candidate is compatible with the chosen
    /// transport and provider connection state.
    pub(crate) backend: ProviderBackend,
    /// Blake3 fingerprint of `(system_prompt, tools, model_params,
    /// tool_choice)` as observed when the anchor was minted. Codex rejects
    /// (or silently misinterprets) a chained request whose non-input fields
    /// drift from the prior turn, so the next send re-hashes the same
    /// inputs and drops the anchor on mismatch — catching the divergence
    /// before the round-trip rather than after.
    pub(crate) request_fingerprint: [u8; 32],
    /// Per-field hashes for the same provider-visible request fields.
    /// Only used for diagnostics when the aggregate fingerprint
    /// mismatches, so the next cache diagnostic says which field drifted.
    pub(crate) request_fingerprint_parts: ChainFingerprintParts,
}

/// Where a queued prompt came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PendingPromptSource {
    /// A normal user or harness steering prompt.
    General,
    /// A prompt created from an `agent.message_received` delivery.
    AgentMessageReceived,
}

/// A queued prompt plus its user/internal classification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PendingPrompt {
    /// Prompt text to fold into the conversation.
    pub(crate) text: String,
    /// Whether this queued prompt is visible user text or hidden internal text.
    pub(crate) message_class: PromptMessageClass,
    /// Source marker for lifecycle decisions that must not confuse internal
    /// prompts.
    pub(crate) source: PendingPromptSource,
}

impl From<String> for PendingPrompt {
    fn from(text: String) -> Self {
        Self::user(text)
    }
}

impl PartialEq<str> for PendingPrompt {
    fn eq(&self, other: &str) -> bool {
        self.text == other
    }
}

impl PartialEq<&str> for PendingPrompt {
    fn eq(&self, other: &&str) -> bool {
        self.text == *other
    }
}

impl PendingPrompt {
    /// Create a user-visible queued prompt.
    pub(crate) fn user(text: String) -> Self {
        Self {
            text,
            message_class: PromptMessageClass::User,
            source: PendingPromptSource::General,
        }
    }

    /// Create a hidden internal queued prompt.
    pub(crate) fn internal(text: String) -> Self {
        Self {
            text,
            message_class: PromptMessageClass::Internal,
            source: PendingPromptSource::General,
        }
    }

    /// Create a hidden queued prompt from an `agent.message_received` delivery.
    pub(crate) fn agent_message_received(text: String) -> Self {
        Self {
            text,
            message_class: PromptMessageClass::Internal,
            source: PendingPromptSource::AgentMessageReceived,
        }
    }

    /// Whether this prompt should be hidden from user-facing UI and metadata.
    #[must_use]
    pub(crate) fn is_internal(&self) -> bool {
        self.message_class.is_internal()
    }

    /// Whether this prompt came from an `agent.message_received` delivery.
    #[must_use]
    pub(crate) fn is_agent_message_received(&self) -> bool {
        self.source == PendingPromptSource::AgentMessageReceived
    }
}

impl Agent {
    pub(crate) fn new(
        id: AgentId,
        session_id: SessionId,
        originator: PromptOriginator,
        head: Option<NodeId>,
        source_connection: Option<ConnectionId>,
    ) -> Self {
        Self {
            id,
            session_id,
            originator,
            head,
            source_connection,
            in_flight_prompt: None,
            pending_prompts: VecDeque::new(),
            pending_cancel: None,
            last_prompt_id: None,
            next_ctx_id: None,
            turn_state: AgentTurnState::Idle,
            parent_tool_call_id: None,
            parent_agent_id: None,
            task_name: None,
            delegate_input_stats: ToolDisplayStats::default(),
            role: None,
            agent_id: None,
            delegate_execution_mode: None,
            tools_in_flight: 0,
            tools_total: 0,
            context_input_tokens: None,
            context_percent_used: None,
            chain_anchor: None,
            result_dedup: ResultDedupMap::new(),
        }
    }
}

#[cfg(test)]
mod tests;
