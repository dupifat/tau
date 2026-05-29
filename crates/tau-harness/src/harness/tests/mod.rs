//! Test suite for the harness. Split by concern to mirror the
//! production module layout (interception, replay, skill_tool, dispatch, …).
//!
//! The shared helpers and imports live here so each submodule can
//! pull them in with `use super::*;`.

use std::io::{BufReader, BufWriter};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tau_core::{
    AgentEntry, AgentStore, AgentTree, Connection, ConnectionMetadata, ConnectionOrigin,
    ConnectionSendError, ConnectionSink, RoutedFrame,
};
use tau_proto::{
    AgentPromptCreated, AgentPromptId, AgentPromptQueued, AgentPromptRecalled, AgentPromptSteered,
    CborValue, ContentPart, ContextItem, ContextRole, Disconnect, Event, EventSelector, Frame,
    FrameReader, FrameWriter, Intercept, InterceptAction, InterceptReply, InterceptionPriority,
    Message, MessageItem, NodeId, ProviderResponseFinished, ProviderResponseUpdated,
    StartAgentRequest, Subscribe, ToolCallId, ToolCallItem, ToolName, ToolResult, ToolResultItem,
    ToolResultStatus, ToolSpec, UiPromptDraft, UiPromptSubmitted,
};
use tau_session_inspect::{
    default_session_id, format_session_entry, open_session_store, policy_lines, session_lines,
    session_list_lines,
};
use tempfile::TempDir;

use super::{AgentState, AgentToolCall, HARNESS_CONNECTION_ID, Harness};
use crate::AgentId;
use crate::agent::{AgentTurnState, PendingPrompt};
use crate::daemon::{
    ServeOptions, bind_listener, get_daemon_rendered_system_prompt,
    get_daemon_rendered_tool_definitions, run_daemon_with_echo, run_embedded_message_with_echo,
    send_daemon_message, send_daemon_message_with_trace,
};
use crate::discovery::DiscoveredAgentsFile;
use crate::error::HarnessError;
use crate::event::HarnessEvent;
use crate::model::{
    clamp_effort, efforts_for_model, load_roles, role_infos, select_model_for_role,
    selected_params_for_role, thinking_summaries_for_model, verbosities_for_model,
};
use crate::turn::{PromptSubmission, TurnState};

fn echo_runner(r: UnixStream, w: UnixStream) -> Result<(), String> {
    crate::harness::run_echo_provider(r, w).map_err(|e| e.to_string())
}

fn agent_id_suffix<'a>(agent_id: &'a str, role: &str) -> &'a str {
    let prefix = format!("{role}_");
    agent_id
        .strip_prefix(&prefix)
        .expect("agent id should include the role prefix")
}

fn assert_role_hex_agent_id(agent_id: &str, role: &str) {
    let suffix = agent_id_suffix(agent_id, role);

    assert!(
        (super::AGENT_ID_MIN_SUFFIX_NIBBLES..=super::AGENT_ID_MAX_SUFFIX_NIBBLES)
            .contains(&suffix.len())
    );
    assert!(suffix.chars().all(|ch| matches!(ch, '0'..='9' | 'a'..='f')));
}

#[test]
fn minted_agent_ids_use_minimal_role_prefixed_hex_suffixes() {
    let agent_id = super::mint_agent_id_for_role("engineer");

    assert_role_hex_agent_id(&agent_id, "engineer");
    assert_eq!(
        agent_id_suffix(&agent_id, "engineer").len(),
        super::AGENT_ID_MIN_SUFFIX_NIBBLES
    );
}

#[test]
fn minting_agent_ids_retries_same_size_hex_collisions() {
    // Randomized search can start on a used suffix. The harness should keep
    // grinding the current minimal width before growing the visible id.
    let agent_id = super::mint_available_agent_id_for_role_with(
        "engineer",
        |agent_id| agent_id == "engineer_a",
        |suffix_nibbles, candidate_count| {
            assert_eq!(suffix_nibbles, 1);
            assert_eq!(candidate_count, 16);
            0xa
        },
    );

    assert_eq!(agent_id, "engineer_b");
}

#[test]
fn minting_agent_ids_grows_only_after_shorter_suffixes_are_taken() {
    // If every one-nibble suffix is already reserved in memory or on disk,
    // the harness moves to two nibbles and keeps the id as short as possible.
    let mut searched = Vec::new();
    let agent_id = super::mint_available_agent_id_for_role_with(
        "engineer",
        |agent_id| {
            agent_id
                .strip_prefix("engineer_")
                .is_some_and(|suffix| suffix.len() == 1)
        },
        |suffix_nibbles, candidate_count| {
            searched.push((suffix_nibbles, candidate_count));
            0
        },
    );

    assert_eq!(agent_id, "engineer_00");
    assert_eq!(searched, vec![(1, 16), (2, 256)]);
}

