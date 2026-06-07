use std::io::Write;

use tau_harness::SessionLaunchStatus;
use tau_proto::{EventSelector, HarnessInputMessage, HarnessOutputMessage};

use crate::daemon::{DaemonCliOverrides, DaemonHandle, daemon_output_for_session, resolve_daemon};
use crate::ui_client::{UiInputReader, UiOutputWriter};
use crate::{CliError, mint_short_id};

pub(crate) fn run_print_prompt(
    role: &str,
    role_cli_overrides: &[tau_config::settings::RoleCliOverride],
    extension_cli_overrides: &[tau_config::settings::ExtensionCliOverride],
    harness_config_overrides: &[tau_config::settings::HarnessConfigCliOverride],
) -> Result<(), CliError> {
    let session_id = mint_short_id("print-prompt");
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

    let prompt = get_rendered_system_prompt(&mut daemon, role)?;

    let mut stdout = std::io::stdout().lock();
    stdout.write_all(prompt.as_bytes())?;
    stdout.flush()?;
    Ok(())
}

fn get_rendered_system_prompt(daemon: &mut DaemonHandle, role: &str) -> Result<String, CliError> {
    let (mut reader, mut writer) = connect_print_prompt_client(daemon)?;
    let request_id = crate::ui_client::next_request_id("tau-rendered-system-prompt");
    crate::ui_client::send_message(
        &mut writer,
        &HarnessInputMessage::GetRenderedSystemPrompt(tau_proto::GetRenderedSystemPrompt {
            request_id: request_id.clone(),
            role: role.to_owned(),
        }),
    )?;
    loop {
        let Some(message) = reader.read_message().map_err(std::io::Error::other)? else {
            return Err(CliError::Participant("daemon disconnected".to_owned()));
        };
        match message {
            HarnessOutputMessage::RenderedSystemPromptResult(result)
                if result.request_id == request_id =>
            {
                disconnect_print_prompt_client(&mut writer);
                if let Some(error) = result.error {
                    return Err(CliError::Participant(error));
                }
                return result.prompt.ok_or_else(|| {
                    CliError::Participant("daemon returned no rendered system prompt".to_owned())
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

fn connect_print_prompt_client(
    daemon: &mut DaemonHandle,
) -> Result<(UiInputReader, UiOutputWriter), CliError> {
    let (reader, mut writer) =
        crate::ui_client::connect_daemon_ui_client(daemon, "tau-print-prompt")?;
    crate::ui_client::subscribe(&mut writer, Vec::<EventSelector>::new())?;
    Ok((reader, writer))
}

fn disconnect_print_prompt_client(writer: &mut UiOutputWriter) {
    let _ = crate::ui_client::send_message(
        writer,
        &HarnessInputMessage::Disconnect(tau_proto::Disconnect {
            reason: Some("done".to_owned()),
        }),
    );
}
