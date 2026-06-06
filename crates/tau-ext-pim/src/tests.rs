use std::collections::BTreeMap;

use serde::Deserialize;
use tau_proto::{
    EventName, EventSelector, HarnessInputMessage, HarnessInputReader, PeerOutputWriter,
};

use super::*;

#[test]
fn self_knowledge_pim_example_matches_extension_config_shape() {
    #[derive(Deserialize)]
    struct HarnessExample {
        extensions: BTreeMap<String, ExtensionExample>,
    }

    #[derive(Deserialize)]
    struct ExtensionExample {
        config: PimExtensionConfig,
    }

    let mut harness: HarnessExample =
        serde_yaml_ng::from_str(include_str!("../config/self-knowledge.harness.yaml"))
            .expect("self-knowledge PIM example parses as YAML");
    let pim = harness
        .extensions
        .remove("std-pim")
        .expect("std-pim example exists")
        .config;

    pim.email
        .expect("email example")
        .validate()
        .expect("email config validates");
    pim.calendar
        .expect("calendar example")
        .validate()
        .expect("calendar config validates");
}

#[test]
fn action_schema_contains_email_and_calendar_roots() {
    let roots = action_schema()
        .roots
        .into_iter()
        .map(|root| root.name)
        .collect::<Vec<_>>();

    assert_eq!(roots, vec!["/email", "/calendar"]);
}

/// PIM subscribes to `tool.started` to receive its own email/calendar
/// calls, but the harness event stream can also contain starts for
/// tools owned by other extensions. Those foreign calls must be ignored
/// instead of producing terminal tool errors that race with the real
/// provider result.
#[test]
fn ignores_tool_started_for_tools_owned_by_other_extensions() {
    let mut runtime = RuntimeState::default();
    for tool_name in ["read", email::TOOL_NAME, calendar::TOOL_NAME] {
        let invoke = tau_proto::ToolStarted {
            call_id: tau_proto::ToolCallId::new(format!("call-{tool_name}")),
            tool_name: tau_proto::ToolName::new(tool_name),
            arguments: CborValue::Map(vec![]),
            agent_id: tau_proto::AgentId::parse("agent-1").expect("agent id"),
            originator: tau_proto::PromptOriginator::User,
        };

        assert!(runtime.dispatch_tool(invoke).is_none());
    }
}

#[test]
fn handshake_registers_email_and_calendar_tools() {
    let mut bytes = Vec::new();
    let handshake = tau_extension::Handshake::tool("tau-ext-pim").subscribe([
        tau_proto::EventName::TOOL_STARTED,
        tau_proto::EventName::ACTION_INVOKE,
    ]);
    let handshake = register_tools_with_prompt_fragment(
        handshake,
        email::email_tool_specs(),
        tau_proto::ToolGroupName::new("email"),
        "email_get",
        email::email_prompt_fragment(),
    );
    let handshake = register_tools_with_prompt_fragment(
        handshake,
        calendar::calendar_tool_specs(),
        tau_proto::ToolGroupName::new("calendar"),
        "calendar_get",
        calendar::calendar_prompt_fragment(),
    );

    handshake
        .publish_actions(action_schema())
        .ready_message("pim extension ready")
        .run(&mut PeerOutputWriter::new(&mut bytes))
        .expect("handshake writes");

    let mut reader = HarnessInputReader::new(bytes.as_slice());
    let mut tools = Vec::new();
    let mut prompt_tools = Vec::new();
    let mut per_tool_prompt_tools = Vec::new();
    let mut saw_subscription = false;
    while let Some(frame) = reader.read_message().expect("frame decodes") {
        match frame {
            HarnessInputMessage::Subscribe(subscribe) => {
                saw_subscription = subscribe.selectors
                    == vec![
                        EventSelector::Exact(EventName::TOOL_STARTED),
                        EventSelector::Exact(EventName::ACTION_INVOKE),
                    ];
            }
            HarnessInputMessage::Emit(emit)
                if matches!(emit.event.as_ref(), Event::ToolRegister(_)) =>
            {
                let Event::ToolRegister(register) = *emit.event else {
                    unreachable!();
                };
                if register.prompt_fragment.is_some() {
                    per_tool_prompt_tools.push(register.tool.name.clone());
                }
                if register
                    .tool_group
                    .as_ref()
                    .and_then(|group| group.prompt_fragment.as_ref())
                    .is_some()
                {
                    prompt_tools.push(
                        register
                            .tool_group
                            .as_ref()
                            .expect("group with prompt")
                            .name
                            .clone(),
                    );
                }
                tools.push(register.tool.name);
            }
            _ => {}
        }
    }

    assert!(saw_subscription);
    assert!(
        tools
            .iter()
            .any(|tool| tool.as_str() == "email_list_folders")
    );
    assert!(tools.iter().any(|tool| tool.as_str() == "email_send"));
    assert!(
        tools
            .iter()
            .any(|tool| tool.as_str() == "calendar_list_calendars")
    );
    assert!(tools.iter().any(|tool| tool.as_str() == "calendar_respond"));
    assert!(prompt_tools.iter().any(|group| group.as_str() == "email"));
    assert!(
        prompt_tools
            .iter()
            .any(|group| group.as_str() == "calendar")
    );
    assert!(
        per_tool_prompt_tools
            .iter()
            .any(|tool| tool.as_str() == "email_get")
    );
    assert!(
        per_tool_prompt_tools
            .iter()
            .any(|tool| tool.as_str() == "calendar_get")
    );
    assert!(!tools.iter().any(|tool| tool.as_str() == email::TOOL_NAME));
    assert!(
        !tools
            .iter()
            .any(|tool| tool.as_str() == calendar::TOOL_NAME)
    );
    assert_eq!(tools.len(), 18);
}
