//! [`DebugEventLog`]: append-only JSONL log of every harness event for
//! offline inspection.

use std::path::{Path, PathBuf};

use tau_proto::UnixMicros;

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
        use std::io::Write;
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
        let _ = serde_json::to_writer(&mut self.file, &entry);
        let _ = self.file.write_all(b"\n");
        let _ = self.file.flush();
    }
}
