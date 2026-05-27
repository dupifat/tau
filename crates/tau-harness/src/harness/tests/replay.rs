use super::*;

fn assistant_output(text: &str) -> Vec<tau_proto::ContextItem> {
    vec![tau_proto::ContextItem::Message(tau_proto::MessageItem {
        role: tau_proto::ContextRole::Assistant,
        content: vec![tau_proto::ContentPart::Text {
            text: text.to_owned(),
        }],
        phase: None,
    })]
}

fn provider_response_contains_text(finished: &ProviderResponseFinished, needle: &str) -> bool {
    finished.output_items.iter().any(|item| {
        matches!(
            item,
            tau_proto::ContextItem::Message(tau_proto::MessageItem { content, .. })
                if content.iter().any(|part| {
                    matches!(part, tau_proto::ContentPart::Text { text } if text.contains(needle))
                })
        )
    })
}

fn response_with_tool_calls(call_ids: &[&str]) -> ProviderResponseFinished {
    ProviderResponseFinished {
        agent_prompt_id: "sp-restored-tools".into(),
        agent_id: "main".into(),
        output_items: call_ids
            .iter()
            .map(|call_id| {
                ContextItem::ToolCall(ToolCallItem {
                    call_id: (*call_id).into(),
                    name: ToolName::new("read"),
                    tool_type: tau_proto::ToolType::Function,
                    arguments: CborValue::Null,
                })
            })
            .collect(),
        stop_reason: tau_proto::ProviderStopReason::ToolCalls,
        originator: tau_proto::PromptOriginator::User,
        usage: None,
        backend: None,
        provider_response_id: None,
        ws_pool_delta: None,
    }
}

fn successful_tool_result(call_id: &str) -> ToolResult {
    ToolResult {
        call_id: call_id.into(),
        tool_name: ToolName::new("read"),
        tool_type: tau_proto::ToolType::Function,
        result: CborValue::Text(format!("result for {call_id}")),
        kind: tau_proto::ToolResultKind::Final,
        display: None,
        originator: tau_proto::PromptOriginator::User,
    }
}

fn seed_restored_tool_round(state_dir: &Path, call_ids: &[&str], completed_call_ids: &[&str]) {
    let sessions_dir = tau_config::settings::sessions_dir_of(state_dir);
    let mut store = tau_core::SessionStore::open(&sessions_dir).expect("session store");
    store
        .append_session_event(
            "s1",
            None,
            Event::SessionAgentLoaded(tau_proto::SessionAgentLoaded {
                session_id: "s1".into(),
                agent_id: "main".into(),
            }),
        )
        .expect("seed session membership");
    let mut agent_store =
        tau_core::AgentStore::open(state_dir.join("agents")).expect("agent store");
    agent_store
        .append_agent_event(
            "main",
            None,
            Event::AgentPromptSubmitted(tau_proto::AgentPromptSubmitted {
                agent_id: "main".into(),
                text: "before restart".to_owned(),
                message_class: tau_proto::PromptMessageClass::User,
                originator: tau_proto::PromptOriginator::User,
                ctx_id: None,
            }),
        )
        .expect("seed user prompt");
    agent_store
        .append_agent_event(
            "main",
            None,
            Event::ProviderResponseFinished(response_with_tool_calls(call_ids)),
        )
        .expect("seed assistant tool calls");
    for call_id in completed_call_ids {
        agent_store
            .append_agent_event(
                "main",
                None,
                Event::ProviderToolResult(successful_tool_result(call_id)),
            )
            .expect("seed completed tool call");
    }
}

fn seed_restored_tool_round_for_agent(
    state_dir: &Path,
    session_id: &str,
    agent_id: &str,
    call_ids: &[&str],
    completed_call_ids: &[&str],
) {
    let sessions_dir = tau_config::settings::sessions_dir_of(state_dir);
    let mut store = tau_core::SessionStore::open(&sessions_dir).expect("session store");
    store
        .append_session_event(
            session_id,
            None,
            Event::SessionAgentLoaded(tau_proto::SessionAgentLoaded {
                session_id: session_id.into(),
                agent_id: agent_id.into(),
            }),
        )
        .expect("seed session membership");
    let mut agent_store =
        tau_core::AgentStore::open(state_dir.join("agents")).expect("agent store");
    agent_store
        .append_agent_event(
            agent_id,
            None,
            Event::AgentPromptSubmitted(tau_proto::AgentPromptSubmitted {
                agent_id: agent_id.into(),
                text: format!("before restart for {agent_id}"),
                message_class: tau_proto::PromptMessageClass::User,
                originator: tau_proto::PromptOriginator::User,
                ctx_id: None,
            }),
        )
        .expect("seed user prompt");
    agent_store
        .append_agent_event(
            agent_id,
            None,
            Event::ProviderResponseFinished(ProviderResponseFinished {
                agent_prompt_id: format!("sp-{agent_id}").into(),
                agent_id: agent_id.into(),
                ..response_with_tool_calls(call_ids)
            }),
        )
        .expect("seed assistant tool calls");
    for call_id in completed_call_ids {
        agent_store
            .append_agent_event(
                agent_id,
                None,
                Event::ProviderToolResult(successful_tool_result(call_id)),
            )
            .expect("seed completed tool call");
    }
}

