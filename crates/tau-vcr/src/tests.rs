use serde_json::json;
use tempfile::TempDir;

use super::*;

/// Cassette keys are logical identifiers, not paths. Rejecting unsupported
/// characters avoids lossy filename normalization where distinct logical keys
/// could collapse onto the same cassette file.
#[test]
fn store_rejects_invalid_keys() {
    let tempdir = TempDir::new().expect("tempdir");
    let store = VcrStore::new(tempdir.path());

    let error = store
        .put("tc-main/0001", &json!({"value": true}))
        .expect_err("invalid key should fail");

    assert!(matches!(error, VcrError::InvalidKey(key) if key == "tc-main/0001"));
}

/// The store is intentionally schema-agnostic: callers own the cassette shape,
/// while `tau-vcr` only persists and loads reviewable YAML by stable key.
#[test]
fn store_puts_and_gets_caller_owned_yaml_schema() {
    #[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
    struct ToolCassette {
        request: serde_json::Value,
        response: String,
    }

    let tempdir = TempDir::new().expect("tempdir");
    let store = VcrStore::new(tempdir.path());
    let cassette = ToolCassette {
        request: json!({"command": "cargo check"}),
        response: "ok".to_owned(),
    };

    store.put("tc-main-0001", &cassette).expect("put cassette");
    let loaded: ToolCassette = store
        .get("tc-main-0001")
        .expect("get cassette")
        .expect("cassette exists");

    assert_eq!(loaded, cassette);
}

/// Missing cassettes are reported as `None` rather than an IO error so callers
/// can implement record-if-missing at the provider/tool boundary that owns the
/// live request path.
#[test]
fn store_get_returns_none_for_missing_cassette() {
    let tempdir = TempDir::new().expect("tempdir");
    let store = VcrStore::new(tempdir.path());

    let loaded: Option<serde_json::Value> = store.get("missing").expect("missing should be ok");

    assert!(loaded.is_none());
}

/// Request validation is caller-owned, but `tau-vcr` still provides a standard
/// diagnostic error constructor so mismatches have consistent key and payload
#[test]
fn request_mismatch_error_carries_serialized_payloads() {
    let error = request_mismatch("tc-main-0001", &json!({"old": true}), &json!({"new": true}));

    match error {
        VcrError::RequestMismatch {
            expected, actual, ..
        } => {
            assert!(expected.contains("old"));
            assert!(actual.contains("new"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

/// Tau's safe automatic recording workflow is record-if-missing: existing
/// fixtures replay, while absent fixtures allow callers to hit the live path
/// and create a new cassette.
#[test]
fn mode_parses_record_if_missing_without_record_overwrite_mode() {
    assert_eq!(VcrMode::parse("off").expect("off"), VcrMode::Off);
    assert_eq!(
        VcrMode::parse("record-if-missing").expect("record-if-missing"),
        VcrMode::RecordIfMissing
    );
    assert_eq!(
        VcrMode::parse("replay-only").expect("replay-only"),
        VcrMode::ReplayOnly
    );
    assert!(VcrMode::parse("record").is_err());
}
