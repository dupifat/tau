use std::path::PathBuf;

use tau_proto::{Event, PromptMessageClass, PromptOriginator, UiCreateAgent};

/// Default role used when the UI submits a prompt without an explicit selected
/// role from session state.
pub(crate) const DEFAULT_AGENT_ROLE: &str = "senior-engineer";

/// Build the standard user-originated create-agent event used by interactive
/// chat and one-shot/headless prompt submission paths.
pub(crate) fn create_user_agent_prompt(
    session_id: &str,
    role: impl Into<String>,
    prompt: impl Into<String>,
) -> Event {
    Event::UiCreateAgent(UiCreateAgent {
        session_id: session_id.into(),
        role: role.into(),
        cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        initial_prompt: Some(prompt.into()),
        message_class: PromptMessageClass::User,
        originator: PromptOriginator::User,
        ctx_id: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_role_matches_built_in_harness_default() {
        let built_in = tau_config::settings::HarnessSettings::built_in();
        assert_eq!(built_in.default_role.as_deref(), Some(DEFAULT_AGENT_ROLE));
    }
}
