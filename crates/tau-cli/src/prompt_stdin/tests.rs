use tau_proto::{MessageItem, PromptOriginator, ProviderStopReason};

use super::*;

#[test]
fn prompt_stdin_role_uses_startup_role_or_default() {
    assert_eq!(prompt_stdin_role(Some("specialist")), "specialist");
    assert_eq!(prompt_stdin_role(None), DEFAULT_AGENT_ROLE);
}
fn user_update(spid: &str, text: &str, thinking: Option<&str>) -> ProviderResponseUpdated {
    let mut items = Vec::new();
    if let Some(thinking) = thinking.filter(|thinking| !thinking.is_empty()) {
        items.push(tau_proto::ProviderResponseItem::InProgress(
            tau_proto::InProgressOutputItem::ReasoningText {
                kind: tau_proto::ReasoningTextKind::Summary,
                text: thinking.to_owned(),
            },
        ));
    }
    if !text.is_empty() {
        items.push(tau_proto::ProviderResponseItem::InProgress(
            tau_proto::InProgressOutputItem::Message {
                text: text.to_owned(),
                phase: None,
            },
        ));
    }
    ProviderResponseUpdated {
        agent_prompt_id: spid.into(),
        items,
        compaction_original_input_tokens: None,
        compaction_compacted_input_tokens: None,
        originator: PromptOriginator::User,
    }
}

fn assistant_finished(
    spid: &str,
    text: &str,
    stop_reason: ProviderStopReason,
) -> ProviderResponseFinished {
    ProviderResponseFinished {
        agent_prompt_id: spid.into(),
        agent_id: "main".into(),
        output_items: vec![ContextItem::Message(MessageItem {
            role: ContextRole::Assistant,
            content: vec![ContentPart::Text {
                text: text.to_owned(),
            }],
            phase: None,
        })],
        stop_reason,
        originator: PromptOriginator::User,
        ..ProviderResponseFinished::default()
    }
}

/// The one-shot client ignores streaming updates for display but keeps the
/// latest complete snapshots so finished turns can print reasoning blocks
/// and the final answer only once the agent is done.
#[test]
fn one_shot_output_waits_through_tool_calls_and_keeps_final_snapshots() {
    let mut output = OneShotOutput::default();
    output.capture_update(&user_update("sp-tool", "", Some("plan v1")));
    output.capture_update(&user_update("sp-tool", "", Some("plan final")));

    assert!(!output.capture_finished(&ProviderResponseFinished {
        agent_prompt_id: "sp-tool".into(),
        agent_id: "main".into(),
        stop_reason: ProviderStopReason::ToolCalls,
        error: None,
        originator: PromptOriginator::User,
        ..ProviderResponseFinished::default()
    }));

    output.capture_update(&user_update(
        "sp-final",
        "streamed answer",
        Some("answer plan"),
    ));
    assert!(output.capture_finished(&assistant_finished(
        "sp-final",
        "final answer",
        ProviderStopReason::EndTurn,
    )));

    assert_eq!(output.thinking_blocks, vec!["plan final", "answer plan"]);
    assert_eq!(output.final_response.as_deref(), Some("final answer"));
}

/// Some provider paths may have accumulated streaming text but no final
/// assistant message item; fall back to the latest full update rather than
/// printing nothing.
#[test]
fn one_shot_output_falls_back_to_latest_streaming_text() {
    let mut output = OneShotOutput::default();
    output.capture_update(&user_update("sp-final", "partial", None));
    output.capture_update(&user_update("sp-final", "complete", None));

    assert!(output.capture_finished(&ProviderResponseFinished {
        agent_prompt_id: "sp-final".into(),
        agent_id: "main".into(),
        stop_reason: ProviderStopReason::EndTurn,
        error: None,
        originator: PromptOriginator::User,
        ..ProviderResponseFinished::default()
    }));

    assert_eq!(output.final_response.as_deref(), Some("complete"));
}
