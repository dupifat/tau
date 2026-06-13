use std::path::PathBuf;
use std::time::Duration;

use tau_proto::{
    CborValue, ClientKind, Disconnect, Event, HarnessInputMessage, HarnessOutputMessage, Hello,
    PROTOCOL_VERSION, Ready, Subscribe, ToolRegister, ToolStarted,
};
use tau_supervisor::{ExtensionCommand, SupervisedChild};

fn test_child_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_tau-supervisor-test-child"))
}

fn expect_child_startup(child: &mut SupervisedChild) -> ToolRegister {
    let hello = child
        .recv_timeout(Duration::from_secs(1))
        .expect("hello should decode")
        .expect("hello should arrive");
    assert_eq!(
        hello,
        HarnessInputMessage::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            client_name: "test-child".into(),
            client_kind: ClientKind::Tool,
        })
    );

    let subscribe = child
        .recv_timeout(Duration::from_secs(1))
        .expect("subscribe should decode")
        .expect("subscribe should arrive");
    assert_eq!(
        subscribe,
        HarnessInputMessage::Subscribe(Subscribe {
            selectors: vec![tau_proto::EventSelector::Exact(
                tau_proto::EventName::TOOL_STARTED,
            )],
        })
    );

    let register = child
        .recv_timeout(Duration::from_secs(1))
        .expect("register should decode")
        .expect("register should arrive");
    let HarnessInputMessage::Emit(emit) = register else {
        panic!("expected tool register emit");
    };
    let Event::ToolRegister(register) = *emit.event else {
        panic!("expected tool register event");
    };

    let ready = child
        .recv_timeout(Duration::from_secs(1))
        .expect("ready should decode")
        .expect("ready should arrive");
    assert_eq!(
        ready,
        HarnessInputMessage::Ready(Ready {
            message: Some("ready".to_owned()),
        })
    );

    register
}

fn disconnect_child(child: &mut SupervisedChild, reason: &str) {
    child
        .send(&HarnessOutputMessage::Disconnect(Disconnect {
            reason: Some(reason.to_owned()),
        }))
        .expect("disconnect should be sent");
}
#[test]
fn supervised_child_exchanges_protocol_events_over_stdio() {
    let command = ExtensionCommand {
        name: "test-child".into(),
        program: test_child_path(),
        args: Vec::new(),
    };
    let mut child = SupervisedChild::spawn(command.clone()).expect("child should spawn");

    assert_eq!(child.command(), &command);
    assert_eq!(
        child.command().starting_event(42.into(), Some(child.pid())),
        Event::ExtensionStarting(tau_proto::ExtensionStarting {
            instance_id: 42.into(),
            extension_name: "test-child".into(),
            pid: Some(child.pid()),
        })
    );

    let register = expect_child_startup(&mut child);
    assert_eq!(
        register,
        ToolRegister {
            tool: tau_proto::ToolSpec {
                name: tau_proto::ToolName::new("echo"),
                model_visible_name: None,
                description: Some("Echo test payloads".to_owned()),
                tool_type: tau_proto::ToolType::Function,
                parameters: None,
                format: None,
                enabled_by_default: true,
                background_support: None,
            },
            tool_group: None,
            prompt_fragment: None,
        }
    );
    assert_eq!(
        child.ready_event(42.into(), Some(child.pid())),
        Event::ExtensionReady(tau_proto::ExtensionReady {
            instance_id: 42.into(),
            extension_name: "test-child".into(),
            pid: Some(child.pid()),
        })
    );

    child
        .send(&HarnessOutputMessage::deliver(Event::ToolStarted(
            ToolStarted {
                call_id: "call-1".into(),
                tool_name: tau_proto::ToolName::new("echo"),
                arguments: CborValue::Text("hello".to_owned()),
                agent_id: tau_proto::AgentId::parse("agent-1").expect("agent id"),
                originator: tau_proto::PromptOriginator::User,
            },
        )))
        .expect("tool should be sent");
    let result = child
        .recv_timeout(Duration::from_secs(1))
        .expect("tool result should decode")
        .expect("tool result should arrive");
    assert_eq!(
        result,
        HarnessInputMessage::emit(Event::ToolResult(tau_proto::ToolResult {
            call_id: "call-1".into(),
            tool_name: tau_proto::ToolName::new("echo"),
            tool_type: tau_proto::ToolType::Function,
            result: CborValue::Text("hello".to_owned()),
            kind: tau_proto::ToolResultKind::Final,
            display: None,
            originator: tau_proto::PromptOriginator::User,
        }))
    );

    disconnect_child(&mut child, "done");
    let exit = child
        .wait_for_exit(Duration::from_secs(2))
        .expect("child should exit");
    assert_eq!(exit.exit_code, Some(0));
    assert_eq!(
        child.exited_event(42.into(), None, &exit),
        Event::ExtensionExited(tau_proto::ExtensionExited {
            instance_id: 42.into(),
            extension_name: "test-child".into(),
            pid: None,
            exit_code: Some(0),
            signal: None,
        })
    );
}

#[test]
fn restarted_child_can_reregister_after_exit() {
    let command = ExtensionCommand {
        name: "test-child".into(),
        program: test_child_path(),
        args: Vec::new(),
    };

    for _ in 0..2 {
        let mut child = SupervisedChild::spawn(command.clone()).expect("child should spawn");
        let register = expect_child_startup(&mut child);
        assert_eq!(register.tool.name, tau_proto::ToolName::new("echo"));

        disconnect_child(&mut child, "restart");
        let exit = child
            .wait_for_exit(Duration::from_secs(2))
            .expect("child should exit");
        assert_eq!(exit.exit_code, Some(0));
    }
}
