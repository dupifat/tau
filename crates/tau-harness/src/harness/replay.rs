//! Late-subscriber replay.
//!
//! When a UI client subscribes after the harness has already emitted
//! events, two replay paths catch it up. Extension subscriptions do not
//! enter these paths today; their `Subscribe` only changes live routing.
//!
//! - [`Harness::replay_session_events`] announces the current loaded-agent
//!   snapshot, then replays each loaded agent's durable transcript facts from
//!   the global agent store.
//! - [`Harness::replay_harness_info`] re-emits harness/extension lifecycle
//!   events from the in-memory [`crate::event_log::EventLog`], plus the current
//!   model / effort / context-usage state, so a UI that just joined sees the
//!   same banners and indicators as one that was here from the start.

use tau_proto::{
    ActionSchemaPublished, AgentPromptQueued, Event, EventSelector, Frame,
    HarnessContextUsageChanged, HarnessModelsAvailable, HarnessRoleSelected, HarnessRolesAvailable,
    Message,
};

use crate::harness::{Harness, selector_matches_event};
use crate::model::{
    baseline_params_for_selection, context_window_for_model, efforts_for_model, role_infos,
    thinking_summaries_for_model, verbosities_for_model,
};

impl Harness {
    pub(crate) fn replay_session_events(&mut self, client_id: &str, selectors: &[EventSelector]) {
        let loaded_agents: Vec<tau_proto::AgentId> = {
            match self.store.load_session(self.current_session_id.as_str()) {
                Ok(Some(membership)) => membership.loaded_agents().into_iter().cloned().collect(),
                _ => Vec::new(),
            }
        };

        for agent_id in &loaded_agents {
            let event = Event::SessionAgentLoaded(tau_proto::SessionAgentLoaded {
                session_id: self.current_session_id.clone(),
                agent_id: agent_id.clone(),
            });
            if selector_matches_event(selectors, &event) {
                let _ = self.bus.send_to(client_id, None, Frame::Event(event));
            }
        }

        for agent_id in loaded_agents {
            let Ok(events) = self.agent_store.agent_events(agent_id.as_str()) else {
                continue;
            };
            for entry in events {
                if selector_matches_event(selectors, &entry.event)
                    && should_replay_agent_event_to_late_subscriber(&entry.event)
                {
                    let frame = Frame::Message(Message::LogEvent(tau_proto::LogEvent {
                        id: entry.id,
                        recorded_at: entry.recorded_at,
                        event: Box::new(entry.event),
                    }));
                    let _ = self.bus.send_to(client_id, entry.source.as_deref(), frame);
                }
            }
        }
        self.replay_active_queued_prompts(client_id, selectors);
    }

    fn replay_active_queued_prompts(&mut self, client_id: &str, selectors: &[EventSelector]) {
        let mut agent_by_conversation = std::collections::HashMap::new();
        for (agent_id, conversation_id) in &self.agent_routes {
            agent_by_conversation.insert(conversation_id.clone(), agent_id.clone());
        }

        for (conversation_id, conversation) in &self.agents {
            if conversation.session_id != self.current_session_id {
                continue;
            }
            let target_agent_id = agent_by_conversation.get(conversation_id).cloned();
            for prompt in &conversation.pending_prompts {
                let Some(agent_id) = target_agent_id.clone() else {
                    continue;
                };
                let event = Event::AgentPromptQueued(AgentPromptQueued {
                    agent_id: agent_id.into(),
                    text: prompt.text.clone(),
                    message_class: prompt.message_class,
                });
                if selector_matches_event(selectors, &event) {
                    let _ = self.bus.send_to(client_id, None, Frame::Event(event));
                }
            }
        }
    }