fn provider_tool_errors(h: &Harness, call_id: &str) -> Vec<tau_proto::ToolError> {
    loaded_agent_events(h, "s1")
        .into_iter()
        .filter_map(|event| match event {
            Event::ProviderToolError(error) if error.call_id.as_str() == call_id => Some(error),
            _ => None,
        })
        .collect()
}

fn prompt_tool_result<'a>(
    prompt: &'a AgentPromptCreated,
    call_id: &str,
) -> Option<&'a ToolResultItem> {
    prompt.context_items.iter().find_map(|item| match item {
        ContextItem::ToolResult(result) if result.call_id.as_str() == call_id => Some(result),
        _ => None,
    })
}

/// Regression: a cold resume used to leave the restored branch ending in an
/// assistant tool call with no matching tool result. The next provider prompt
/// then replayed an orphan tool call. Resume must close that foreground call
/// before the user can extend the branch.
#[test]
fn resume_repairs_unresolved_tool_call_before_next_prompt_context() {
    let td = TempDir::new().expect("tempdir");
    let sp = td.path().join("state");
    seed_restored_tool_round(&sp, &["interrupted-call"], &[]);

    let mut h = echo_harness_with_start_reason("s1", &sp, tau_proto::SessionStartReason::Resume)
        .expect("resume");

    let errors = provider_tool_errors(&h, "interrupted-call");
    assert_eq!(errors.len(), 1);
    assert!(errors[0].message.contains("tau_internal: true"));
    assert!(errors[0].message.contains("Side effects may have occurred"));

    append_user_message_via_event(&mut h, "s1", "after restart");
    let spid = h.send_prompt_to_agent("s1");
    let prompt = read_prompt_created(&h, &spid);
    let repaired = prompt_tool_result(&prompt, "interrupted-call")
        .expect("synthetic tool result should be in provider context");
    assert!(matches!(repaired.status, ToolResultStatus::Error { .. }));

    h.shutdown().expect("shutdown");
}

/// Regression: a parallel tool round can be partly complete when the process
/// dies. Resume must preserve completed calls and synthesize errors only for
/// the missing foreground calls so the provider sees one balanced round.
#[test]
fn resume_repairs_only_missing_call_in_partial_parallel_round() {
    let td = TempDir::new().expect("tempdir");
    let sp = td.path().join("state");
    seed_restored_tool_round(&sp, &["done-call", "missing-call"], &["done-call"]);

    let mut h = echo_harness_with_start_reason("s1", &sp, tau_proto::SessionStartReason::Resume)
        .expect("resume");

    assert!(provider_tool_errors(&h, "done-call").is_empty());
    assert_eq!(provider_tool_errors(&h, "missing-call").len(), 1);

    append_user_message_via_event(&mut h, "s1", "after restart");
    let spid = h.send_prompt_to_agent("s1");
    let prompt = read_prompt_created(&h, &spid);
    let completed = prompt_tool_result(&prompt, "done-call")
        .expect("completed tool result should remain in provider context");
    let repaired = prompt_tool_result(&prompt, "missing-call")
        .expect("missing tool result should be synthesized in provider context");
    assert!(matches!(completed.status, ToolResultStatus::Success));
    assert!(matches!(repaired.status, ToolResultStatus::Error { .. }));

    h.shutdown().expect("shutdown");
}

/// Regression: the resume repair writes durable events. A later cold resume
/// must see the already-closed tool round and avoid appending another synthetic
/// error for the same call.
#[test]
fn repeated_resume_does_not_duplicate_synthetic_tool_errors() {
    let td = TempDir::new().expect("tempdir");
    let sp = td.path().join("state");
    seed_restored_tool_round(&sp, &["interrupted-once"], &[]);

    {
        let mut h =
            echo_harness_with_start_reason("s1", &sp, tau_proto::SessionStartReason::Resume)
                .expect("first resume");
        assert_eq!(provider_tool_errors(&h, "interrupted-once").len(), 1);
        h.shutdown().expect("shutdown");
    }
    wait_for_session_unlock(&sp, "s1");

    {
        let mut h =
            echo_harness_with_start_reason("s1", &sp, tau_proto::SessionStartReason::Resume)
                .expect("second resume");
        assert_eq!(provider_tool_errors(&h, "interrupted-once").len(), 1);
        h.shutdown().expect("shutdown");
    }
}

