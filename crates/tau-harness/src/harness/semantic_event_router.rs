//! Semantic persistence classification for harness events.

use tau_proto::{Event, SessionId};

/// Return whether an event should enter durable semantic stores.
///
/// Transient events normally exist only for live observers. Terminal tool
/// events are the exception: they must still be persisted so resumed agents can
/// see tool completions that happened after a transient dispatch path.
pub(crate) fn should_persist_event(event: &Event, transient: bool) -> bool {
    !transient || is_transient_tool_terminal_event(event)
}

/// Return the session log target for session membership events.
pub(crate) fn session_membership_id_for_event(event: &Event) -> Option<SessionId> {
    match event {
        Event::SessionAgentLoaded(loaded) => Some(loaded.session_id.clone()),
        Event::SessionAgentUnloaded(unloaded) => Some(unloaded.session_id.clone()),
        _ => None,
    }
}

fn is_transient_tool_terminal_event(event: &Event) -> bool {
    matches!(
        event,
        Event::ToolResult(_)
            | Event::ToolError(_)
            | Event::ProviderToolResult(_)
            | Event::ProviderToolError(_)
            | Event::ToolCancelled(_)
            | Event::ToolBackgroundResult(_)
            | Event::ToolBackgroundError(_)
    )
}

#[cfg(test)]
mod tests {
    use tau_proto::{
        AgentStarted, Event, PromptOriginator, SessionAgentLoaded, SessionAgentUnloaded, ToolError,
        ToolName, ToolType,
    };

    use super::{session_membership_id_for_event, should_persist_event};
    use crate::parse_agent_id;

    #[test]
    fn transient_non_tool_event_is_not_persisted() {
        let event = Event::AgentStarted(AgentStarted {
            agent_id: parse_agent_id("agent-1"),
            role: "default".into(),
            display_name: None,
        });

        assert!(!should_persist_event(&event, true));
        assert!(should_persist_event(&event, false));
    }

    #[test]
    fn transient_terminal_tool_event_is_persisted() {
        let event = Event::ToolError(ToolError {
            call_id: "call-1".into(),
            tool_name: ToolName::new("tool"),
            tool_type: ToolType::Function,
            message: "failed".to_owned(),
            details: None,
            display: None,
            originator: PromptOriginator::User,
        });

        assert!(should_persist_event(&event, true));
    }

    #[test]
    fn session_membership_events_route_to_session_log() {
        let loaded = Event::SessionAgentLoaded(SessionAgentLoaded {
            session_id: "session-1".into(),
            agent_id: parse_agent_id("agent-1"),
        });
        let unloaded = Event::SessionAgentUnloaded(SessionAgentUnloaded {
            session_id: "session-2".into(),
            agent_id: parse_agent_id("agent-1"),
        });

        assert_eq!(
            session_membership_id_for_event(&loaded).as_deref(),
            Some("session-1")
        );
        assert_eq!(
            session_membership_id_for_event(&unloaded).as_deref(),
            Some("session-2")
        );
    }
}
