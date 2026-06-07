use std::collections::BTreeMap;
use std::io::{Cursor, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use tau_proto::{
    CborValue, Configure, Event, EventLogSeq, EventSelector, HarnessInputMessage,
    HarnessInputReader, HarnessOutputMessage, HarnessOutputWriter, InterceptAction,
    InterceptRequest, InterceptionPriority, UnixMicros,
};

use super::*;

#[derive(Clone, Default)]
struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl SharedWriter {
    fn into_bytes(self) -> Vec<u8> {
        Arc::try_unwrap(self.0)
            .expect("single writer reference")
            .into_inner()
            .expect("writer mutex")
    }
}

impl Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("writer mutex").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn write_script(dir: &tempfile::TempDir, source: &str) -> std::path::PathBuf {
    let path = dir.path().join("hook.rhai");
    std::fs::write(&path, source).expect("write script");
    path
}

fn configure_with_script(path: &Path) -> HarnessOutputMessage {
    HarnessOutputMessage::Configure(Configure {
        config: CborValue::Map(vec![(
            CborValue::Text("script".to_owned()),
            CborValue::Text(path.display().to_string()),
        )]),
        state_dir: None,
        secrets: BTreeMap::new(),
    })
}

fn empty_configure() -> HarnessOutputMessage {
    HarnessOutputMessage::Configure(Configure {
        config: CborValue::Map(Vec::new()),
        state_dir: None,
        secrets: BTreeMap::new(),
    })
}

fn prompt_event(text: &str) -> Event {
    Event::AgentPromptSubmitted(tau_proto::AgentPromptSubmitted {
        agent_id: tau_proto::AgentId::parse("main").expect("agent id"),
        text: text.to_owned(),
        message_class: tau_proto::PromptMessageClass::User,
        originator: tau_proto::PromptOriginator::User,
        display_name: None,
        ctx_id: None,
    })
}

fn run_frames(input_frames: &[HarnessOutputMessage]) -> Vec<HarnessInputMessage> {
    let mut input = Vec::new();
    let mut writer = HarnessOutputWriter::new(&mut input);
    for frame in input_frames {
        writer.write_message(frame).expect("write input frame");
    }
    writer.flush().expect("flush input");

    let output = SharedWriter::default();
    run(Cursor::new(input), output.clone()).expect("run rhai extension");

    let mut reader = HarnessInputReader::new(Cursor::new(output.into_bytes()));
    let mut frames = Vec::new();
    while let Some(frame) = reader.read_message().expect("read output frame") {
        frames.push(frame);
    }
    frames
}

fn emitted_event(message: &HarnessInputMessage) -> Option<&Event> {
    match message {
        HarnessInputMessage::Emit(emit) => Some(emit.event.as_ref()),
        _ => None,
    }
}

#[test]
fn bootstrap_waits_for_configure_then_uses_init_plan() {
    // The Rhai extension must not send subscriptions until it has the
    // configured script, because the script decides its own event interest.
    let dir = tempfile::tempdir().expect("tempdir");
    let script = write_script(
        &dir,
        r#"
            fn init(config) {
                return #{
                    subscribe: [#{ kind: "exact", value: "agent.prompt_submitted" }],
                    ready_message: "demo ready",
                };
            }
        "#,
    );

    let frames = run_frames(&[configure_with_script(&script)]);

    assert!(matches!(frames[0], HarnessInputMessage::Hello(_)));
    assert!(matches!(frames[1], HarnessInputMessage::Subscribe(_)));
    assert!(matches!(frames[2], HarnessInputMessage::Ready(_)));
    let HarnessInputMessage::Ready(ready) = &frames[2] else {
        panic!("expected ready");
    };
    assert_eq!(ready.message.as_deref(), Some("demo ready"));
    assert_eq!(frames.len(), 3);
}

#[test]
fn no_op_init_uses_default_ready_message() {
    // A script can define init for future use without returning a map;
    // unit means the same as an absent init hook.
    let dir = tempfile::tempdir().expect("tempdir");
    let script = write_script(&dir, "fn init(config) {}\n");

    let frames = run_frames(&[configure_with_script(&script)]);

    assert!(matches!(frames[0], HarnessInputMessage::Hello(_)));
    let ready = frames
        .iter()
        .find_map(|frame| match frame {
            HarnessInputMessage::Ready(ready) => Some(ready),
            _ => None,
        })
        .expect("ready frame");
    assert_eq!(ready.message.as_deref(), Some("rhai ready"));
    assert!(
        frames
            .iter()
            .all(|frame| !matches!(frame, HarnessInputMessage::ConfigError(_)))
    );
}

#[test]
fn init_host_emit_failure_is_inert() {
    // Host emit helpers are intentionally unavailable during init so a
    // script that fails init cannot leak pre-Ready side effects.
    let dir = tempfile::tempdir().expect("tempdir");
    let script = write_script(
        &dir,
        r#"
            fn init(config) {
                tau_info("should not leak");
                fail;
            }
        "#,
    );

    let frames = run_frames(&[configure_with_script(&script)]);

    assert!(
        frames
            .iter()
            .any(|frame| matches!(frame, HarnessInputMessage::ConfigError(_)))
    );
    assert!(frames.iter().all(|frame| !matches!(
        emitted_event(frame),
        Some(Event::HarnessInfo(info)) if info.message.contains("should not leak")
    )));
}
#[test]
fn missing_script_config_reports_error_and_stays_inert() {
    // Missing scripts are configuration errors, but the process stays
    // alive long enough to avoid a harness restart loop.
    let frames = run_frames(&[empty_configure()]);

    assert!(matches!(frames[0], HarnessInputMessage::Hello(_)));
    assert!(matches!(frames[1], HarnessInputMessage::ConfigError(_)));
    assert!(matches!(frames[2], HarnessInputMessage::Ready(_)));
    let HarnessInputMessage::Ready(ready) = &frames[2] else {
        panic!("expected ready");
    };
    assert!(
        ready
            .message
            .as_deref()
            .is_some_and(|m| m.contains("disabled"))
    );
}

#[test]
fn delivered_event_invokes_script_and_acks_sequence() {
    // A delivered event is converted to the JSON-shaped Rhai map and
    // acknowledged after the callback has had a chance to run.
    let dir = tempfile::tempdir().expect("tempdir");
    let script = write_script(
        &dir,
        r#"
            fn init(config) {
                return #{ subscribe: [#{ kind: "exact", value: "agent.prompt_submitted" }] };
            }
            fn on_event(event, meta) {
                tau_info(`saw ${meta.seq}: ${event.payload.text}`);
            }
        "#,
    );
    let delivered = HarnessOutputMessage::deliver_sequenced(
        EventLogSeq::new(7),
        UnixMicros::new(11),
        prompt_event("hello"),
    );

    let frames = run_frames(&[configure_with_script(&script), delivered]);

    assert!(frames.iter().any(|frame| matches!(
        emitted_event(frame),
        Some(Event::HarnessInfo(info)) if info.message.contains("saw 7: hello")
    )));
    assert!(frames.iter().any(|frame| matches!(
        frame,
        HarnessInputMessage::Ack(ack) if ack.up_to == EventLogSeq::new(7)
    )));
}

#[test]
fn script_error_during_on_event_still_acks_and_reports() {
    // Callback errors are isolated to the failing callback. The ack is
    // still sent so one bad hook cannot wedge sequenced delivery.
    let dir = tempfile::tempdir().expect("tempdir");
    let script = write_script(
        &dir,
        r#"
            fn init(config) {
                return #{ subscribe: [#{ kind: "exact", value: "agent.prompt_submitted" }] };
            }
            fn on_event(event, meta) {
                unknown_function();
            }
        "#,
    );
    let delivered = HarnessOutputMessage::deliver_sequenced(
        EventLogSeq::new(8),
        UnixMicros::new(12),
        prompt_event("hello"),
    );

    let frames = run_frames(&[configure_with_script(&script), delivered]);

    assert!(frames.iter().any(|frame| matches!(
        frame,
        HarnessInputMessage::Ack(ack) if ack.up_to == EventLogSeq::new(8)
    )));
    assert!(frames.iter().any(|frame| matches!(
        emitted_event(frame),
        Some(Event::HarnessInfo(info)) if info.message.contains("on_event failed")
    )));
}

#[test]
fn init_merges_same_priority_intercepts() {
    // The harness stores one interceptor registration per connection, so
    // same-priority init entries are collapsed into one registration.
    let dir = tempfile::tempdir().expect("tempdir");
    let script = write_script(
        &dir,
        r#"
            fn init(config) {
                return #{
                    intercept: [
                        #{ selectors: [#{ kind: "exact", value: "agent.prompt_submitted" }], priority: 0 },
                        #{ selectors: [#{ kind: "prefix", value: "tool." }], priority: 0 },
                    ],
                };
            }
        "#,
    );

    let frames = run_frames(&[configure_with_script(&script)]);

    let intercepts: Vec<_> = frames
        .iter()
        .filter_map(|frame| match frame {
            HarnessInputMessage::Intercept(intercept) => Some(intercept),
            _ => None,
        })
        .collect();
    assert_eq!(intercepts.len(), 1);
    assert_eq!(intercepts[0].priority, InterceptionPriority::new(0));
    assert_eq!(intercepts[0].selectors.len(), 2);
    assert!(matches!(
        &intercepts[0].selectors[0],
        EventSelector::Exact(name) if name.to_string() == "agent.prompt_submitted"
    ));
    assert!(matches!(
        &intercepts[0].selectors[1],
        EventSelector::Prefix(prefix) if prefix == "tool."
    ));
}