#[test]
fn resume_repairs_unresolved_tool_call_on_non_default_loaded_agent() {
    let td = TempDir::new().expect("tempdir");
    let sp = td.path().join("state");
    seed_restored_tool_round_for_agent(&sp, "s1", "aaa_agent", &[], &[]);
    seed_restored_tool_round_for_agent(&sp, "s1", "zzz_agent", &["side-interrupted-call"], &[]);

    let mut h = echo_harness_with_start_reason("s1", &sp, tau_proto::SessionStartReason::Resume)
        .expect("resume");

    let errors = provider_tool_errors(&h, "side-interrupted-call");
    assert_eq!(errors.len(), 1);
    assert!(errors[0].message.contains("Side effects may have occurred"));

    h.shutdown().expect("shutdown");
}

#[test]
fn late_joining_ui_client_receives_replayed_agent_message_exact_selector() {
    let td = TempDir::new().expect("tempdir");
    let sp = td.path().join("state");
    let mut h = echo_harness(&sp).expect("start");

    h.store
        .append_session_event(
            "s1",
            Some(HARNESS_CONNECTION_ID.into()),
            Event::SessionAgentLoaded(tau_proto::SessionAgentLoaded {
                session_id: "s1".into(),
                agent_id: "agent-1".into(),
            }),
        )
        .expect("seed session membership");
    h.agent_store
        .append_agent_event(
            "agent-1",
            Some(HARNESS_CONNECTION_ID.into()),
            Event::AgentMessageSent(tau_proto::AgentMessageSent {
                message_id: "test-message".into(),
                sender_id: "agent-1".to_owned().into(),
                recipient: tau_proto::AgentMessageRecipient::User,
                message: "persisted hello".to_owned(),
            }),
        )
        .expect("seed agent message");

    let (server_end, client_end) = UnixStream::pair().expect("pair");
    client_end
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("read timeout");
    h.accept_client(server_end).expect("accept");
    let ui_conn = h
        .bus
        .connections()
        .into_iter()
        .find(|c| c.name == "socket-ui")
        .expect("ui connection")
        .id
        .to_string();

    h.handle_client_event(
        &ui_conn,
        Frame::Message(Message::Subscribe(Subscribe {
            selectors: vec![EventSelector::Exact(
                tau_proto::EventName::AGENT_MESSAGE_SENT,
            )],
        })),
    )
    .expect("subscribe");

    let mut reader = FrameReader::new(BufReader::new(client_end));
    let mut got_message = false;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && !got_message {
        let Ok(Some(frame)) = reader.read_frame() else {
            break;
        };
        let (_log_id, inner) = frame.peel_log();
        got_message = matches!(
            inner,
            Frame::Event(Event::AgentMessageSent(message))
                if message.sender_id == "agent-1"
                    && message.recipient == tau_proto::AgentMessageRecipient::User
                    && message.message == "persisted hello"
        );
    }

    assert!(got_message, "late UI should replay durable agent messages");

    h.shutdown().expect("shutdown");
}

#[test]
fn late_joining_ui_client_receives_replayed_session_events() {
    let td = TempDir::new().expect("tempdir");
    let sp = td.path().join("state");
    let mut h = echo_harness(&sp).expect("start");

    h.send_user_message("s1", "hello replay", None)
        .expect("send message");

    let events = loaded_agent_events(&h, "s1");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::AgentPromptSubmitted(_))),
        "user prompt should be in a durable loaded-agent event log"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::ProviderResponseFinished(_))),
        "final agent response should be in a durable loaded-agent event log"
    );
    assert!(
        events.iter().all(|event| !event.defaults_to_transient()),
        "transient events must not be persisted"
    );

    let (server_end, client_end) = UnixStream::pair().expect("pair");
    client_end
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("read timeout");
    h.accept_client(server_end).expect("accept");
    let ui_conn = h
        .bus
        .connections()
        .into_iter()
        .find(|c| c.name == "socket-ui")
        .expect("ui connection")
        .id
        .to_string();

    h.handle_client_event(
        &ui_conn,
        Frame::Message(Message::Subscribe(Subscribe {
            selectors: vec![
                EventSelector::Prefix("session.".to_owned()),
                EventSelector::Prefix("agent.".to_owned()),
                EventSelector::Prefix("provider.".to_owned()),
            ],
        })),
    )
    .expect("subscribe");

    let mut reader = FrameReader::new(BufReader::new(client_end));
    let mut got_session_started = false;
    let mut got_prompt = false;
    let mut got_response = false;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && !(got_session_started && got_prompt && got_response) {
        let Ok(Some(frame)) = reader.read_frame() else {
            break;
        };
        let (_log_id, inner) = frame.peel_log();
        match inner {
            Frame::Event(Event::SessionStarted(started)) if started.session_id.as_str() == "s1" => {
                got_session_started = true;
            }
            Frame::Event(Event::AgentPromptSubmitted(prompt)) if prompt.text == "hello replay" => {
                got_prompt = true;
            }
            Frame::Event(Event::ProviderResponseFinished(finished))
                if finished.output_items.iter().any(|item| {
                    matches!(
                        item,
                        tau_proto::ContextItem::Message(tau_proto::MessageItem { content, .. })
                            if matches!(&content[0], tau_proto::ContentPart::Text { text }
                                if text.contains("hello replay"))
                    )
                }) =>
            {
                got_response = true;
            }
            _ => {}
        }
    }

    assert!(
        got_session_started,
        "late UI should replay current session start"
    );
    assert!(got_prompt, "late UI should replay prior user prompt");
    assert!(got_response, "late UI should replay prior agent response");

    h.shutdown().expect("shutdown");
}

