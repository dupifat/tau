use std::time::Duration;

use serde_json::json;
use tempfile::TempDir;

use super::*;

/// Provider cassettes should be reviewable fixtures keyed by Tau's stable
/// session/prompt/transport identity, and replay must validate the exact
/// request before yielding upstream events.
#[test]
fn record_then_replay_validates_request_and_returns_scaled_events() {
    let tempdir = TempDir::new().expect("tempdir");
    let request = json!({
        "model": "gpt-test",
        "prompt_cache_key": "stable-cache-key",
        "input": [{"role": "user", "content": "hello"}],
    });
    let key = TurnKey::new("session-a", "ap-agent-0", "websocket");

    let VcrTurn::Record(mut recording) = begin(
        &VcrConfig::new(VcrMode::RecordIfMissing, tempdir.path()),
        key.clone(),
        request.clone(),
    )
    .expect("record") else {
        panic!("record-if-missing mode should return recorder for a missing cassette");
    };
    recording.record_raw_event_after(Duration::from_micros(250_000), "{\"type\":\"delta\"}");
    recording.record_raw_event_after(Duration::from_micros(500_000), "{\"type\":\"done\"}");
    let path = recording.path().to_path_buf();
    recording.finish().expect("finish recording");

    assert_eq!(
        path.file_name().and_then(|name| name.to_str()),
        Some("session-a-ap-agent-0-websocket.yaml")
    );
    let written = std::fs::read_to_string(&path).expect("read cassette");
    assert!(written.contains("prompt_cache_key"));

    let VcrTurn::Replay(replay) = begin(
        &VcrConfig::new(VcrMode::ReplayOnly, tempdir.path()),
        key,
        request,
    )
    .expect("replay") else {
        panic!("replay-only mode should return replay");
    };
    let events = replay.events_at_speed(100.0).collect::<Vec<_>>();

    assert_eq!(events.len(), 2);
    assert_eq!(events[0].delay, Duration::from_micros(2_500));
    assert_eq!(events[0].raw, "{\"type\":\"delta\"}");
    assert_eq!(events[1].delay, Duration::from_micros(5_000));
    assert_eq!(events[1].raw, "{\"type\":\"done\"}");
}

/// A cassette belongs to one exact provider request. If request construction
/// drifts, replay should fail at the cassette boundary instead of returning a
/// plausible-looking but wrong upstream stream.
#[test]
fn replay_rejects_request_mismatch() {
    let tempdir = TempDir::new().expect("tempdir");
    let key = TurnKey::new("session-a", "ap-agent-0", "http-sse");
    let VcrTurn::Record(recording) = begin(
        &VcrConfig::new(VcrMode::RecordIfMissing, tempdir.path()),
        key.clone(),
        json!({"input": "old"}),
    )
    .expect("record") else {
        panic!("record-if-missing mode should return recorder for a missing cassette");
    };
    recording.finish().expect("finish recording");

    let error = begin(
        &VcrConfig::new(VcrMode::ReplayOnly, tempdir.path()),
        key,
        json!({"input": "new"}),
    )
    .expect_err("request mismatch should fail replay");

    match error {
        VcrError::RequestMismatch {
            expected, actual, ..
        } => {
            assert_eq!(expected, json!({"input": "old"}));
            assert_eq!(actual, json!({"input": "new"}));
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

/// Record-if-missing mode protects existing fixtures. To refresh a cassette,
/// developers delete the file and let the next run record it again.
#[test]
fn record_if_missing_replays_existing_until_file_is_deleted() {
    let tempdir = TempDir::new().expect("tempdir");
    let key = TurnKey::new("session-a", "ap-agent-0", "websocket");
    let request = json!({"turn": 1});

    let VcrTurn::Record(mut first) = begin(
        &VcrConfig::new(VcrMode::RecordIfMissing, tempdir.path()),
        key.clone(),
        request.clone(),
    )
    .expect("record first") else {
        panic!("missing cassette should record");
    };
    first.record_raw_event_after(Duration::ZERO, "first");
    let path = first.path().to_path_buf();
    first.finish().expect("finish first");

    let VcrTurn::Replay(existing) = begin(
        &VcrConfig::new(VcrMode::RecordIfMissing, tempdir.path()),
        key.clone(),
        request.clone(),
    )
    .expect("replay existing") else {
        panic!("existing cassette should replay");
    };
    assert_eq!(
        existing.events_at_speed(1.0).next().expect("event").raw,
        "first"
    );

    std::fs::remove_file(&path).expect("delete cassette");
    let VcrTurn::Record(mut second) = begin(
        &VcrConfig::new(VcrMode::RecordIfMissing, tempdir.path()),
        key,
        json!({"turn": 2}),
    )
    .expect("record second") else {
        panic!("deleted cassette should record again");
    };
    second.record_raw_event_after(Duration::ZERO, "second");
    second.finish().expect("finish second");

    let cassette: Cassette =
        serde_yaml_ng::from_str(&std::fs::read_to_string(path).expect("read refreshed cassette"))
            .expect("cassette yaml");
    assert_eq!(cassette.request.body, json!({"turn": 2}));
    assert_eq!(cassette.response.raw_events[0].raw, "second");
}

/// Session names are often test names, but providers should not be able to
/// accidentally create nested paths or shell-looking file names through a
/// cassette key.
#[test]
fn turn_key_sanitizes_file_name_components() {
    let key = TurnKey::new("mod::test/name", "ap-agent.0", "http/sse");

    assert_eq!(key.file_name(), "mod__test_name-ap-agent_0-http_sse.yaml");
}

/// Record-if-missing mode is useful for local development: first run records a
/// missing cassette, later runs replay the same cassette.
#[test]
fn record_if_missing_records_missing_and_replays_existing_cassette() {
    let tempdir = TempDir::new().expect("tempdir");
    let key = TurnKey::new("session-a", "ap-agent-0", "websocket");
    let request = json!({"input": "hello"});

    let VcrTurn::Record(mut recording) = begin(
        &VcrConfig::new(VcrMode::RecordIfMissing, tempdir.path()),
        key.clone(),
        request.clone(),
    )
    .expect("record") else {
        panic!("missing cassette should record");
    };
    recording.record_raw_event_after(Duration::ZERO, "hello");
    recording.finish().expect("finish recording");

    let VcrTurn::Replay(replay) = begin(
        &VcrConfig::new(VcrMode::RecordIfMissing, tempdir.path()),
        key,
        request,
    )
    .expect("replay") else {
        panic!("existing cassette should replay");
    };

    let events = replay.events_at_speed(1.0).collect::<Vec<_>>();
    assert_eq!(events[0].raw, "hello");
}