#[test]
fn minting_agent_ids_skips_persisted_agent_dirs() {
    // A suffix already present on disk must stay reserved even when the lazy
    // store has not loaded that agent tree into memory yet.
    let td = TempDir::new().expect("tempdir");
    let agents_dir = td.path().join("agents");
    let store = AgentStore::open_lazy(agents_dir.clone()).expect("agent store");
    let reserved_dir = agents_dir.join("engineer_0");
    std::fs::create_dir_all(&reserved_dir).expect("agent dir");
    std::fs::write(reserved_dir.join("meta.json"), "{}").expect("agent meta");

    let agent_id = super::mint_available_agent_id_for_role_with(
        "engineer",
        |agent_id| store.agent_exists(agent_id),
        |_, _| 0,
    );

    assert_eq!(agent_id, "engineer_1");
}

#[test]
fn render_self_knowledge_config_content_inserts_config_defaults() {
    let rendered = crate::harness::render_self_knowledge_config_content();

    assert!(!rendered.contains("{harness_config}"));
    assert!(!rendered.contains("{ui_config}"));
    assert!(rendered.contains("${XDG_RUNTIME_DIR}/tau/<pid>/"));
    assert!(rendered.contains("session_retention_days: 60"));
    assert!(rendered.contains("show_thinking: true"));
}

#[test]
fn render_self_knowledge_pim_content_inserts_config_defaults() {
    let rendered = crate::harness::render_self_knowledge_pim_content();

    assert!(!rendered.contains("{pim_config}"));
    assert!(rendered.contains("std-pim:"));
    assert!(rendered.contains("calendar:"));
}

fn agent_tree_for_conversation<'a>(h: &'a Harness, cid: &AgentId) -> &'a AgentTree {
    let agent_id = h
        .agents
        .get(cid)
        .and_then(|conv| conv.agent_id.as_deref())
        .expect("conversation has agent id");
    h.agent_store.agent(agent_id).expect("agent tree")
}

fn test_cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn ensure_test_user_agent(h: &mut Harness) -> AgentId {
    let cid = h
        .agents
        .iter()
        .find_map(|(cid, conv)| conv.originator.is_user().then_some(cid.clone()))
        .unwrap_or_else(|| {
            let session_id = h.current_session_id.clone();
            let role = h.selected_role.clone();
            h.create_durable_user_agent(session_id, &role, test_cwd())
        });
    // Most harness unit tests use this helper to focus on tool/provider state,
    // not extension-provided prompt context. Treat the synthetic agent as if
    // registered context providers have already acknowledged it; tests that
    // exercise context readiness drive `session.agent_loaded` explicitly.
    if let Some(agent_id) = h
        .agents
        .get(&cid)
        .and_then(|conv| conv.agent_id.as_deref())
        .map(tau_proto::AgentId::from)
    {
        h.pending_agent_context_ready.remove(&agent_id);
    }
    cid
}

fn test_user_agent(h: &Harness) -> AgentId {
    h.agents
        .iter()
        .find_map(|(cid, conv)| conv.originator.is_user().then_some(cid.clone()))
        .expect("test should create a user agent first")
}

fn durable_agent_id_for_conversation(h: &Harness, cid: &AgentId) -> tau_proto::AgentId {
    h.agents
        .get(cid)
        .and_then(|conv| conv.agent_id.clone())
        .expect("conversation has durable agent id")
        .into()
}

fn default_agent_tree(h: &Harness) -> &AgentTree {
    let cid = test_user_agent(h);
    agent_tree_for_conversation(h, &cid)
}

fn agent_branch_for_conversation<'a>(h: &'a Harness, cid: &AgentId) -> Vec<&'a AgentEntry> {
    let head = h.agents.get(cid).and_then(|conv| conv.head);
    agent_tree_for_conversation(h, cid).branch_from(head)
}

fn default_agent_branch(h: &Harness) -> Vec<&AgentEntry> {
    let cid = test_user_agent(h);
    agent_branch_for_conversation(h, &cid)
}

fn default_agent_node(h: &Harness, id: NodeId) -> &tau_core::AgentNode {
    default_agent_tree(h).node(id).expect("agent node")
}

