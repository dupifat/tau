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