/// Regression: extension subscriptions are live-only even for durable events
/// that a late UI client would replay. This protects live-only extensions such
/// as std-notifications from replaying sounds or idle work for old turns.
#[test]
fn extension_subscribe_receives_no_replayed_past_events() {
    let td = TempDir::new().expect("tempdir");
    let sp = td.path().join("state");
    let mut h = echo_harness(&sp).expect("start");

    let past_text = "past extension replay guard";
    h.send_user_message("s1", past_text, None)
        .expect("send past message");

    let durable_events = loaded_agent_events(&h, "s1");
    assert!(
        durable_events.iter().any(|event| {
            matches!(event, Event::ProviderResponseFinished(finished)
                if provider_response_contains_text(finished, past_text))
        }),
        "test setup: past provider response should be durable and eligible for UI replay",
    );

    let extension_events = connect_test_tool(&mut h, "live-only-extension");
    h.handle_extension_message(
        "live-only-extension",
        Message::Subscribe(Subscribe {
            selectors: vec![EventSelector::Exact(
                tau_proto::EventName::PROVIDER_RESPONSE_FINISHED,
            )],
        }),
    )
    .expect("extension subscribe");

    {
        let events = extension_events.lock().expect("sink");
        assert!(
            events.is_empty(),
            "extension subscribe must not replay the past provider response",
        );
    }

    let live_text = "future live extension event";
    h.send_user_message("s1", live_text, None)
        .expect("send live message");

    {
        let events = extension_events.lock().expect("sink");
        assert!(
            events.iter().any(|routed| {
                matches!(
                    peel_inner_event(&routed.frame),
                    Some(Event::ProviderResponseFinished(finished))
                        if provider_response_contains_text(finished, live_text)
                )
            }),
            "extension should receive future live provider responses",
        );
        assert!(
            events.iter().all(|routed| {
                !matches!(
                    peel_inner_event(&routed.frame),
                    Some(Event::ProviderResponseFinished(finished))
                        if provider_response_contains_text(finished, past_text)
                )
            }),
            "extension must not receive replayed past provider responses",
        );
    }

    h.shutdown().expect("shutdown");
}

#[test]
fn queued_and_recalled_prompt_lifecycle_is_not_durable() {
    // Queue/recall state is process-local scheduler/UI state. Persisting only
    // part of that lifecycle makes cold resume resurrect prompts that already
    // dispatched or were recalled, so both events must remain transient.
    let td = TempDir::new().expect("tempdir");
    let sp = td.path().join("state");
    let mut h = echo_harness(&sp).expect("start");
    let cid = ensure_test_user_agent(&mut h);
    let agent_id = h
        .ensure_agent_id_for_agent(&cid)
        .expect("default conversation has an agent id");

    h.publish_event(
        None,
        Event::AgentPromptQueued(AgentPromptQueued {
            agent_id: agent_id.clone().into(),
            text: "edit me".to_owned(),
            message_class: tau_proto::PromptMessageClass::User,
        }),
    );
    h.publish_event(
        None,
        Event::AgentPromptRecalled(AgentPromptRecalled {
            agent_id: agent_id.clone().into(),
            text: "edit me".to_owned(),
        }),
    );

    let events = h.store.session_events("s1").expect("session events");
    assert!(
        events.iter().all(|entry| !matches!(
            entry.event,
            Event::AgentPromptQueued(_) | Event::AgentPromptRecalled(_)
        )),
        "cold replay must not resurrect transient queue lifecycle events: {events:?}"
    );

    h.shutdown().expect("shutdown");
}