fn event_log_events(h: &Harness) -> Vec<Event> {
    let mut events = Vec::new();
    let mut seq = tau_proto::EventLogSeq::new(0);
    while let Some(entry) = h.event_log.get_next_from(seq) {
        seq = entry.seq.next();
        events.push(entry.event);
    }
    events
}

fn loaded_agent_events(h: &Harness, session_id: &str) -> Vec<Event> {
    let Some(session) = h.store.session(session_id) else {
        return Vec::new();
    };

    session
        .loaded_agents()
        .into_iter()
        .filter_map(|agent_id| h.agent_store.agent_events(agent_id.as_str()).ok())
        .flatten()
        .map(|entry| entry.event)
        .collect()
}

fn persisted_agent_branch(state_dir: &Path, session_id: &str) -> Vec<AgentEntry> {
    persisted_agent_branches(state_dir, session_id)
        .into_iter()
        .next()
        .expect("loaded agent")
}

fn persisted_agent_branches(state_dir: &Path, session_id: &str) -> Vec<Vec<AgentEntry>> {
    let sessions_dir = tau_config::settings::sessions_dir_of(state_dir);
    let store = open_session_store(&sessions_dir).expect("session store");
    let session = store.session(session_id).expect("session membership");
    let mut agent_store = AgentStore::open(state_dir.join("agents")).expect("agent store");
    session
        .loaded_agents()
        .into_iter()
        .map(|agent_id| {
            let tree = agent_store
                .load_agent(agent_id.as_str())
                .expect("load agent")
                .expect("agent tree");
            tree.current_branch().into_iter().cloned().collect()
        })
        .collect()
}

/// Test-only helper that appends a user message through the harness's normal
/// agent-transcript publish path without driving a provider turn.
fn append_user_message_via_event(h: &mut Harness, session_id: &str, text: &str) {
    assert_eq!(session_id, h.current_session_id.as_str());
    let cid = ensure_test_user_agent(h);
    h.publish_pending_prompt_for_agent(&cid, PendingPrompt::user(text.to_owned()))
        .expect("append user message");
}

fn echo_harness(state_dir: impl Into<PathBuf>) -> Result<Harness, HarnessError> {
    echo_harness_for("s1", state_dir)
}

fn echo_harness_for(
    session_id: &str,
    state_dir: impl Into<PathBuf>,
) -> Result<Harness, HarnessError> {
    let state_dir = state_dir.into();
    let dirs = tau_config::settings::TauDirs {
        config_dir: Some(state_dir.join("config")),
        state_dir: Some(state_dir.join("runtime")),
    };
    echo_harness_with_dirs(session_id, state_dir, dirs)
}

fn echo_harness_with_dirs(
    session_id: &str,
    state_dir: impl Into<PathBuf>,
    dirs: tau_config::settings::TauDirs,
) -> Result<Harness, HarnessError> {
    echo_harness_with_dirs_and_start_reason(
        session_id,
        state_dir,
        dirs,
        tau_proto::SessionStartReason::Initial,
    )
}

fn echo_harness_with_start_reason(
    session_id: &str,
    state_dir: impl Into<PathBuf>,
    start_reason: tau_proto::SessionStartReason,
) -> Result<Harness, HarnessError> {
    let state_dir = state_dir.into();
    let dirs = tau_config::settings::TauDirs {
        config_dir: Some(state_dir.join("config")),
        state_dir: Some(state_dir.join("runtime")),
    };
    echo_harness_with_dirs_and_start_reason(session_id, state_dir, dirs, start_reason)
}

fn echo_harness_with_dirs_and_start_reason(
    session_id: &str,
    state_dir: impl Into<PathBuf>,
    dirs: tau_config::settings::TauDirs,
    start_reason: tau_proto::SessionStartReason,
) -> Result<Harness, HarnessError> {
    fn shell_runner(r: UnixStream, w: UnixStream) -> Result<(), String> {
        tau_ext_shell::run(r, w).map_err(|e| e.to_string())
    }
    let mut h = Harness::new_with_provider(
        state_dir,
        dirs,
        echo_runner,
        vec![crate::harness::InProcessTool {
            name: "shell",
            runner: shell_runner,
        }],
        session_id,
        start_reason,
    )?;
    // Most harness tests use the in-process shell only as a tool provider. Do
    // not let its startup context-provider registration defer unrelated prompt
    // dispatch assertions; readiness-specific tests register providers directly.
    h.agent_context_providers.clear();
    h.pending_agent_context_ready.clear();
    Ok(h)
}

