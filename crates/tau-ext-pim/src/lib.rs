//! Standard personal information management extension.
//!
//! The extension currently preserves the existing controlled `email` tool and
//! introduces the `calendar` tool surface. Calendar backends are added
//! incrementally behind the same extension boundary.

use std::error::Error;
use std::io::{BufReader, BufWriter, Read, Write};

use serde::Deserialize;
use tau_proto::{
    Ack, ActionSchema, CborValue, ConfigError, Event, EventLogSeq, Frame, FrameReader, FrameWriter,
    Message,
};

pub mod calendar;
pub mod email;

/// `tracing` target for extension-level events emitted by the PIM wrapper.
pub const LOG_TARGET: &str = "pim";

/// Run the extension over stdio.
pub fn run_stdio() -> Result<(), Box<dyn Error>> {
    tau_extension::init_logging_for(LOG_TARGET);
    run(std::io::stdin(), std::io::stdout())
}

/// Run the extension over the supplied reader/writer pair.
pub fn run<R, W>(reader: R, writer: W) -> Result<(), Box<dyn Error>>
where
    R: Read,
    W: Write,
{
    let mut reader = FrameReader::new(BufReader::new(reader));
    let mut writer = FrameWriter::new(BufWriter::new(writer));
    let mut runtime = RuntimeState::default();

    tau_extension::Handshake::tool("tau-ext-pim")
        .subscribe([
            tau_proto::EventName::TOOL_STARTED,
            tau_proto::EventName::ACTION_INVOKE,
        ])
        .register_tool_with_prompt_fragment(
            email::email_tool_spec(),
            Some(email::email_prompt_fragment()),
        )
        .register_tool_with_prompt_fragment(
            calendar::calendar_tool_spec(),
            Some(calendar::calendar_prompt_fragment()),
        )
        .publish_actions(action_schema())
        .ready_message("pim extension ready")
        .run(&mut writer)?;

    while let Some(frame) = reader.read_frame()? {
        let (log_id, inner) = frame.peel_log();
        match inner {
            Frame::Message(Message::Configure(configure)) => {
                if let Err(message) = runtime.configure(configure) {
                    writer.write_frame(&Frame::Message(Message::ConfigError(ConfigError {
                        message,
                    })))?;
                    writer.flush()?;
                }
            }
            Frame::Event(Event::ToolStarted(invoke)) => {
                if let Some(event) = runtime.dispatch_tool(invoke) {
                    writer.write_frame(&Frame::Event(event))?;
                    writer.flush()?;
                }
            }
            Frame::Event(Event::ActionInvoke(invoke)) => {
                let event = runtime.dispatch_action(invoke);
                writer.write_frame(&Frame::Event(event))?;
                writer.flush()?;
            }
            Frame::Message(Message::Disconnect(_)) => break,
            _ => {}
        }
        if let Some(id) = log_id {
            ack_log_event(id, &mut writer)?;
        }
    }

    Ok(())
}

#[derive(Default)]
struct RuntimeState {
    email: email::RuntimeState,
    calendar: calendar::RuntimeState,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct PimExtensionConfig {
    email: Option<email::EmailExtensionConfig>,
    calendar: Option<calendar::CalendarExtensionConfig>,
}

impl RuntimeState {
    fn configure(&mut self, configure: tau_proto::Configure) -> Result<(), String> {
        match tau_extension::parse_config::<PimExtensionConfig>(&configure.config) {
            Ok(pim) => self.configure_pim(pim, configure),
            Err(message) if has_pim_module_keys(&configure.config) => Err(message),
            Err(_) => {
                let calendar_secrets = configure.secrets.clone();
                let calendar_state_dir = configure.state_dir.clone();
                self.email.configure(configure)?;
                self.calendar.configure_with_config(
                    calendar::CalendarExtensionConfig::default(),
                    calendar_state_dir,
                    calendar_secrets,
                )
            }
        }
    }

    fn configure_pim(
        &mut self,
        pim: PimExtensionConfig,
        configure: tau_proto::Configure,
    ) -> Result<(), String> {
        let email_config = pim.email.unwrap_or_default();
        let calendar_config = pim.calendar.unwrap_or_default();
        calendar_config.clone().validate()?;
        self.email.configure_with_config(
            email_config,
            configure.state_dir.clone(),
            configure.secrets.clone(),
        )?;
        self.calendar
            .configure_with_config(calendar_config, configure.state_dir, configure.secrets)
    }

