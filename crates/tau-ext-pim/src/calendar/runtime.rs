use tau_proto::{CborValue, Event, ToolError, ToolResult, ToolStarted};

use super::actions;
use super::config::{CalendarExtensionConfig, ValidatedConfig};
use super::tool::{CalendarCommand, ToolInvocation};

const LIST_ACCOUNTS_FORMAT: &str =
    "format: id flags backend default_calendar timezone display_name";

/// Runtime state for the calendar module.
#[derive(Default)]
pub struct RuntimeState {
    config_state: ConfigState,
}

#[derive(Default)]
enum ConfigState {
    #[default]
    Unconfigured,
    Configured(ValidatedConfig),
    Rejected {
        reason: String,
    },
}

impl RuntimeState {
    /// Configure the calendar module from an already-decoded calendar config.
    pub fn configure_with_config(&mut self, cfg: CalendarExtensionConfig) -> Result<(), String> {
        match cfg.validate() {
            Ok(config) => {
                self.config_state = ConfigState::Configured(config);
                Ok(())
            }
            Err(message) => {
                self.config_state = ConfigState::Rejected {
                    reason: message.clone(),
                };
                Err(message)
            }
        }
    }

    /// Dispatch a model-visible `calendar` tool invocation.
    pub fn dispatch(&mut self, invoke: ToolStarted) -> Event {
        let result = match &self.config_state {
            ConfigState::Configured(config) => dispatch_configured(config, &invoke.arguments),
            ConfigState::Unconfigured => Err("calendar module has not been configured".to_owned()),
            ConfigState::Rejected { reason } => Err(format!(
                "calendar module configuration was rejected: {reason}"
            )),
        };
        match result {
            Ok(text) => tool_result(invoke, text),
            Err(message) => tool_error(invoke, message),
        }
    }

    /// Dispatch a user `/calendar` action invocation.
    pub fn dispatch_action(&mut self, invoke: tau_proto::ActionInvoke) -> Event {
        actions::dispatch_action(invoke)
    }
}

fn dispatch_configured(config: &ValidatedConfig, arguments: &CborValue) -> Result<String, String> {
    let invocation: ToolInvocation = arguments
        .deserialized()
        .map_err(|error| format!("invalid calendar tool arguments: {error}"))?;
    let ToolInvocation { command, args } = invocation;
    drop(args);
    match command {
        CalendarCommand::ListAccounts => Ok(list_accounts(config)),
        CalendarCommand::ListCalendars
        | CalendarCommand::ListEvents
        | CalendarCommand::ReadEvent
        | CalendarCommand::FreeBusy => Err(read_not_implemented(command)),
        CalendarCommand::CreateEvent
        | CalendarCommand::UpdateEvent
        | CalendarCommand::DeleteEvent
        | CalendarCommand::RespondInvite => Err(write_not_implemented(command)),
    }
}

fn list_accounts(config: &ValidatedConfig) -> String {
    let mut lines = vec![LIST_ACCOUNTS_FORMAT.to_owned()];
    if !config.enable {
        return lines.join("\n");
    }
    for account_id in &config.account_order {
        let Some(account) = config.accounts.get(account_id) else {
            continue;
        };
        if !account.enable {
            continue;
        }
        let backend = account.backend_kind.unwrap_or("none");
        let default_calendar = account.default_calendar.as_deref().unwrap_or("-");
        let timezone = account.timezone.as_deref().unwrap_or("-");
        let display_name = account.display_name.as_deref().unwrap_or("-");
        lines.push(format!(
            "{} {} {} {} {} {}",
            safe_field(&account.id),
            "enabled",
            safe_field(backend),
            safe_field(default_calendar),
            safe_field(timezone),
            safe_field(display_name)
        ));
    }
    lines.join("\n")
}

fn read_not_implemented(command: CalendarCommand) -> String {
    format!(
        "calendar command `{}` needs a real backend",
        command_name(command)
    )
}

fn write_not_implemented(command: CalendarCommand) -> String {
    format!(
        "calendar command `{}` needs approval state and a real backend",
        command_name(command)
    )
}

fn command_name(command: CalendarCommand) -> &'static str {
    match command {
        CalendarCommand::ListAccounts => "list_accounts",
        CalendarCommand::ListCalendars => "list_calendars",
        CalendarCommand::ListEvents => "list_events",
        CalendarCommand::ReadEvent => "read_event",
        CalendarCommand::FreeBusy => "free_busy",
        CalendarCommand::CreateEvent => "create_event",
        CalendarCommand::UpdateEvent => "update_event",
        CalendarCommand::DeleteEvent => "delete_event",
        CalendarCommand::RespondInvite => "respond_invite",
    }
}

fn tool_result(invoke: ToolStarted, result: String) -> Event {
    Event::ToolResult(ToolResult {
        call_id: invoke.call_id,
        tool_name: invoke.tool_name,
        tool_type: tau_proto::ToolType::Function,
        result: CborValue::Text(result),
        kind: tau_proto::ToolResultKind::Final,
        display: None,
        originator: tau_proto::PromptOriginator::User,
    })
}

fn tool_error(invoke: ToolStarted, message: String) -> Event {
    Event::ToolError(ToolError {
        call_id: invoke.call_id,
        tool_name: invoke.tool_name,
        tool_type: tau_proto::ToolType::Function,
        message,
        details: None,
        display: None,
        originator: tau_proto::PromptOriginator::User,
    })
}

fn safe_field(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("_")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::config::{CalendarAccountConfig, CalendarBackendConfig};

    #[test]
    fn list_accounts_reports_enabled_configured_accounts() {
        let cfg = CalendarExtensionConfig {
            enable: true,
            accounts: vec![CalendarAccountConfig {
                id: "work".to_owned(),
                enable: true,
                display_name: Some("Work Calendar".to_owned()),
                backend: Some(CalendarBackendConfig::Google {
                    oauth_profile: Some("work".to_owned()),
                }),
                calendars: Default::default(),
                timezone: Some("UTC".to_owned()),
            }],
        };
        let config = cfg.validate().expect("valid calendar config");

        assert_eq!(
            list_accounts(&config),
            "format: id flags backend default_calendar timezone display_name\nwork enabled google - UTC Work_Calendar"
        );
    }

    #[test]
    fn duplicate_account_ids_are_rejected() {
        let cfg = CalendarExtensionConfig {
            enable: true,
            accounts: vec![
                CalendarAccountConfig {
                    id: "work".to_owned(),
                    ..Default::default()
                },
                CalendarAccountConfig {
                    id: "work".to_owned(),
                    ..Default::default()
                },
            ],
        };

        let err = match cfg.validate() {
            Ok(_) => panic!("duplicate ids should fail"),
            Err(err) => err,
        };
        assert!(err.contains("duplicate calendar account id"), "{err}");
    }
}
