use super::*;

/// Live delegate progress carries prompt size stats so users see the
/// delegate input volume immediately, before the sub-agent finishes.
#[test]
fn progress_display_includes_delegate_input_stats() {
    let input_stats = tau_proto::ToolUseStats {
        matches: None,
        lines: Some(2),
        bytes: Some(12),
    };
    let display = build_delegate_progress_display("audit", None, None, None, 0, 0, input_stats);

    assert_eq!(display.args, "[audit]");
    assert_eq!(display.stats, input_stats);
    assert_eq!(display.status, tau_proto::ToolUseStatus::InProgress);
}