fn quiet_provider_harness(state_dir: impl Into<PathBuf>) -> Result<Harness, HarnessError> {
    quiet_provider_harness_with_start_reason(state_dir, tau_proto::SessionStartReason::Initial)
}

fn quiet_provider_harness_with_start_reason(
    state_dir: impl Into<PathBuf>,
    start_reason: tau_proto::SessionStartReason,
) -> Result<Harness, HarnessError> {
    fn quiet_provider_runner(r: UnixStream, w: UnixStream) -> Result<(), String> {
        fn inner(r: UnixStream, w: UnixStream) -> Result<(), Box<dyn std::error::Error>> {
            let mut reader = FrameReader::new(BufReader::new(r));
            let mut writer = FrameWriter::new(BufWriter::new(w));

            writer.write_frame(&Frame::Message(Message::Hello(tau_proto::Hello {
                protocol_version: tau_proto::PROTOCOL_VERSION,
                client_name: "tau-quiet-provider".into(),
                client_kind: tau_proto::ClientKind::Provider,
            })))?;
            writer.write_frame(&Frame::Event(Event::ProviderModelsUpdated(
                tau_proto::ProviderModelsUpdated {
                    models: vec![tau_proto::ProviderModelInfo {
                        id: "test/model".into(),
                        display_name: Some("Test".to_owned()),
                        default_affinity: 0,
                        context_window: 1_000,
                        efforts: vec![tau_proto::Effort::Medium],
                        verbosities: vec![tau_proto::Verbosity::Medium],
                        thinking_summaries: vec![tau_proto::ThinkingSummary::Auto],
                        supports_compaction: true,
                    }],
                },
            )))?;
            writer.write_frame(&Frame::Message(Message::Ready(tau_proto::Ready {
                message: Some("quiet provider ready".to_owned()),
            })))?;
            writer.flush()?;

            while let Some(frame) = reader.read_frame()? {
                let (_, frame) = frame.peel_log();
                if matches!(frame, Frame::Message(Message::Disconnect(_))) {
                    return Ok(());
                }
            }
            Ok(())
        }

        inner(r, w).map_err(|e| e.to_string())
    }

    let state_dir = state_dir.into();
    let dirs = tau_config::settings::TauDirs {
        config_dir: Some(state_dir.join("config")),
        state_dir: Some(state_dir.join("runtime")),
    };
    Harness::new_with_provider(
        state_dir,
        dirs,
        quiet_provider_runner,
        Vec::new(),
        "s1",
        start_reason,
    )
}

struct TestSink {
    events: Arc<Mutex<Vec<RoutedFrame>>>,
}

impl ConnectionSink for TestSink {
    fn send(&mut self, event: RoutedFrame) -> Result<(), ConnectionSendError> {
        self.events.lock().expect("sink mutex").push(event);
        Ok(())
    }
}

fn connect_test_client(
    h: &mut Harness,
    name: &str,
    kind: tau_proto::ClientKind,
) -> Arc<Mutex<Vec<RoutedFrame>>> {
    let events = Arc::new(Mutex::new(Vec::new()));
    h.bus.connect(Connection::new(
        ConnectionMetadata {
            id: name.into(),
            name: name.to_owned(),
            kind,
            origin: ConnectionOrigin::InMemory,
        },
        Box::new(TestSink {
            events: Arc::clone(&events),
        }),
    ));
    events
}

fn connect_test_tool(h: &mut Harness, name: &str) -> Arc<Mutex<Vec<RoutedFrame>>> {
    connect_test_client(h, name, tau_proto::ClientKind::Tool)
}

/// Pre-seed the per-conversation `AgentThinking` state for tests that
/// bypass `dispatch_prompt_for_agent` and call response handlers
/// directly.
fn seed_agent_thinking(h: &mut Harness, cid: &crate::AgentId, spid: &str) {
    // Tests that bypass prompt dispatch still need the same loaded-agent and
    // session-membership side effects that a real dispatch would establish.
    let agent_id = h
        .ensure_agent_id_for_agent(cid)
        .expect("conversation agent id");
    let conv = h.agents.get_mut(cid).expect("conversation present");
    conv.turn_state = AgentTurnState::AgentThinking {
        agent_prompt_id: spid.into(),
    };
    h.agent_routes.insert(agent_id.clone(), cid.clone());
    h.agent_states.insert(agent_id, AgentState::Active);
}

