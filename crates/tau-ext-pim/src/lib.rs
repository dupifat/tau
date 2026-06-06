//! Standard personal information management extension.
//!
//! The extension exposes split email and calendar command tools while keeping
//! shared configuration, approval, and runtime boundaries inside one extension.

use std::error::Error;
use std::io::{BufReader, BufWriter, Read, Write};

use serde::Deserialize;
use tau_proto::{
    Ack, ActionSchema, CborValue, ConfigError, Event, EventLogSeq, HarnessInputMessage,
    HarnessOutputMessage, PeerInputReader, PeerOutputWriter,
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
    let mut reader = PeerInputReader::new(BufReader::new(reader));
    let mut writer = PeerOutputWriter::new(BufWriter::new(writer));
    let mut runtime = RuntimeState::default();

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
        .run(&mut writer)?;

    while let Some(message) = reader.read_message()? {
        if !handle_message(&mut runtime, message, &mut writer)? {
            break;
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

    fn initial_tool_progress(&self, invoke: &tau_proto::ToolStarted) -> Option<Event> {
        match invoke.tool_name.as_str() {
            name if email::is_tool_name(name) => {
                Some(Event::ToolProgress(tau_proto::ToolProgress {
                    call_id: invoke.call_id.clone(),
                    tool_name: invoke.tool_name.clone(),
                    message: None,
                    progress: None,
                    display: Some(email::initial_display_for_tool(
                        invoke.tool_name.as_str(),
                        &invoke.arguments,
                    )),
                }))
            }
            name if calendar::is_tool_name(name) => Some(calendar::initial_progress(invoke)),
            _ => None,
        }
    }

    fn dispatch_tool(&mut self, invoke: tau_proto::ToolStarted) -> Option<Event> {
        match invoke.tool_name.as_str() {
            name if email::is_tool_name(name) => Some(self.email.dispatch(invoke)),
            name if calendar::is_tool_name(name) => Some(self.calendar.dispatch(invoke)),
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
fn register_tools_with_prompt_fragment(
    mut handshake: tau_extension::Handshake,
    tools: Vec<tau_proto::ToolSpec>,
    group_name: tau_proto::ToolGroupName,
    prompt_tool_name: &str,
    prompt_fragment: tau_proto::PromptFragment,
) -> tau_extension::Handshake {
    let tool_group = tau_proto::ToolGroup {
        name: group_name,
        prompt_fragment: Some(prompt_fragment.clone()),
    };
    for tool in tools {
        let prompt_fragment = if tool.name.as_str() == prompt_tool_name {
            Some(prompt_fragment.clone())
        } else {
            None
        };
        handshake = handshake.register_tool_with_group_and_prompt_fragment(
            tool,
            Some(tool_group.clone()),
            prompt_fragment,
        );
    }
    handshake
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

fn handle_message<W: Write>(
    runtime: &mut RuntimeState,
    message: HarnessOutputMessage,
    writer: &mut PeerOutputWriter<W>,
) -> Result<bool, Box<dyn Error>> {
    match message {
        HarnessOutputMessage::Configure(configure) => {
            if let Err(message) = runtime.configure(configure) {
                writer.write_message(&HarnessInputMessage::ConfigError(ConfigError { message }))?;
                writer.flush()?;
            }
        }
        HarnessOutputMessage::Deliver(delivery) => {
            let (event, log_id, _) = delivery.into_parts();
            match event {
                Event::ToolStarted(invoke) => handle_tool_started(runtime, invoke, writer)?,
                Event::ActionInvoke(invoke) => {
                    let event = runtime.dispatch_action(invoke);
                    writer.write_message(&HarnessInputMessage::emit(event))?;
                    writer.flush()?;
                }
                _ => {}
            }
            if let Some(id) = log_id {
                ack_log_event(id, writer)?;
            }
        }
        HarnessOutputMessage::Disconnect(_) => return Ok(false),
        _ => {}
    }
    Ok(true)
}

fn handle_tool_started<W: Write>(
    runtime: &mut RuntimeState,
    invoke: tau_proto::ToolStarted,
    writer: &mut PeerOutputWriter<W>,
) -> Result<(), Box<dyn Error>> {
    if let Some(progress) = runtime.initial_tool_progress(&invoke) {
        writer.write_message(&HarnessInputMessage::emit(progress))?;
        writer.flush()?;
    }
    if let Some(event) = runtime.dispatch_tool(invoke) {
        writer.write_message(&HarnessInputMessage::emit(event))?;
        writer.flush()?;
    }
    Ok(())
}

fn ack_log_event<W: Write>(
    id: EventLogSeq,
    writer: &mut PeerOutputWriter<W>,
) -> Result<(), tau_proto::EncodeError> {
    writer.write_message(&HarnessInputMessage::Ack(Ack { up_to: id }))?;
    writer.flush().map_err(tau_proto::EncodeError::Io)
}

#[cfg(test)]
mod tests;
