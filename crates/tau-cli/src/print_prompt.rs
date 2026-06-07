use std::io::Write;

use tau_harness::SessionLaunchStatus;
use tau_proto::{HarnessInputMessage, HarnessOutputMessage};

use crate::daemon::{DaemonCliOverrides, DaemonHandle, daemon_output_for_session, resolve_daemon};
use crate::render_request::RenderResponse;
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
    crate::render_request::request_rendered_value(
        daemon,
        "tau-print-prompt",
        "tau-rendered-system-prompt",
        |request_id| {
            HarnessInputMessage::GetRenderedSystemPrompt(tau_proto::GetRenderedSystemPrompt {
                request_id,
                role: role.to_owned(),
            })
        },
        |message, request_id| match message {
            HarnessOutputMessage::RenderedSystemPromptResult(result)
                if result.request_id == request_id =>
            {
                let prompt = if let Some(error) = result.error {
                    Err(CliError::Participant(error))
                } else {
                    result.prompt.ok_or_else(|| {
                        CliError::Participant(
                            "daemon returned no rendered system prompt".to_owned(),
                        )
                    })
                };
                RenderResponse::Matched(prompt)
            }
            _ => RenderResponse::Ignore,
        },
    )
}
