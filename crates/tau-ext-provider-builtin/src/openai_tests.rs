use std::io::{BufReader, Cursor};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use tau_proto::{Effort, Verbosity};

use super::*;

fn chatgpt_auth() -> OpenAiAuth {
    OpenAiAuth {
        access_token: "access".to_owned(),
        refresh_token: "refresh".to_owned(),
        expires_at_ms: u64::MAX,
        account_id: Some("account".to_owned()),
    }
}

fn model_ids(models: &[ProviderModelInfo]) -> Vec<String> {
    models.iter().map(|model| model.id.to_string()).collect()
}

fn decode_frames(bytes: &[u8]) -> Vec<Frame> {
    let mut reader = tau_proto::FrameReader::new(BufReader::new(bytes));
    let mut frames = Vec::new();
    while let Some(frame) = reader.read_frame().expect("decode frame") {
        frames.push(frame);
    }
    frames
}

fn encode_frames(frames: &[Frame]) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut writer = FrameWriter::new(&mut bytes);
        for frame in frames {
            writer.write_frame(frame).expect("encode frame");
        }
        writer.flush().expect("flush frames");
    }
    bytes
}

fn model_id(provider: &str, model: &str) -> ModelId {
    ModelId::new(ProviderName::new(provider), ModelName::new(model))
}

fn prompt() -> tau_proto::SessionPromptCreated {
    tau_proto::SessionPromptCreated {
        session_prompt_id: "sp-1".into(),
        session_id: "s1".into(),
        system_prompt: String::new(),
        context_items: vec![ContextItem::Message(tau_proto::MessageItem {
            role: tau_proto::ContextRole::User,
            content: vec![tau_proto::ContentPart::Text {
                text: "hello".to_owned(),
            }],
            phase: None,
        })],
        tools: Vec::new(),
        tools_ref: None,
        model: Some(model_id(CHATGPT_PROVIDER_NAME, "gpt-5.5")),
        model_params: Default::default(),
        tool_choice: tau_proto::ToolChoice::Auto,
        originator: tau_proto::PromptOriginator::User,
        share_user_cache_key: false,
        ctx_id: None,
        previous_response_candidate: None,
    }
}

#[test]
fn chatgpt_profile_publishes_models_even_without_auth_tokens() {
    // Profile existence is the registration signal. Auth validity affects
    // prompt execution, not whether the registered account's models are visible.
    let models = models_for_auth(&OpenAiAuth::default());

    assert!(model_ids(&models).starts_with(&["chatgpt/gpt-5.5".to_owned()]));
}

#[test]
fn chatgpt_oauth_publishes_chatgpt_models() {
    // ChatGPT/Codex is a provider namespace named `chatgpt`; there is no
    // compatibility fallback to an `openai-codex` provider name.
    let models = models_for_auth(&chatgpt_auth());

    assert_eq!(
        model_ids(&models),
        vec![
            "chatgpt/gpt-5.5",
            "chatgpt/gpt-5.4",
            "chatgpt/gpt-5.4-mini",
            "chatgpt/gpt-5.3-codex"
        ]
    );
    assert!(models.iter().all(|model| model.supports_compaction));
}

#[test]
fn resolves_chatgpt_to_codex_responses_backend() {
    // ChatGPT is OAuth-backed and enables Codex-specific transport and replay
    // features owned by this provider slice.
    let mut profiles = profiles_with_chatgpt_auth(chatgpt_auth());

    let config =
        resolve_responses_backend(&model_id(CHATGPT_PROVIDER_NAME, "gpt-5.4"), &mut profiles)
            .expect("chatgpt backend");

    assert_eq!(config.surface, responses::ResponsesSurface::ChatGpt);
    assert_eq!(config.base_url, tau_provider_chatgpt::DEFAULT_BASE_URL);
    assert_eq!(config.api_key, "access");
    assert_eq!(config.account_id.as_deref(), Some("account"));
    assert!(config.supports_websocket);
    assert!(config.supports_compaction);
    assert!(config.supports_phase);
    assert!(config.supports_encrypted_reasoning);
}

