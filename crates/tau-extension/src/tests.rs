use std::io::Cursor;

use tau_proto::{
    EventName, EventSelector, HarnessInputMessage, HarnessInputReader, Intercept,
    InterceptionPriority, PeerOutputWriter,
};

use super::*;

#[derive(serde::Deserialize, PartialEq, Debug)]
#[serde(default, deny_unknown_fields)]
struct Demo {
    n: u32,
}

impl Default for Demo {
    fn default() -> Self {
        Self { n: 1 }
    }
}

fn handshake_messages(handshake: Handshake) -> Vec<HarnessInputMessage> {
    let mut bytes = Vec::new();
    {
        let mut writer = PeerOutputWriter::new(&mut bytes);
        handshake.run(&mut writer).expect("handshake should encode");
    }

    let mut reader = HarnessInputReader::new(Cursor::new(bytes));
    let mut messages = Vec::new();
    while let Some(message) = reader
        .read_message()
        .expect("handshake message should decode")
    {
        messages.push(message);
    }
    messages
}

fn cbor_from_json(json: serde_json::Value) -> tau_proto::CborValue {
    tau_proto::json_to_cbor(&json)
}

#[test]
fn parse_config_returns_typed_struct() {
    let value = cbor_from_json(serde_json::json!({ "n": 7 }));
    let cfg: Demo = parse_config(&value).expect("parse");
    assert_eq!(cfg, Demo { n: 7 });
}

#[test]
fn parse_config_error_strips_debug_wrapper() {
    let value = cbor_from_json(serde_json::json!({ "wrong": 7 }));
    let err = parse_config::<Demo>(&value).expect_err("should fail");
    // No `Semantic(None, "…")` wrapping: just the message.
    assert!(!err.starts_with("Semantic"), "got: {err}");
    assert!(err.contains("unknown field"), "got: {err}");
    assert!(err.contains("wrong"), "got: {err}");
}

/// Ensures repeated same-priority intercepts become one wire registration.
#[test]
fn handshake_accumulates_intercepts_into_one_message() {
    let priority = InterceptionPriority::new(5);
    let messages = handshake_messages(
        Handshake::tool("demo")
            .intercept(EventSelector::Exact(EventName::TOOL_STARTED), priority)
            .intercept(EventSelector::Prefix("tool.".to_owned()), priority),
    );

    assert_eq!(messages.len(), 3);
    assert_eq!(
        messages[1],
        HarnessInputMessage::Intercept(Intercept {
            selectors: vec![
                EventSelector::Exact(EventName::TOOL_STARTED),
                EventSelector::Prefix("tool.".to_owned()),
            ],
            priority,
        })
    );
}

/// Ensures mixed-priority intercept registrations fail loudly instead of
/// silently replacing an earlier policy in the harness.
#[test]
#[should_panic(expected = "mixed interception priorities")]
fn handshake_rejects_mixed_intercept_priorities() {
    let _ = Handshake::tool("demo")
        .intercept(
            EventSelector::Exact(EventName::TOOL_STARTED),
            InterceptionPriority::new(1),
        )
        .intercept(
            EventSelector::Prefix("tool.".to_owned()),
            InterceptionPriority::new(2),
        );
}