#[test]
fn late_joining_ui_client_replays_final_but_not_stale_queued_session_events() {
    let td = TempDir::new().expect("tempdir");
    let sp = td.path().join("state");
    let mut h = echo_harness(&sp).expect("start");

    let spid: AgentPromptId = "sp-replay".into();
    let cid = ensure_test_user_agent(&mut h);
    let agent_id = h
        .ensure_agent_id_for_agent(&cid)
        .expect("default conversation has an agent id");
    let session_id = h.agents[&cid].session_id.clone();
    h.prompt_agents.insert(spid.clone(), cid.clone());
    h.publish_event(
        None,
        Event::AgentPromptQueued(AgentPromptQueued {
            agent_id: agent_id.clone().into(),
            text: "queued for reconnect".to_owned(),
            message_class: tau_proto::PromptMessageClass::User,
        }),
    );
    h.publish_event(
        None,
        Event::AgentPromptCreated(AgentPromptCreated {
            agent_id: agent_id.clone().into(),
            agent_prompt_id: spid.clone(),
            session_id: session_id.clone(),
            system_prompt: String::new(),
            context_items: Vec::new(),
            tools: Vec::new(),
            tools_ref: None,
            model: None,
            model_params: Default::default(),
            tool_choice: Default::default(),
            originator: Default::default(),
            previous_response_candidate: None,
            share_user_cache_key: false,
            ctx_id: None,
        }),
    );
    h.publish_event(
        None,
        Event::ProviderResponseUpdated(ProviderResponseUpdated {
            agent_prompt_id: spid.clone(),
            text: "partial".to_owned(),
            thinking: None,
            originator: Default::default(),
        }),
    );
    h.publish_event(
        None,
        Event::AgentCompactionStarted(tau_proto::AgentCompactionStarted {
            agent_id: agent_id.clone().into(),
            originator: tau_proto::PromptOriginator::User,
            original_input_tokens: None,
        }),
    );
    h.publish_event(
        None,
        Event::AgentCompacted(tau_proto::AgentCompacted {
            agent_id: agent_id.clone().into(),
            originator: tau_proto::PromptOriginator::User,
            original_input_tokens: None,
            compacted_input_tokens: None,
            replacement_window: assistant_output("Agent compacted."),
        }),
    );
    h.publish_event(
        None,
        Event::ProviderResponseFinished(ProviderResponseFinished {
            agent_prompt_id: spid,
            agent_id: agent_id.clone().into(),
            output_items: assistant_output("final"),
            stop_reason: tau_proto::ProviderStopReason::EndTurn,
            originator: Default::default(),
            usage: None,
            backend: None,
            provider_response_id: None,
            ws_pool_delta: None,
        }),
    );

    let (server_end, client_end) = UnixStream::pair().expect("pair");
    client_end
        .set_read_timeout(Some(Duration::from_millis(200)))
        .expect("read timeout");
    h.accept_client(server_end).expect("accept");
    let ui_conn = h
        .bus
        .connections()
        .into_iter()
        .find(|c| c.name == "socket-ui")
        .expect("ui connection")
        .id
        .to_string();

    h.handle_client_event(
        &ui_conn,
        Frame::Message(Message::Subscribe(Subscribe {
            selectors: vec![
                EventSelector::Prefix("agent.".to_owned()),
                EventSelector::Prefix("provider.".to_owned()),
            ],
        })),
    )
    .expect("subscribe");

    let mut reader = FrameReader::new(BufReader::new(client_end));
    let mut replayed = Vec::new();
    while let Ok(Some(frame)) = reader.read_frame() {
        let (_log_id, inner) = frame.peel_log();
        if let Frame::Event(event) = inner {
            replayed.push(event.name());
        }
    }

    assert!(replayed.contains(&tau_proto::EventName::PROVIDER_RESPONSE_FINISHED));
    assert!(replayed.contains(&tau_proto::EventName::AGENT_COMPACTED));
    assert!(!replayed.contains(&tau_proto::EventName::AGENT_COMPACTION_STARTED));
    assert!(!replayed.contains(&tau_proto::EventName::AGENT_PROMPT_QUEUED));
    assert!(!replayed.contains(&tau_proto::EventName::AGENT_PROMPT_CREATED));
    assert!(!replayed.contains(&tau_proto::EventName::PROVIDER_RESPONSE_UPDATED));

    h.shutdown().expect("shutdown");
}