#[test]
fn init_rejects_mixed_priority_intercepts() {
    // Multiple priority levels would require multiple harness
    // registrations, so the prototype rejects that script contract.
    let dir = tempfile::tempdir().expect("tempdir");
    let script = write_script(
        &dir,
        r#"
            fn init(config) {
                return #{
                    intercept: [
                        #{ selectors: [#{ kind: "exact", value: "agent.prompt_submitted" }], priority: 0 },
                        #{ selectors: [#{ kind: "prefix", value: "tool." }], priority: 1 },
                    ],
                };
            }
        "#,
    );

    let frames = run_frames(&[configure_with_script(&script)]);

    assert!(frames.iter().any(|frame| matches!(
        frame,
        HarnessInputMessage::ConfigError(error) if error.message.contains("same priority")
    )));
    assert!(frames.iter().any(|frame| matches!(
        frame,
        HarnessInputMessage::Ready(ready) if ready.message.as_deref().is_some_and(|m| m.contains("disabled"))
    )));
    assert!(
        frames
            .iter()
            .all(|frame| !matches!(frame, HarnessInputMessage::Intercept(_)))
    );
}
#[test]
fn intercept_callback_can_drop_event() {
    // Intercept callbacks must return exactly one InterceptReply. This
    // covers the simplest script-controlled policy: dropping an event.
    let dir = tempfile::tempdir().expect("tempdir");
    let script = write_script(
        &dir,
        r#"
            fn init(config) {
                return #{
                    intercept: [#{
                        selectors: [#{ kind: "exact", value: "agent.prompt_submitted" }],
                        priority: 0,
                    }],
                };
            }
            fn on_intercept(event, transient) { return "drop"; }
        "#,
    );
    let req = HarnessOutputMessage::InterceptRequest(InterceptRequest {
        event: Box::new(prompt_event("hello")),
        transient: false,
    });

    let frames = run_frames(&[configure_with_script(&script), req]);
    assert!(frames.iter().any(|frame| matches!(
        frame,
        HarnessInputMessage::InterceptReply(reply) if matches!(reply.action, InterceptAction::Drop)
    )));
}

#[test]
fn intercept_callback_can_return_replacement_event() {
    // A script can mutate the JSON-shaped event map and pass the
    // replacement back through Rust deserialization.
    let dir = tempfile::tempdir().expect("tempdir");
    let script = write_script(
        &dir,
        r#"
            fn init(config) {
                return #{
                    intercept: [#{
                        selectors: [#{ kind: "exact", value: "agent.prompt_submitted" }],
                        priority: 0,
                    }],
                };
            }
            fn on_intercept(event, transient) {
                event.payload.text = "changed";
                return #{ kind: "pass", event: event };
            }
        "#,
    );
    let req = HarnessOutputMessage::InterceptRequest(InterceptRequest {
        event: Box::new(prompt_event("hello")),
        transient: false,
    });

    let frames = run_frames(&[configure_with_script(&script), req]);

    let replacement = frames.iter().find_map(|frame| match frame {
        HarnessInputMessage::InterceptReply(reply) => match &reply.action {
            InterceptAction::Pass(Some(event)) => Some(event.as_ref()),
            _ => None,
        },
        _ => None,
    });
    assert!(matches!(
        replacement,
        Some(Event::AgentPromptSubmitted(prompt)) if prompt.text == "changed"
    ));
}
