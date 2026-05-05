use std::io::Cursor;

use tau_proto::{Event, EventReader, ToolInvoke};

use super::*;

fn invoke_restart() -> Event {
    Event::ToolInvoke(ToolInvoke {
        call_id: "call-1".into(),
        tool_name: RESTART_TEST_DUMMY_TOOL_NAME.into(),
        arguments: tau_proto::CborValue::Map(Vec::new()),
    })
}

#[test]
fn restart_tool_can_return_error() {
    let mut input = Vec::new();
    let mut writer = EventWriter::new(&mut input);
    writer.write_event(&invoke_restart()).expect("write invoke");
    writer.flush().expect("flush");

    let mut output = Vec::new();
    let mut rng = StdRng::seed_from_u64(1);
    run_with_rng(Cursor::new(input), &mut output, &mut rng).expect("run");

    let mut reader = EventReader::new(Cursor::new(output));
    let hello = reader
        .read_event()
        .expect("read")
        .expect("hello should exist");
    assert!(matches!(hello, Event::LifecycleHello(_)));
    let subscribe = reader
        .read_event()
        .expect("read")
        .expect("subscribe should exist");
    assert!(matches!(subscribe, Event::LifecycleSubscribe(_)));
    let register = reader
        .read_event()
        .expect("read")
        .expect("register should exist");
    assert!(matches!(register, Event::ToolRegister(_)));
    let ready = reader
        .read_event()
        .expect("read")
        .expect("ready should exist");
    assert!(matches!(ready, Event::LifecycleReady(_)));
    let error = reader
        .read_event()
        .expect("read")
        .expect("error should exist");
    let Event::ToolError(error) = error else {
        panic!("expected tool error");
    };
    assert_eq!(error.message, "restarting failed");
    assert!(reader.read_event().expect("read eof").is_none());
}

#[test]
fn restart_tool_can_exit_without_reply() {
    let mut input = Vec::new();
    let mut writer = EventWriter::new(&mut input);
    writer.write_event(&invoke_restart()).expect("write invoke");
    writer.flush().expect("flush");

    let mut output = Vec::new();
    let mut rng = StdRng::seed_from_u64(2);
    run_with_rng(Cursor::new(input), &mut output, &mut rng).expect("run");

    let mut reader = EventReader::new(Cursor::new(output));
    let mut events = Vec::new();
    while let Some(event) = reader.read_event().expect("read") {
        events.push(event);
    }
    assert_eq!(events.len(), 4);
    assert!(matches!(events[0], Event::LifecycleHello(_)));
    assert!(matches!(events[1], Event::LifecycleSubscribe(_)));
    assert!(matches!(events[2], Event::ToolRegister(_)));
    assert!(matches!(events[3], Event::LifecycleReady(_)));
}