#[test]
fn late_joining_ui_client_replays_only_current_active_queue() {
    // Queue lifecycle events are transient; durable replay must not resurrect
    // prompts that already dispatched. A late UI still needs the harness's
    // current in-memory queue so it can show prompts that are actually pending.
    let td = TempDir::new().expect("tempdir");
    let sp = td.path().join("state");
    let mut h = echo_harness(&sp).expect("start");
    let cid = ensure_test_user_agent(&mut h);
    let agent_id = h
        .ensure_agent_id_for_agent(&cid)
        .expect("default conversation has an agent id");
    h.agents
        .get_mut(&cid)
        .expect("default conversation")
        .pending_prompts
        .push_back(crate::agent::PendingPrompt::user("still queued".to_owned()));

    let (server_end, client_end) = UnixStream::pair().expect("pair");
    client_end
        .set_read_timeout(Some(Duration::from_millis(200)))
        .expect("read timeout");
    h.accept_client(server_end).expect("accept");
    let ui_conn = h
        .bus
        .connections()
        .into_iter()
        .find(|c| c.name == "socket-ui")
        .expect("ui connection")
        .id
        .to_string();

    h.handle_client_event(
        &ui_conn,
        Frame::Message(Message::Subscribe(Subscribe {
            selectors: vec![EventSelector::Prefix("agent.".to_owned())],
        })),
    )
    .expect("subscribe");

    let mut reader = FrameReader::new(BufReader::new(client_end));
    let mut queued = Vec::new();
    while let Ok(Some(frame)) = reader.read_frame() {
        let (_log_id, inner) = frame.peel_log();
        if let Frame::Event(Event::AgentPromptQueued(event)) = inner {
            queued.push(event);
        }
    }

    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].text, "still queued");
    assert_eq!(queued[0].agent_id.as_str(), agent_id);

    h.shutdown().expect("shutdown");
}

#[test]
fn late_joining_ui_client_replays_terminal_tool_events() {
    // Background completions and cancellation are terminal UI facts. A
    // late UI needs them to clear running tool blocks that were created
    // from earlier live progress before the UI joined.
    let td = TempDir::new().expect("tempdir");
    let sp = td.path().join("state");
    let mut h = echo_harness(&sp).expect("start");
    let cid = ensure_test_user_agent(&mut h);
    let agent_id = h
        .ensure_agent_id_for_agent(&cid)
        .expect("default conversation has an agent id");

    // Seed known tool calls in the agent transcript before recording terminal
    // UI facts for them. Background completions are separate facts that arrive
    // after the foreground round has already been closed with a placeholder.
    let seed_tool_call = |h: &mut Harness, spid: &str, call_id: &str, tool_name: &str| {
        h.publish_for_agent(
            &cid,
            Event::ProviderResponseFinished(ProviderResponseFinished {
                agent_prompt_id: spid.into(),
                agent_id: agent_id.clone().into(),
                output_items: vec![ContextItem::ToolCall(ToolCallItem {
                    call_id: call_id.into(),
                    name: ToolName::new(tool_name),
                    tool_type: tau_proto::ToolType::Function,
                    arguments: CborValue::Map(Vec::new()),
                })],
                stop_reason: tau_proto::ProviderStopReason::ToolCalls,
                originator: Default::default(),
                usage: None,
                backend: None,
                provider_response_id: None,
                ws_pool_delta: None,
            }),
        );
    };

    seed_tool_call(
        &mut h,
        "sp-background-result",
        "background-result-call",
        "background_ok",
    );
    h.publish_for_agent(
        &cid,
        Event::ProviderToolResult(ToolResult {
            call_id: "background-result-call".into(),
            tool_name: ToolName::new("background_ok"),
            tool_type: tau_proto::ToolType::Function,
            result: CborValue::Text("running".to_owned()),
            kind: tau_proto::ToolResultKind::BackgroundPlaceholder,
            display: None,
            originator: Default::default(),
        }),
    );
    h.publish_for_agent(
        &cid,
        Event::ToolBackgroundResult(tau_proto::ToolBackgroundResult {
            call_id: "background-result-call".into(),
            tool_name: ToolName::new("background_ok"),
            tool_type: tau_proto::ToolType::Function,
            result: CborValue::Text("done".to_owned()),
            display: None,
            originator: Default::default(),
        }),
    );

    seed_tool_call(
        &mut h,
        "sp-background-error",
        "background-error-call",
        "background_err",
    );
    h.publish_for_agent(
        &cid,
        Event::ProviderToolResult(ToolResult {
            call_id: "background-error-call".into(),
            tool_name: ToolName::new("background_err"),
            tool_type: tau_proto::ToolType::Function,
            result: CborValue::Text("running".to_owned()),
            kind: tau_proto::ToolResultKind::BackgroundPlaceholder,
            display: None,
            originator: Default::default(),
        }),
    );
    h.publish_for_agent(
        &cid,
        Event::ToolBackgroundError(tau_proto::ToolBackgroundError {
            call_id: "background-error-call".into(),
            tool_name: ToolName::new("background_err"),
            tool_type: tau_proto::ToolType::Function,
            message: "failed after backgrounding".to_owned(),
            details: None,
            display: None,
            originator: Default::default(),
        }),
    );

    seed_tool_call(&mut h, "sp-cancelled", "cancelled-call", "cancel_me");
    h.publish_for_agent(
        &cid,
        Event::ToolCancelled(tau_proto::ToolCancelled {
            call_id: "cancelled-call".into(),
            tool_name: ToolName::new("cancel_me"),
            tool_type: tau_proto::ToolType::Function,
        }),
    );

    let durable_events = loaded_agent_events(&h, "s1");
    assert!(
        durable_events.iter().any(|event| {
            matches!(event, Event::ToolBackgroundResult(result)
                if result.call_id.as_str() == "background-result-call")
        }),
        "background result should be in a durable loaded-agent event log"
    );
    assert!(
        durable_events.iter().any(|event| {
            matches!(event, Event::ToolBackgroundError(error)
                if error.call_id.as_str() == "background-error-call")
        }),
        "background error should be in a durable loaded-agent event log"
    );
    assert!(
        durable_events.iter().any(|event| {
            matches!(event, Event::ToolCancelled(cancelled)
                if cancelled.call_id.as_str() == "cancelled-call")
        }),
        "cancellation should be in a durable loaded-agent event log"
    );

    let (server_end, client_end) = UnixStream::pair().expect("pair");
    client_end
        .set_read_timeout(Some(Duration::from_millis(200)))
        .expect("read timeout");
    h.accept_client(server_end).expect("accept");
    let ui_conn = h
        .bus
        .connections()
        .into_iter()
        .find(|c| c.name == "socket-ui")
        .expect("ui connection")
        .id
        .to_string();

    h.handle_client_event(
        &ui_conn,
        Frame::Message(Message::Subscribe(Subscribe {
            selectors: vec![EventSelector::Prefix("tool.".to_owned())],
        })),
    )
    .expect("subscribe");

    let mut reader = FrameReader::new(BufReader::new(client_end));
    let mut got_background_result = false;
    let mut got_background_error = false;
    let mut got_cancelled = false;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline
        && !(got_background_result && got_background_error && got_cancelled)
    {
        let Ok(Some(frame)) = reader.read_frame() else {
            break;
        };
        let (_log_id, inner) = frame.peel_log();
        let Frame::Event(event) = inner else { continue };
        match event {
            Event::ToolBackgroundResult(result)
                if result.call_id.as_str() == "background-result-call" =>
            {
                got_background_result = true;
            }
            Event::ToolBackgroundError(error)
                if error.call_id.as_str() == "background-error-call" =>
            {
                got_background_error = true;
            }
            Event::ToolCancelled(cancelled) if cancelled.call_id.as_str() == "cancelled-call" => {
                got_cancelled = true;
            }
            _ => {}
        }
    }

    assert!(
        got_background_result,
        "late UI should replay background tool result"
    );
    assert!(
        got_background_error,
        "late UI should replay background tool error"
    );
    assert!(got_cancelled, "late UI should replay tool cancellation");

    h.shutdown().expect("shutdown");
}