/// Pre-seed the per-conversation `ToolsRunning` state for tests that
/// bypass the agent-response path and call tool handlers directly.
fn seed_tools_running(h: &mut Harness, cid: &crate::AgentId, remaining: Vec<ToolCallId>) {
    h.agents
        .get_mut(cid)
        .expect("conversation present")
        .turn_state = AgentTurnState::ToolsRunning {
        remaining_calls: remaining,
    };
}

/// Seed the transcript and turn state as if the assistant had just
/// emitted one or more tool calls for this conversation.
fn seed_assistant_tool_round(h: &mut Harness, cid: &crate::AgentId, calls: &[(&str, &str)]) {
    let agent_id = h
        .agents
        .get(cid)
        .and_then(|conv| conv.agent_id.clone())
        .unwrap_or_else(|| "main".to_owned());
    h.publish_for_agent(
        cid,
        Event::ProviderResponseFinished(ProviderResponseFinished {
            agent_prompt_id: "sp-seeded-tools".into(),
            agent_id: agent_id.into(),
            output_items: calls
                .iter()
                .map(|(call_id, tool_name)| {
                    ContextItem::ToolCall(ToolCallItem {
                        call_id: (*call_id).into(),
                        name: ToolName::new(*tool_name),
                        tool_type: tau_proto::ToolType::Function,
                        arguments: CborValue::Map(Vec::new()),
                    })
                })
                .collect(),
            stop_reason: tau_proto::ProviderStopReason::ToolCalls,
            error: None,
            usage: None,
            originator: tau_proto::PromptOriginator::User,
            backend: None,
            provider_response_id: None,
            ws_pool_delta: None,
        }),
    );
    seed_tools_running(
        h,
        cid,
        calls.iter().map(|(call_id, _)| (*call_id).into()).collect(),
    );
}

/// Pumps the harness event loop until the named tool call's result
/// or error is received and handled. Panics on timeout.
fn drive_harness_until_call_completes(h: &mut Harness, target_call_id: &str) {
    let started = Instant::now();
    loop {
        if started.elapsed() >= Duration::from_secs(3) {
            panic!("timed out waiting for {target_call_id} to complete");
        }
        let event =
            h.rx.recv_timeout(Duration::from_secs(1))
                .expect("tool result should arrive");
        match event {
            HarnessEvent::FromConnection {
                connection_id,
                frame,
            } => {
                let is_target = match frame.as_ref() {
                    Frame::Event(Event::ToolResult(r)) => r.call_id.as_str() == target_call_id,
                    Frame::Event(Event::ToolError(e)) => e.call_id.as_str() == target_call_id,
                    _ => false,
                };
                h.handle_extension_event(&connection_id, *frame)
                    .expect("handle");
                if is_target {
                    return;
                }
            }
            HarnessEvent::Disconnected { connection_id } => {
                h.handle_disconnect(&connection_id);
            }
            HarnessEvent::NewClient(_) => {}
            HarnessEvent::Command(command) => h.handle_harness_command(command).expect("handle"),
        }
    }
}

fn drive_harness_until_tool_turn_empty(h: &mut Harness) {
    let started = Instant::now();
    loop {
        if h.tool_turn.is_empty() {
            return;
        }
        if started.elapsed() >= Duration::from_secs(3) {
            panic!("timed out waiting for tool turn to empty");
        }
        let event =
            h.rx.recv_timeout(Duration::from_secs(1))
                .expect("tool result should arrive");
        match event {
            HarnessEvent::FromConnection {
                connection_id,
                frame,
            } => h
                .handle_extension_event(&connection_id, *frame)
                .expect("handle"),
            HarnessEvent::Disconnected { connection_id } => {
                h.handle_disconnect(&connection_id);
            }
            HarnessEvent::NewClient(_) => {}
            HarnessEvent::Command(command) => h.handle_harness_command(command).expect("handle"),
        }
    }
}

