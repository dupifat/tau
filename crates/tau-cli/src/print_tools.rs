use std::io::Write;

use tau_harness::SessionLaunchStatus;
use tau_proto::{EventSelector, HarnessInputMessage, HarnessOutputMessage};

use crate::daemon::{DaemonCliOverrides, DaemonHandle, daemon_output_for_session, resolve_daemon};
use crate::ui_client::{UiInputReader, UiOutputWriter};
use crate::{CliError, mint_short_id};

pub(crate) fn run_print_tools(
    role: &str,
    role_cli_overrides: &[tau_config::settings::RoleCliOverride],
    extension_cli_overrides: &[tau_config::settings::ExtensionCliOverride],
    harness_config_overrides: &[tau_config::settings::HarnessConfigCliOverride],
) -> Result<(), CliError> {
    let session_id = mint_short_id("print-tools");
    let output = daemon_output_for_session(&session_id)?;
    let mut daemon = resolve_daemon(
        false,
        &session_id,
        SessionLaunchStatus::New,
        Some(output),
        Some(role),
        DaemonCliOverrides {
            role: role_cli_overrides,
            extension: extension_cli_overrides,
            harness_config: harness_config_overrides,
        },
    )?;

    let tools = get_rendered_tool_definitions(&mut daemon, role)?;

    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, &tools).map_err(|error| {
        CliError::Participant(format!("failed to serialize tool definitions: {error}"))
    })?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

fn get_rendered_tool_definitions(
    daemon: &mut DaemonHandle,
    role: &str,
) -> Result<Vec<tau_proto::ToolDefinition>, CliError> {
    let (mut reader, mut writer) = connect_print_tools_client(daemon)?;
    let request_id = crate::ui_client::next_request_id("tau-rendered-tools");
    crate::ui_client::send_message(
        &mut writer,
        &HarnessInputMessage::GetRenderedToolDefinitions(tau_proto::GetRenderedToolDefinitions {
            request_id: request_id.clone(),
            role: role.to_owned(),
        }),
    )?;
    loop {
        let Some(message) = reader.read_message().map_err(std::io::Error::other)? else {
            return Err(CliError::Participant("daemon disconnected".to_owned()));
        };
        match message {
            HarnessOutputMessage::RenderedToolDefinitionsResult(result)
                if result.request_id == request_id =>
            {
                disconnect_print_tools_client(&mut writer);
                if let Some(error) = result.error {
                    return Err(CliError::Participant(error));
                }
                return result.tools.ok_or_else(|| {
                    CliError::Participant("daemon returned no rendered tool definitions".to_owned())
                });
            }
            HarnessOutputMessage::Disconnect(disconnect) => {
                return Err(CliError::Participant(
                    disconnect
                        .reason
                        .unwrap_or_else(|| "daemon disconnected".to_owned()),
                ));
            }
            _ => {}
        }
    }
}

fn connect_print_tools_client(
    daemon: &mut DaemonHandle,
) -> Result<(UiInputReader, UiOutputWriter), CliError> {
    let (reader, mut writer) =
        crate::ui_client::connect_daemon_ui_client(daemon, "tau-print-tools")?;
    crate::ui_client::subscribe(&mut writer, Vec::<EventSelector>::new())?;
    Ok((reader, writer))
}

fn disconnect_print_tools_client(writer: &mut UiOutputWriter) {
    let _ = crate::ui_client::send_message(
        writer,
        &HarnessInputMessage::Disconnect(tau_proto::Disconnect {
            reason: Some("done".to_owned()),
        }),
    );
}