#[test]
fn chatgpt_phase_metadata_is_model_specific() {
    // The assistant `phase` field is only accepted by newer Codex model
    // families, so the hardcoded resolver must preserve the old whitelist.
    let mut profiles = profiles_with_chatgpt_auth(chatgpt_auth());

    let old = resolve_responses_backend(
        &model_id(CHATGPT_PROVIDER_NAME, "gpt-5.2-codex"),
        &mut profiles,
    )
    .expect("old codex backend");
    let new = resolve_responses_backend(
        &model_id(CHATGPT_PROVIDER_NAME, "gpt-5.3-codex"),
        &mut profiles,
    )
    .expect("new codex backend");

    assert!(!old.supports_phase);
    assert!(new.supports_phase);
}

#[test]
fn xhigh_metadata_is_model_specific() {
    // The UI cycles through the provider-published effort list, so hardcoded
    // metadata must preserve xhigh only for model families that accept it.
    let models = models_for_auth(&chatgpt_auth());
    let ids_with_xhigh = models
        .iter()
        .filter(|model| model.efforts.contains(&Effort::XHigh))
        .map(|model| model.id.to_string())
        .collect::<Vec<_>>();

    assert_eq!(
        ids_with_xhigh,
        vec![
            "chatgpt/gpt-5.5",
            "chatgpt/gpt-5.4",
            "chatgpt/gpt-5.3-codex"
        ]
    );
}

#[test]
fn verbosity_metadata_is_published_for_chatgpt_models() {
    // The provider snapshot is authoritative for UI cycling, so ChatGPT
    // models must publish the verbosity choices they accept.
    let models = models_for_auth(&chatgpt_auth());
    let gpt = models
        .iter()
        .find(|model| model.id.to_string() == "chatgpt/gpt-5.5")
        .expect("gpt-5.5 model");

    assert_eq!(
        gpt.verbosities,
        vec![Verbosity::Low, Verbosity::Medium, Verbosity::High]
    );
}

#[test]
fn ack_tracker_waits_for_contiguous_completed_log_events() {
    // Parallel prompt workers can finish out of order, but `Ack { up_to }`
    // is cumulative. Do not ack a later prompt until earlier received log
    // events have completed, or a crash could lose accepted work.
    let mut tracker = AckTracker::default();
    tracker.register(tau_proto::LogEventId::new(7));
    tracker.register(tau_proto::LogEventId::new(8));

    tracker.complete(tau_proto::LogEventId::new(8));
    assert_eq!(tracker.next_ack(), None);

    tracker.complete(tau_proto::LogEventId::new(7));
    assert_eq!(tracker.next_ack(), Some(tau_proto::LogEventId::new(8)));
    assert_eq!(tracker.next_ack(), None);
}

#[test]
fn prompt_workers_start_concurrently() {
    // Regression coverage for backend-agent parallelism: two accepted
    // provider prompts must both enter worker execution before the first
    // one finishes. A serial dispatcher would time out the first worker's
    // wait and never observe two active starts at once.
    let mut first = prompt();
    first.session_prompt_id = "sp-par-1".into();
    let mut second = prompt();
    second.session_prompt_id = "sp-par-2".into();
    let input = encode_frames(&[
        Frame::Message(Message::LogEvent(tau_proto::LogEvent {
            id: tau_proto::LogEventId::new(7),
            recorded_at: tau_proto::UnixMicros::new(11),
            event: Box::new(Event::SessionPromptCreated(first)),
        })),
        Frame::Message(Message::LogEvent(tau_proto::LogEvent {
            id: tau_proto::LogEventId::new(8),
            recorded_at: tau_proto::UnixMicros::new(12),
            event: Box::new(Event::SessionPromptCreated(second)),
        })),
    ]);
    let started = std::sync::Arc::new((Mutex::new((0_usize, 0_usize)), Condvar::new()));
    let executor_started = started.clone();
    let executor: PromptExecutor = std::sync::Arc::new(move |execution| {
        let session_prompt_id = execution.job.session_prompt_id.clone();
        let originator = execution.job.prompt.originator.clone();
        let (lock, cv) = &*executor_started;
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut guard = lock.lock().expect("started lock");
        guard.0 += 1;
        guard.1 = guard.1.max(guard.0);
        cv.notify_all();
        while guard.0 < 2 {
            let now = Instant::now();
            let Some(remaining) = deadline.checked_duration_since(now) else {
                break;
            };
            let (next, wait) = cv.wait_timeout(guard, remaining).expect("wait for peer");
            guard = next;
            if wait.timed_out() {
                break;
            }
        }
        drop(guard);

        let mut writer = execution.frame_writer();
        write_prompt_submitted(&session_prompt_id, &originator, &mut writer).expect("submitted");
        writer
            .write_frame(&Frame::Event(Event::ProviderResponseFinished(
                simple_finished(session_prompt_id.clone(), originator, "done"),
            )))
            .expect("finished");
        writer.flush().expect("flush fake response");

        let mut guard = lock.lock().expect("started lock");
        guard.0 -= 1;
        cv.notify_all();
    });

    let profiles = profiles_with_chatgpt_auth(chatgpt_auth());
    let prompt_profiles = profiles.clone();
    let mut output = Vec::new();
    run_inner_with_prompt_executor(
        Cursor::new(input),
        &mut output,
        profiles,
        move || prompt_profiles.clone(),
        2,
        executor,
    )
    .expect("run provider extension");

    let max_started = started.0.lock().expect("started lock").1;
    assert_eq!(max_started, 2, "both prompt workers should overlap");
    let frames = decode_frames(&output);
    let finished_count = frames
        .iter()
        .filter(|frame| matches!(frame, Frame::Event(Event::ProviderResponseFinished(_))))
        .count();
    assert_eq!(finished_count, 2);
    assert!(frames.iter().any(|frame| {
        matches!(frame, Frame::Message(Message::Ack(ack)) if ack.up_to.get() == 8)
    }));
}

