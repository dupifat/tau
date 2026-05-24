use tau_proto::{
    Frame, ModelId, PromptOriginator, ProviderResponseFinished, ProviderResponseUpdated,
    ProviderTokenUsage, SessionPromptId,
};

use super::*;
use crate::event::HarnessEvent;

fn read_lines(path: &Path) -> Vec<serde_json::Value> {
    let raw = std::fs::read_to_string(path).expect("read events.jsonl");
    raw.lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str::<serde_json::Value>(l).expect("parse line"))
        .collect()
}

#[test]
fn published_line_preserves_enriched_token_usage() {
    let td = tempfile::tempdir().expect("tempdir");
    let mut log = DebugEventLog::open(td.path()).expect("open");
    let model: ModelId = "openai/gpt-5".parse().expect("model id");
    let event = Event::ProviderResponseFinished(ProviderResponseFinished {
        session_prompt_id: SessionPromptId::from("sp-0"),
        target_agent_id: None,
        output_items: Vec::new(),
        stop_reason: tau_proto::ProviderStopReason::EndTurn,
        originator: PromptOriginator::User,
        usage: Some(ProviderTokenUsage {
            model: Some(model),
            prompt_sent_tokens: 1000,
            prompt_cached_tokens: 800,
            response_received_tokens: 42,
            stats: tau_proto::TokenUsageStats::default(),
        }),
        backend: None,
        provider_response_id: None,
        ws_pool_delta: None,
    });
    log.log_published_event(
        Some(&ConnectionId::from("conn-1")),
        &event,
        UnixMicros::now(),
    );

    let lines = read_lines(log.path());
    assert_eq!(lines.len(), 1);
    let line = &lines[0];
    assert_eq!(line["type"], "published");
    assert_eq!(line["event_name"], "provider.response_finished");
    assert_eq!(line["source"], "conn-1");
    let usage = &line["event"]["payload"]["usage"];
    assert_eq!(usage["prompt_sent_tokens"], 1000);
    assert_eq!(usage["prompt_cached_tokens"], 800);
    assert_eq!(usage["response_received_tokens"], 42);
    assert_eq!(usage["model"], "openai/gpt-5");
}

#[test]
fn published_line_compacts_long_strings() {
    let td = tempfile::tempdir().expect("tempdir");
    let mut log = DebugEventLog::open(td.path()).expect("open");
    let event = Event::ProviderResponseUpdated(ProviderResponseUpdated {
        session_prompt_id: SessionPromptId::from("sp-0"),
        text: "x".repeat(101),
        thinking: Some(format!("{}{}{}", "α".repeat(30), "middle", "ω".repeat(30))),
        originator: PromptOriginator::User,
    });

    log.log_published_event(None, &event, UnixMicros::now());

    let lines = read_lines(log.path());
    assert_eq!(lines.len(), 1);
    let payload = &lines[0]["event"]["payload"];
    assert_eq!(
        payload["text"],
        "xxxxxxxxxxxxxxxxxxxx┄total 101┄xxxxxxxxxxxxxxxxxxxx"
    );
    assert_eq!(payload["thinking"], "αααααααααα┄total 126┄ωωωωωωωωωω");
}

#[test]
fn compact_debug_string_keeps_short_strings() {
    assert_eq!(compact_debug_string(&"x".repeat(100)), "x".repeat(100));
}

#[test]
fn transient_from_connection_events_are_not_logged_twice() {
    let td = tempfile::tempdir().expect("tempdir");
    let mut log = DebugEventLog::open(td.path()).expect("open");
    let event = Event::ProviderResponseUpdated(ProviderResponseUpdated {
        session_prompt_id: SessionPromptId::from("sp-0"),
        text: "partial".to_owned(),
        thinking: None,
        originator: PromptOriginator::User,
    });

    log.log_harness_event(&HarnessEvent::FromConnection {
        connection_id: ConnectionId::from("conn-1"),
        frame: Box::new(Frame::Event(event)),
    });

    let lines = read_lines(log.path());
    assert!(
        lines.is_empty(),
        "transient streaming events are logged on publish; the raw inbound copy is redundant"
    );
}