#[test]
fn late_joining_ui_client_does_not_replay_runtime_extension_setup() {
    // Extension discovery/context-ready events are runtime setup facts. The
    // durable replay path now comes from session membership plus loaded-agent
    // transcripts, so these extension events should neither land in the
    // membership log nor be replayed from a transcript to late UI clients.
    let td = TempDir::new().expect("tempdir");
    let sp = td.path().join("state");
    let mut h = echo_harness(&sp).expect("start");
    let tools_conn = h
        .extension_connection_id("shell")
        .expect("shell")
        .to_owned();

    // Inject synthetic discovery events as if ext-shell had reported
    // them during eager init. They should remain runtime-only events.
    h.publish_event(
        Some(&tools_conn),
        Event::ExtAgentsMdAvailable(tau_proto::ExtAgentsMdAvailable {
            file_path: "/test/AGENTS.md".into(),
            content: "# test\n".to_owned(),
        }),
    );
    h.publish_event(
        Some(&tools_conn),
        Event::ExtensionContextReady(tau_proto::ExtensionContextReady {
            session_id: default_session_id().into(),
        }),
    );

    // Hook up a fake UI client via a UnixStream pair.
    let (server_end, client_end) = UnixStream::pair().expect("pair");
    client_end
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("read timeout");
    h.accept_client(server_end).expect("accept");

    // Find the UI connection the bus assigned. `accept_client`
    // gives it name "socket-ui".
    let ui_conn = h
        .bus
        .connections()
        .into_iter()
        .find(|c| c.name == "socket-ui")
        .expect("ui connection")
        .id
        .to_string();

    // Trigger subscribe + replay via the normal client-event path.
    h.handle_client_event(
        &ui_conn,
        Frame::Message(Message::Subscribe(Subscribe {
            selectors: vec![EventSelector::Prefix("extension.".to_owned())],
        })),
    )
    .expect("subscribe");

    let session_events = h
        .store
        .session_events(h.current_session_id.as_str())
        .expect("events");
    assert!(
        session_events.iter().all(|e| !matches!(
            &e.event,
            Event::ExtAgentsMdAvailable(_) | Event::ExtensionContextReady(_)
        )),
        "runtime extension setup must not be persisted in the session membership log"
    );

    let mut reader = FrameReader::new(BufReader::new(client_end));
    let mut agents_md_count = 0;
    let mut context_ready_count = 0;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let Ok(Some(frame)) = reader.read_frame() else {
            break;
        };
        let (_log_id, inner) = frame.peel_log();
        let Frame::Event(inner) = inner else { continue };
        match inner {
            Event::ExtAgentsMdAvailable(a)
                if a.file_path == std::path::Path::new("/test/AGENTS.md") =>
            {
                agents_md_count += 1;
            }
            Event::ExtensionContextReady(_) => {
                context_ready_count += 1;
            }
            _ => {}
        }
    }
    assert_eq!(
        agents_md_count, 0,
        "runtime agents_md setup should not replay to late UI clients"
    );
    assert_eq!(
        context_ready_count, 0,
        "runtime context-ready setup should not replay to late UI clients"
    );

    h.shutdown().expect("shutdown");
}

