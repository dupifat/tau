use tau_proto::{ModelParams, ToolDefinition, ToolName};

use super::compute_chain_fingerprint;

fn tool(name: &str) -> ToolDefinition {
    ToolDefinition {
        name: ToolName::new(name),
        model_visible_name: None,
        description: None,
        tool_type: tau_proto::ToolType::Function,
        parameters: None,
        format: None,
    }
}

/// Locks in the inputs that DO matter for chain validity. If a
/// future change drops one of these from the hash, the matching
/// pair below stops differing and the test fails — catching the
/// regression before any real session loses cache.
#[test]
fn fingerprint_changes_when_real_inputs_drift() {
    let base = compute_chain_fingerprint(
        "sys",
        &[tool("a")],
        &ModelParams::default(),
        tau_proto::ToolChoice::Auto,
    );

    assert_ne!(
        base,
        compute_chain_fingerprint(
            "sys-changed",
            &[tool("a")],
            &ModelParams::default(),
            tau_proto::ToolChoice::Auto,
        ),
        "system_prompt drift must change the fingerprint",
    );
    assert_ne!(
        base,
        compute_chain_fingerprint(
            "sys",
            &[tool("a"), tool("b")],
            &ModelParams::default(),
            tau_proto::ToolChoice::Auto,
        ),
        "tools drift must change the fingerprint",
    );
    let params = ModelParams {
        effort: tau_proto::Effort::High,
        ..ModelParams::default()
    };
    assert_ne!(
        base,
        compute_chain_fingerprint("sys", &[tool("a")], &params, tau_proto::ToolChoice::Auto),
        "model_params drift must change the fingerprint",
    );
}

#[test]
fn fingerprint_changes_when_tool_choice_drifts() {
    let base = compute_chain_fingerprint(
        "sys",
        &[tool("a")],
        &ModelParams::default(),
        tau_proto::ToolChoice::Auto,
    );

    assert_ne!(
        base,
        compute_chain_fingerprint(
            "sys",
            &[tool("a")],
            &ModelParams::default(),
            tau_proto::ToolChoice::None,
        ),
        "tool_choice drift must change the fingerprint because it changes the wire request",
    );
}

#[test]
fn fingerprint_is_stable_across_repeated_calls() {
    // Whatever inputs `compute_chain_fingerprint` accepts must
    // produce the same hash when called twice with the same
    // values. Guards against accidental nondeterminism (e.g. if
    // someone reaches for `HashMap` serialization for tools).
    let a = compute_chain_fingerprint(
        "sys",
        &[tool("a")],
        &ModelParams::default(),
        tau_proto::ToolChoice::Auto,
    );
    let b = compute_chain_fingerprint(
        "sys",
        &[tool("a")],
        &ModelParams::default(),
        tau_proto::ToolChoice::Auto,
    );
    assert_eq!(a, b, "fingerprint must be deterministic");
}