fn wait_for_session_unlock(state_dir: &Path, session_id: &str) {
    let sessions_dir = tau_config::settings::sessions_dir_of(state_dir);
    let started = Instant::now();
    loop {
        let locked =
            tau_core::session_is_locked(&sessions_dir, session_id).expect("session lock probe");
        if !locked {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "timed out waiting for session `{session_id}` lock to clear"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Find the conversation id of the outer side conversation (the one
/// whose originator is the delegate extension's first query). Used by
/// the cross-conversation regression test above to disambiguate
/// nested-vs-outer side prompt ids.
fn outer_side_cid_str(h: &Harness) -> &str {
    h.agents
        .iter()
        .find_map(|(cid, conv)| {
            matches!(
                &conv.originator,
                tau_proto::PromptOriginator::Extension { query_id, .. }
                    if query_id == "q-outer"
            )
            .then_some(cid.as_str())
        })
        .unwrap_or("")
}

/// Subscribe a fresh test sink to `tool.delegate_progress` events and
/// hand back its accumulator.
fn collect_event_sink(h: &mut Harness) -> Arc<Mutex<Vec<RoutedFrame>>> {
    let events = connect_test_tool(h, "test-delegate-progress-sink");
    h.bus
        .set_subscriptions(
            "test-delegate-progress-sink",
            vec![tau_proto::EventSelector::Exact(
                tau_proto::EventName::TOOL_DELEGATE_PROGRESS,
            )],
        )
        .expect("subscribe");
    events
}

/// Peel a routed frame to its bus-event payload, unwrapping the
/// `Message::LogEvent` envelope when present. Returns `None` for
/// non-event messages (Hello, Ack, …).
fn peel_inner_event(frame: &Frame) -> Option<&Event> {
    match frame {
        Frame::Event(event) => Some(event),
        Frame::Message(Message::LogEvent(env)) => Some(&env.event),
        Frame::Message(_) => None,
    }
}

fn pop_delegate_progress(
    sink: &Arc<Mutex<Vec<RoutedFrame>>>,
    call_id: &str,
) -> Option<tau_proto::DelegateProgress> {
    let mut events = sink.lock().expect("sink");
    let pos = events.iter().position(|routed| {
        matches!(
            peel_inner_event(&routed.frame),
            Some(Event::ToolDelegateProgress(p)) if p.call_id.as_str() == call_id
        )
    })?;
    let removed = events.remove(pos);
    match removed.frame {
        Frame::Event(Event::ToolDelegateProgress(p)) => Some(p),
        Frame::Message(Message::LogEvent(env)) => match *env.event {
            Event::ToolDelegateProgress(p) => Some(p),
            _ => unreachable!(),
        },
        _ => unreachable!(),
    }
}

fn drain_delegate_progress(
    sink: &Arc<Mutex<Vec<RoutedFrame>>>,
    call_id: &str,
) -> Vec<tau_proto::DelegateProgress> {
    let mut events = sink.lock().expect("sink");
    let mut out = Vec::new();
    events.retain(|routed| match peel_inner_event(&routed.frame) {
        Some(Event::ToolDelegateProgress(p)) if p.call_id.as_str() == call_id => {
            out.push(p.clone());
            false
        }
        _ => true,
    });
    out
}

fn read_raw_prompt_created(h: &Harness, spid: &AgentPromptId) -> AgentPromptCreated {
    let mut cursor = tau_proto::EventLogSeq::new(0);
    loop {
        let entry = h
            .event_log
            .get_next_from(cursor)
            .expect("prompt event in log");
        cursor = entry.seq.next();
        match entry.event {
            Event::AgentPromptCreated(prompt) if &prompt.agent_prompt_id == spid => {
                return prompt;
            }
            _ => {}
        }
    }
}

fn read_prompt_created(h: &Harness, spid: &AgentPromptId) -> AgentPromptCreated {
    let raw = read_raw_prompt_created(h, spid);
    h.read_agent_prompt_created(&raw.session_id, spid)
        .expect("materialized prompt event")
}

fn intercepted_payload(events: &Arc<Mutex<Vec<RoutedFrame>>>) -> (Event, bool) {
    let events = events.lock().expect("events mutex");
    let intercepted = events
        .iter()
        .find_map(|routed| match &routed.frame {
            Frame::Message(Message::InterceptRequest(req)) => Some(req),
            _ => None,
        })
        .expect("intercept request delivered");
    ((*intercepted.event).clone(), intercepted.transient)
}

fn draft_event(text: &str) -> Event {
    Event::UiPromptDraft(UiPromptDraft {
        session_id: "s1".into(),
        text: text.to_owned(),
    })
}

#[test]
fn shell_command_args_middle_shortens_long_first_line() {
    assert_eq!(
        super::shell_command_args(
            "printf 1234567890123456789012345678901234567890\nprintf ignored"
        ),
        "printf 1234567890123┄12345678901234567890"
    );
}

#[test]
fn shell_command_args_keeps_short_first_line() {
    assert_eq!(
        super::shell_command_args("printf 1234567890123"),
        "printf 1234567890123"
    );
}

mod action;
mod dedup;
mod dispatch;
mod format;
mod interception;
mod lifecycle;
mod mode;
mod model;
mod replay;