    /// Replays harness info and extension lifecycle events to a
    /// late-joining client.
    ///
    /// Runtime-only extension setup events are intentionally NOT replayed here.
    /// The transcript catch-up path above comes from durable agent logs, while
    /// this method only reconstructs current harness status snapshots.
    pub(crate) fn replay_harness_info(&mut self, client_id: &str, selectors: &[EventSelector]) {
        let mut cursor = 0;
        while let Some(entry) = self.event_log.get_next_from(cursor) {
            cursor = entry.seq + 1;
            let dominated = matches!(
                entry.event,
                Event::HarnessInfo(_)
                    | Event::HarnessSessionDir(_)
                    | Event::HarnessUiDir(_)
                    | Event::ExtensionStarting(_)
                    | Event::ExtensionReady(_)
                    | Event::ExtensionExited(_)
            );
            if dominated && selector_matches_event(selectors, &entry.event) {
                let _ = self.bus.send_to(
                    client_id,
                    entry.source.as_deref(),
                    Frame::Event(entry.event),
                );
            }
        }

        let mut provider_sources: Vec<_> =
            self.provider_models_by_extension.keys().cloned().collect();
        provider_sources.sort();
        for source_id in provider_sources {
            let Some(models) = self.provider_models_by_extension.get(&source_id).cloned() else {
                continue;
            };
            let provider_event =
                Event::ProviderModelsUpdated(tau_proto::ProviderModelsUpdated { models });
            if selector_matches_event(selectors, &provider_event) {
                let _ = self.bus.send_to(
                    client_id,
                    Some(source_id.as_str()),
                    Frame::Event(provider_event),
                );
            }
        }

        for published in self.action_registry.published_schemas() {
            let action_event = Event::ActionSchemaPublished(ActionSchemaPublished {
                extension_name: published.extension_name,
                instance_id: published.instance_id,
                schema: published.schema,
            });
            if selector_matches_event(selectors, &action_event) {
                let _ = self.bus.send_to(
                    client_id,
                    Some(published.connection_id.as_str()),
                    Frame::Event(action_event),
                );
            }
        }

        // Send current model state to the new client.
        let models_event = Event::HarnessModelsAvailable(HarnessModelsAvailable {
            models: self.available_models.clone(),
        });
        if selector_matches_event(selectors, &models_event) {
            let _ = self
                .bus
                .send_to(client_id, None, Frame::Event(models_event));
        }
        let roles_event = Event::HarnessRolesAvailable(HarnessRolesAvailable {
            roles: role_infos(
                &self.provider_model_info,
                &self.available_roles,
                &self.available_models,
            ),
            groups: self.current_role_groups(),
        });
        if selector_matches_event(selectors, &roles_event) {
            let _ = self.bus.send_to(client_id, None, Frame::Event(roles_event));
        }
        let (harness_settings, _) = crate::settings::load_harness_settings_or_warn(&self.dirs);
        let selected_event = Event::HarnessRoleSelected(HarnessRoleSelected {
            baseline_params: self.selected_model.as_ref().map(|model| {
                baseline_params_for_selection(
                    &harness_settings,
                    &self.provider_model_info,
                    &self.selected_role,
                    model,
                )
            }),
            model_params: self.selected_model_params(),
            model: self.selected_model.clone(),
            context_window: self
                .selected_model
                .as_ref()
                .and_then(|m| context_window_for_model(&self.provider_model_info, m)),
            role: self.selected_role.clone(),
        });
        if selector_matches_event(selectors, &selected_event) {
            let _ = self
                .bus
                .send_to(client_id, None, Frame::Event(selected_event));
        }
        let context_event = Event::HarnessContextUsageChanged(HarnessContextUsageChanged {
            input_tokens: self.current_session_state.context_input_tokens,
            cached_tokens: self.current_session_state.context_cached_tokens,
            percent_used: self.current_session_state.context_percent_used,
        });
        if selector_matches_event(selectors, &context_event) {
            let _ = self
                .bus
                .send_to(client_id, None, Frame::Event(context_event));
        }
        let effort_levels = self
            .selected_model
            .as_ref()
            .map(|m| efforts_for_model(&self.provider_model_info, m))
            .unwrap_or_default();
        let effort_levels_event =
            Event::HarnessEffortsAvailable(tau_proto::HarnessEffortsAvailable {
                levels: effort_levels,
            });
        if selector_matches_event(selectors, &effort_levels_event) {
            let _ = self
                .bus
                .send_to(client_id, None, Frame::Event(effort_levels_event));
        }
        let verbosity_levels = self
            .selected_model
            .as_ref()
            .map(|m| verbosities_for_model(&self.provider_model_info, m))
            .unwrap_or_default();
        let verbosity_levels_event =
            Event::HarnessVerbositiesAvailable(tau_proto::HarnessVerbositiesAvailable {
                levels: verbosity_levels,
            });
        if selector_matches_event(selectors, &verbosity_levels_event) {
            let _ = self
                .bus
                .send_to(client_id, None, Frame::Event(verbosity_levels_event));
        }
        let thinking_levels = self
            .selected_model
            .as_ref()
            .map(|m| thinking_summaries_for_model(&self.provider_model_info, m))
            .unwrap_or_default();
        let thinking_levels_event = Event::HarnessThinkingSummariesAvailable(
            tau_proto::HarnessThinkingSummariesAvailable {
                levels: thinking_levels,
            },
        );
        if selector_matches_event(selectors, &thinking_levels_event) {
            let _ = self
                .bus
                .send_to(client_id, None, Frame::Event(thinking_levels_event));
        }
    }
}

fn should_replay_agent_event_to_late_subscriber(event: &Event) -> bool {
    // Replay final, durable transcript facts, not progress. In particular, skip
    // provider streaming chunks and prompt-created pending markers, but keep the
    // agent-owned user/assistant/tool facts needed to reconstruct transcript UI.
    matches!(
        event,
        Event::AgentPromptSubmitted(_)
            | Event::AgentPromptSteered(_)
            | Event::AgentUserMessageInjected(_)
            | Event::AgentMessageSent(_)
            | Event::AgentMessageReceived(_)
            | Event::ProviderToolResult(_)
            | Event::ProviderToolError(_)
            | Event::ToolBackgroundResult(_)
            | Event::ToolBackgroundError(_)
            | Event::ToolCancelled(_)
            | Event::AgentCompacted(_)
            | Event::ProviderResponseFinished(_)
    )
}
