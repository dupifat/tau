//! [`DebugEventLog`]: append-only JSONL log of every harness event for
//! offline inspection.

use std::path::{Path, PathBuf};

use tau_proto::{ConnectionId, Event, UnixMicros};

use crate::error::HarnessError;
use crate::event::HarnessEvent;

/// Append-only JSON event log for debugging.
pub(crate) struct DebugEventLog {
    path: PathBuf,
    file: std::fs::File,
}

impl DebugEventLog {
    pub(crate) fn open(dir: &Path) -> Result<Self, HarnessError> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("events.jsonl");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        Ok(Self { path, file })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn log_harness_event(&mut self, harness_event: &HarnessEvent) {
        // Stamped on every line — including incoming-frame and
        // lifecycle entries that aren't event-log emissions — so an
        // offline reader can compute inter-event gaps and bursts
        // across the entire harness, not just the durable subset.
        let recorded_at = UnixMicros::now().get();
        let entry = match harness_event {
            HarnessEvent::FromConnection {
                connection_id,
                frame,
            } => {
                let frame_json = serde_json::to_value(frame).unwrap_or_default();
                let name = match frame.as_ref() {
                    tau_proto::Frame::Event(event) => event.name().to_string(),
                    tau_proto::Frame::Message(_) => "<message>".to_owned(),
                };
                serde_json::json!({
                    "type": "from_connection",
                    "recorded_at_micros": recorded_at,
                    "source": connection_id,
                    "event_name": name,
                    "event": frame_json,
                })
            }
            HarnessEvent::Disconnected { connection_id } => {
                serde_json::json!({
                    "type": "disconnected",
                    "recorded_at_micros": recorded_at,
                    "source": connection_id,
                })
            }
            HarnessEvent::NewClient(_) => {
                serde_json::json!({
                    "type": "new_client",
                    "recorded_at_micros": recorded_at,
                })
            }
        };
        self.write_entry(&entry);
    }

    /// Logs an event the harness committed (broadcast onto the bus).
    /// Captures the *enriched* payload — for `AgentResponseFinished`
    /// that's the harness-built `token_usage` with model and running
    /// session stats, which the inbound `from_connection` line could
    /// not carry. Together with `log_harness_event`, an offline reader
    /// can correlate the raw agent emit against the enriched committed
    /// copy.
    pub(crate) fn log_published_event(
        &mut self,
        source: Option<&ConnectionId>,
        event: &Event,
        recorded_at: UnixMicros,
    ) {
        let event_json = serde_json::to_value(event).unwrap_or_default();
        let entry = serde_json::json!({
            "type": "published",
            "recorded_at_micros": recorded_at.get(),
            "source": source,
            "event_name": event.name(),
            "event": event_json,
        });
        self.write_entry(&entry);
    }

    fn write_entry(&mut self, entry: &serde_json::Value) {
        use std::io::Write;
        let _ = serde_json::to_writer(&mut self.file, entry);
        let _ = self.file.write_all(b"\n");
        let _ = self.file.flush();
    }
}

#[cfg(test)]
mod tests {
    use tau_proto::{AgentResponseFinished, AgentTokenUsage, ModelId, SessionPromptId};

    use super::*;

    fn read_lines(path: &Path) -> Vec<serde_json::Value> {
        let raw = std::fs::read_to_string(path).expect("read events.jsonl");
        raw.lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str::<serde_json::Value>(l).expect("parse line"))
            .collect()
    }

    #[test]
    fn published_line_preserves_enriched_token_usage() {
        let td = tempfile::tempdir().expect("tempdir");
        let mut log = DebugEventLog::open(td.path()).expect("open");
        let model: ModelId = "openai/gpt-5".parse().expect("model id");
        let event = Event::AgentResponseFinished(AgentResponseFinished {
            session_prompt_id: SessionPromptId::from("sp-0"),
            input_tokens: Some(1000),
            cached_tokens: Some(800),
            output_tokens: Some(42),
            token_usage: Some(AgentTokenUsage {
                model: Some(model),
                prompt_sent_tokens: 1000,
                prompt_cached_tokens: 800,
                response_received_tokens: 42,
                stats: tau_proto::TokenUsageStats::default(),
            }),
            ..AgentResponseFinished::default()
        });
        log.log_published_event(
            Some(&ConnectionId::from("conn-1")),
            &event,
            UnixMicros::now(),
        );

        let lines = read_lines(log.path());
        assert_eq!(lines.len(), 1);
        let line = &lines[0];
        assert_eq!(line["type"], "published");
        assert_eq!(line["event_name"], "agent.response_finished");
        assert_eq!(line["source"], "conn-1");
        let usage = &line["event"]["payload"]["token_usage"];
        assert_eq!(usage["prompt_sent_tokens"], 1000);
        assert_eq!(usage["prompt_cached_tokens"], 800);
        assert_eq!(usage["response_received_tokens"], 42);
        assert_eq!(usage["model"], "openai/gpt-5");
    }
}