#[test]
fn resumed_harness_replays_persisted_session_history() {
    let td = TempDir::new().expect("tempdir");
    let sp = td.path().join("state");

    {
        let mut h = echo_harness_for("s1", &sp).expect("start");
        h.selected_model = Some("test/model".into());

        h.submit_user_prompt("s1".into(), "remember potato".to_owned())
            .expect("submit first prompt");
        let spid = h
            .prompt_agents
            .keys()
            .next()
            .expect("first session prompt id")
            .clone();
        let cid = h
            .prompt_agents
            .get(&spid)
            .expect("first prompt conversation")
            .clone();
        let agent_id = h
            .agents
            .get(&cid)
            .and_then(|conv| conv.agent_id.as_ref())
            .expect("first prompt agent id")
            .clone();
        h.handle_provider_response_finished(ProviderResponseFinished {
            agent_prompt_id: spid,
            agent_id: agent_id.into(),
            output_items: assistant_output("remembered potato"),
            stop_reason: tau_proto::ProviderStopReason::EndTurn,
            originator: tau_proto::PromptOriginator::User,
            usage: None,
            backend: None,
            provider_response_id: None,
            ws_pool_delta: None,
        })
        .expect("persist agent response");

        h.shutdown().expect("shutdown");
        drop(h);
        wait_for_session_unlock(&sp, "s1");
    }

    let mut resumed =
        echo_harness_with_start_reason("s1", &sp, tau_proto::SessionStartReason::Resume)
            .expect("resume");
    resumed.selected_model = Some("test/model".into());

    resumed
        .submit_user_prompt("s1".into(), "what was it?".to_owned())
        .expect("submit resumed prompt");
    let spid = resumed
        .prompt_agents
        .keys()
        .next()
        .expect("resumed session prompt id")
        .clone();
    let prompt = read_prompt_created(&resumed, &spid);
    let serialized = serde_json::to_string(&prompt.context_items).expect("json");

    assert!(
        serialized.contains("remember potato"),
        "resumed prompt must replay persisted user message: {serialized}",
    );
    assert!(
        serialized.contains("remembered potato"),
        "resumed prompt must replay persisted agent response: {serialized}",
    );
    assert!(
        serialized.contains("what was it?"),
        "resumed prompt must include the new prompt: {serialized}",
    );

    resumed.shutdown().expect("shutdown");
}

#[test]
fn thinking_is_persisted_but_excluded_from_prompt_replay() {
    // Linear-prefix and prompt-cache hygiene depends on
    // `assemble_conversation` ignoring the persisted thinking
    // field. Otherwise the model would see its own reasoning
    // summary echoed back as plain assistant text.
    let td = TempDir::new().expect("tempdir");
    let sp = td.path().join("state");
    let mut h = echo_harness(&sp).expect("start");
    h.selected_model = Some("test/model".into());

    append_user_message_via_event(&mut h, "s1", "first");

    let spid1 = h.send_prompt_to_agent("s1");
    h.handle_provider_response_finished(ProviderResponseFinished {
        agent_prompt_id: spid1,
        agent_id: "main".into(),
        output_items: assistant_output("answer"),
        stop_reason: tau_proto::ProviderStopReason::EndTurn,
        originator: tau_proto::PromptOriginator::User,
        usage: None,
        backend: None,
        provider_response_id: None,
        ws_pool_delta: None,
    })
    .expect("persist agent response");

    append_user_message_via_event(&mut h, "s1", "second");
    let spid2 = h.send_prompt_to_agent("s1");
    let prompt2 = read_prompt_created(&h, &spid2);
    let serialized = serde_json::to_string(&prompt2.context_items).expect("json");
    assert!(
        !serialized.contains("The user is asking"),
        "prompt replay must not echo reasoning summary back to the model",
    );

    h.shutdown().expect("shutdown");
}
