use std::io::Cursor;

use tau_proto::{Event, Frame, FrameReader, FrameWriter, InterceptRequest, Message, ToolInvoke};

use super::*;

fn invoke_restart() -> Frame {
    Frame::Event(Event::ToolInvoke(ToolInvoke {
        call_id: "call-1".into(),
        tool_name: tau_proto::ToolName::new(RESTART_TEST_DUMMY_TOOL_NAME),
        arguments: tau_proto::CborValue::Map(Vec::new()),
        originator: tau_proto::PromptOriginator::User,
    }))
}

#[test]
fn restart_tool_can_return_error() {
    let mut input = Vec::new();
    let mut writer = FrameWriter::new(&mut input);
    writer.write_frame(&invoke_restart()).expect("write invoke");
    writer.flush().expect("flush");

    let mut output = Vec::new();
    let mut rng = StdRng::seed_from_u64(1);
    run_with_rng(Cursor::new(input), &mut output, &mut rng).expect("run");

    let mut reader = FrameReader::new(Cursor::new(output));
    let hello = reader
        .read_frame()
        .expect("read")
        .expect("hello should exist");
    assert!(matches!(hello, Frame::Message(Message::Hello(_))));
    let intercept = reader
        .read_frame()
        .expect("read")
        .expect("intercept should exist");
    assert!(matches!(intercept, Frame::Message(Message::Intercept(_))));
    let register = reader
        .read_frame()
        .expect("read")
        .expect("register should exist");
    assert!(matches!(register, Frame::Event(Event::ToolRegister(_))));
    let ready = reader
        .read_frame()
        .expect("read")
        .expect("ready should exist");
    assert!(matches!(ready, Frame::Message(Message::Ready(_))));
    let error = reader
        .read_frame()
        .expect("read")
        .expect("error should exist");
    let Frame::Event(Event::ToolError(error)) = error else {
        panic!("expected tool error");
    };
    assert_eq!(error.message, "restarting failed");
    assert!(reader.read_frame().expect("read eof").is_none());
}

#[test]
fn restart_tool_can_exit_without_reply() {
    let mut input = Vec::new();
    let mut writer = FrameWriter::new(&mut input);
    writer.write_frame(&invoke_restart()).expect("write invoke");
    writer.flush().expect("flush");

    let mut output = Vec::new();
    let mut rng = StdRng::seed_from_u64(2);
    run_with_rng(Cursor::new(input), &mut output, &mut rng).expect("run");

    let mut reader = FrameReader::new(Cursor::new(output));
    let mut frames = Vec::new();
    while let Some(frame) = reader.read_frame().expect("read") {
        frames.push(frame);
    }
    assert_eq!(frames.len(), 4);
    assert!(matches!(frames[0], Frame::Message(Message::Hello(_))));
    assert!(matches!(frames[1], Frame::Message(Message::Intercept(_))));
    assert!(matches!(frames[2], Frame::Event(Event::ToolRegister(_))));
    assert!(matches!(frames[3], Frame::Message(Message::Ready(_))));
    // The restart-success branch must exit without emitting any
    // reply frame for the invoke — guard against a future bug that
    // re-introduces a stray ToolResult/ToolError before exit.
    assert!(
        frames.iter().all(|f| !matches!(
            f,
            Frame::Event(Event::ToolError(_)) | Frame::Event(Event::ToolResult(_))
        )),
        "no tool reply frame should appear in the restart-success branch"
    );
}

fn intercepted_prompt(text: &str) -> Frame {
    Frame::Message(Message::InterceptRequest(InterceptRequest {
        event: Box::new(Event::UiPromptSubmitted(tau_proto::UiPromptSubmitted {
            session_id: "s1".into(),
            text: text.to_owned(),
            message_class: tau_proto::PromptMessageClass::User,
            originator: tau_proto::PromptOriginator::User,
            ctx_id: None,
        })),
        transient: false,
    }))
}

fn run_intercept(prompt: &str) -> (Vec<tau_proto::Emit>, Vec<InterceptReply>) {
    let mut input = Vec::new();
    let mut writer = FrameWriter::new(&mut input);
    writer
        .write_frame(&intercepted_prompt(prompt))
        .expect("write intercepted prompt");
    writer.flush().expect("flush");

    let mut output = Vec::new();
    let mut rng = StdRng::seed_from_u64(1);
    run_with_rng(Cursor::new(input), &mut output, &mut rng).expect("run");

    let mut reader = FrameReader::new(Cursor::new(output));
    let mut emits = Vec::new();
    let mut replies = Vec::new();
    while let Some(frame) = reader.read_frame().expect("read") {
        match frame {
            Frame::Message(Message::Emit(emit)) => emits.push(emit),
            Frame::Message(Message::InterceptReply(reply)) => replies.push(reply),
            _ => {}
        }
    }
    (emits, replies)
}

fn replaced_prompt_text(reply: &InterceptReply) -> Option<String> {
    match &reply.action {
        tau_proto::InterceptAction::Pass(Some(boxed)) => match boxed.as_ref() {
            Event::UiPromptSubmitted(p) => Some(p.text.clone()),
            _ => None,
        },
        _ => None,
    }
}

#[test]
fn prompt_with_tao_is_corrected_with_notification() {
    let (emits, replies) = run_intercept("I love Tao");

    assert_eq!(emits.len(), 1, "exactly one info emit on correction");
    assert!(matches!(
        emits[0].event.as_ref(),
        Event::HarnessInfo(info) if info.message.contains("Tau") && info.message.contains("corrected")
    ));

    assert_eq!(replies.len(), 1);
    let replaced =
        replaced_prompt_text(&replies[0]).expect("intercept reply carries replacement event");
    assert_eq!(replaced, "I love Tau");
}

#[test]
fn prompt_correction_preserves_letter_case() {
    for (input, expected) in [
        ("tao", "tau"),
        ("Tao", "Tau"),
        ("TAO", "TAU"),
        ("tAo", "tAu"),
        ("TaO", "TaU"),
        ("the TAO of Tao and tao", "the TAU of Tau and tau"),
    ] {
        let (_, replies) = run_intercept(input);
        let replaced = replaced_prompt_text(&replies[0]).unwrap_or_else(|| {
            panic!("expected replacement for input {input:?}");
        });
        assert_eq!(replaced, expected, "case preservation for {input:?}");
    }
}

#[test]
fn prompt_correction_skips_substrings_inside_words() {
    // `tao` inside `chaotic` is just three letters, not the word —
    // don't touch it.
    let (emits, replies) = run_intercept("a chaotic taoism enjoyer");

    assert_eq!(emits.len(), 0, "no notification when no whole-word match");
    assert_eq!(replies.len(), 1);
    assert!(
        matches!(&replies[0].action, tau_proto::InterceptAction::Pass(None)),
        "no replacement when no whole-word match"
    );
}

#[test]
fn prompt_without_tao_passes_through_unchanged() {
    let (emits, replies) = run_intercept("hello world");

    assert_eq!(emits.len(), 0);
    assert_eq!(replies.len(), 1);
    assert!(matches!(
        &replies[0].action,
        tau_proto::InterceptAction::Pass(None)
    ));
}