    fn dispatch_tool(&mut self, invoke: tau_proto::ToolStarted) -> Option<Event> {
        match invoke.tool_name.as_str() {
            email::TOOL_NAME => Some(self.email.dispatch(invoke)),
            calendar::TOOL_NAME => Some(self.calendar.dispatch(invoke)),
            _ => None,
        }
    }

    fn dispatch_action(&mut self, invoke: tau_proto::ActionInvoke) -> Event {
        if invoke.action_id.starts_with("email.") {
            self.email.dispatch_action(invoke)
        } else if invoke.action_id.starts_with("calendar.") {
            self.calendar.dispatch_action(invoke)
        } else {
            Event::ActionError(tau_proto::ActionError {
                invocation_id: invoke.invocation_id,
                action_id: invoke.action_id,
                message: "unknown pim action".to_owned(),
                details: None,
            })
        }
    }
}

fn has_pim_module_keys(config: &CborValue) -> bool {
    let CborValue::Map(entries) = config else {
        return false;
    };
    entries.iter().any(|(key, _)| match key {
        CborValue::Text(key) => key == "email" || key == "calendar",
        _ => false,
    })
}

fn action_schema() -> ActionSchema {
    let mut schema = ActionSchema {
        version: tau_proto::ACTION_SCHEMA_VERSION,
        roots: Vec::new(),
    };
    schema.roots.extend(email::email_action_schema().roots);
    schema
        .roots
        .extend(calendar::calendar_action_schema().roots);
    schema
}

fn ack_log_event<W: Write>(
    id: EventLogSeq,
    writer: &mut FrameWriter<W>,
) -> Result<(), tau_proto::EncodeError> {
    writer.write_frame(&Frame::Message(Message::Ack(Ack { up_to: id })))?;
    writer.flush().map_err(tau_proto::EncodeError::Io)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde::Deserialize;
    use tau_proto::{EventName, EventSelector, ToolName};

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
        let invoke = tau_proto::ToolStarted {
            call_id: tau_proto::ToolCallId::new("call-read"),
            tool_name: tau_proto::ToolName::new("read"),
            arguments: CborValue::Map(vec![]),
            agent_id: tau_proto::AgentId::new("agent-1"),
            originator: tau_proto::PromptOriginator::User,
        };

        assert!(runtime.dispatch_tool(invoke).is_none());
    }

    #[test]
    fn handshake_registers_email_and_calendar_tools() {
        let mut bytes = Vec::new();
        tau_extension::Handshake::tool("tau-ext-pim")
            .subscribe([
                tau_proto::EventName::TOOL_STARTED,
                tau_proto::EventName::ACTION_INVOKE,
            ])
            .register_tool_with_prompt_fragment(
                email::email_tool_spec(),
                Some(email::email_prompt_fragment()),
            )
            .register_tool_with_prompt_fragment(
                calendar::calendar_tool_spec(),
                Some(calendar::calendar_prompt_fragment()),
            )
            .publish_actions(action_schema())
            .ready_message("pim extension ready")
            .run(&mut FrameWriter::new(&mut bytes))
            .expect("handshake writes");

        let mut reader = FrameReader::new(bytes.as_slice());
        let mut tools = Vec::new();
        let mut saw_subscription = false;
        while let Some(frame) = reader.read_frame().expect("frame decodes") {
            match frame {
                Frame::Message(Message::Subscribe(subscribe)) => {
                    saw_subscription = subscribe.selectors
                        == vec![
                            EventSelector::Exact(EventName::TOOL_STARTED),
                            EventSelector::Exact(EventName::ACTION_INVOKE),
                        ];
                }
                Frame::Event(Event::ToolRegister(register)) => tools.push(register.tool.name),
                _ => {}
            }
        }

        assert!(saw_subscription);
        assert_eq!(
            tools,
            vec![
                ToolName::new(email::TOOL_NAME),
                ToolName::new(calendar::TOOL_NAME)
            ]
        );
    }
}