#[test]
fn run_announces_provider_models_before_ready() {
    // Provider model snapshots need to reach the harness during startup so
    // model/role UI state is available immediately after all extensions are
    // ready.
    let mut output = Vec::new();
    run_with_auth(std::io::empty(), &mut output, chatgpt_auth()).expect("run provider extension");

    let frames = decode_frames(&output);
    assert!(
        matches!(
            &frames[0],
            Frame::Message(Message::Hello(hello))
                if hello.client_kind == ClientKind::Provider
                    && hello.client_name.as_str() == EXTENSION_NAME
        ),
        "first frame should be provider hello: {frames:?}"
    );
    assert!(
        frames
            .iter()
            .any(|frame| matches!(frame, Frame::Message(Message::Subscribe(_)))),
        "provider should subscribe for prewarm/cancel events: {frames:?}"
    );
    assert!(
        frames.iter().any(|frame| matches!(
            frame,
            Frame::Event(Event::ProviderModelsUpdated(updated))
                if model_ids(&updated.models).starts_with(&["chatgpt/gpt-5.5".to_owned()])
        )),
        "startup frames should announce provider models: {frames:?}"
    );
    assert!(
        matches!(frames.last(), Some(Frame::Message(Message::Ready(_)))),
        "last frame should be ready: {frames:?}"
    );
}

#[test]
fn direct_prompt_request_with_missing_backend_is_acknowledged_and_closed() {
    // Direct provider routing must never leave the harness waiting forever,
    // even if a prompt reaches this extension without usable credentials.
    let input = encode_frames(&[
        Frame::Message(Message::LogEvent(tau_proto::LogEvent {
            id: tau_proto::LogEventId::new(7),
            recorded_at: tau_proto::UnixMicros::new(11),
            event: Box::new(Event::SessionPromptCreated(prompt())),
        })),
        Frame::Message(Message::Disconnect(tau_proto::Disconnect {
            reason: Some("done".to_owned()),
        })),
    ]);
    let mut output = Vec::new();
    run_with_auth(Cursor::new(input), &mut output, OpenAiAuth::default())
        .expect("run provider extension");

    let frames = decode_frames(&output);
    let submitted = frames.iter().position(|frame| {
        matches!(
            frame,
            Frame::Event(Event::ProviderPromptSubmitted(submitted))
                if submitted.session_prompt_id.as_str() == "sp-1"
        )
    });
    let finished = frames.iter().position(|frame| {
        matches!(
            frame,
            Frame::Event(Event::ProviderResponseFinished(finished))
                if finished.session_prompt_id.as_str() == "sp-1"
                    && finished.stop_reason == ProviderStopReason::EndTurn
        )
    });
    let ack = frames.iter().position(|frame| {
        matches!(
            frame,
            Frame::Message(Message::Ack(ack)) if ack.up_to.get() == 7
        )
    });

    let submitted = submitted.expect("prompt submitted event");
    let finished = finished.expect("missing-backend response finished event");
    let ack = ack.expect("ack for prompt LogEvent");
    assert!(submitted < finished, "submission should precede finish");
    assert!(finished < ack, "ack should follow prompt handling");
}
