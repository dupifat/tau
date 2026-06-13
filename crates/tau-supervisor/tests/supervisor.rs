use std::fs;
use std::path::PathBuf;
use std::process::Command as StdCommand;
use std::time::Duration;

use tau_proto::{
    CborValue, ClientKind, Disconnect, Event, HarnessInputMessage, HarnessOutputMessage, Hello,
    PROTOCOL_VERSION, Ready, Subscribe, ToolRegister, ToolStarted,
};
use tau_supervisor::{
    ExtensionCommand, ReceiveOutcome, StderrPolicy, SupervisedChild, SupervisionError,
};

fn test_command(args: Vec<String>) -> ExtensionCommand {
    ExtensionCommand {
        name: "test-child".into(),
        program: PathBuf::from(env!("CARGO_BIN_EXE_tau-supervisor-test-child")),
        args,
        working_dir: None,
        stderr: StderrPolicy::Inherit,
    }
}

fn test_child_path() -> PathBuf {
    test_command(Vec::new()).program
}

fn expect_message(child: &mut SupervisedChild, label: &str) -> HarnessInputMessage {
    match child
        .recv_timeout(Duration::from_secs(1))
        .unwrap_or_else(|error| panic!("{label} should decode: {error}"))
    {
        ReceiveOutcome::Message(message) => message,
        ReceiveOutcome::Timeout => panic!("{label} should arrive before timeout"),
        ReceiveOutcome::Closed => panic!("{label} should arrive before stdout closes"),
    }
}

fn expect_child_startup(child: &mut SupervisedChild) -> ToolRegister {
    let hello = expect_message(child, "hello");
    assert_eq!(
        hello,
        HarnessInputMessage::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            client_name: "test-child".into(),
            client_kind: ClientKind::Tool,
        })
    );

    let subscribe = expect_message(child, "subscribe");
    assert_eq!(
        subscribe,
        HarnessInputMessage::Subscribe(Subscribe {
            selectors: vec![tau_proto::EventSelector::Exact(
                tau_proto::EventName::TOOL_STARTED,
            )],
        })
    );

    let register = expect_message(child, "register");
    let HarnessInputMessage::Emit(emit) = register else {
        panic!("expected tool register emit");
    };
    let Event::ToolRegister(register) = *emit.event else {
        panic!("expected tool register event");
    };

    let ready = expect_message(child, "ready");
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

/// Ensures receive timeout is observable without treating the child as
/// disconnected.
#[test]
fn recv_timeout_reports_timeout_without_conflating_disconnect() {
    let mut child = SupervisedChild::spawn(test_command(Vec::new())).expect("child should spawn");
    let _register = expect_child_startup(&mut child);

    assert_eq!(
        child
            .recv_timeout(Duration::from_millis(20))
            .expect("timeout should not be an error"),
        ReceiveOutcome::Timeout
    );

    disconnect_child(&mut child, "done");
    let _exit = child
        .wait_for_exit(Duration::from_secs(2))
        .expect("child should exit");
}

/// Ensures clean stdout EOF is reported separately from timeout and decode
/// failure.
#[test]
fn recv_timeout_reports_clean_stdout_close() {
    let mut child = SupervisedChild::spawn(test_command(vec!["--exit-immediately".to_owned()]))
        .expect("child should spawn");

    assert_eq!(
        child
            .recv_timeout(Duration::from_secs(1))
            .expect("clean close should not be an error"),
        ReceiveOutcome::Closed
    );
    let exit = child
        .wait_for_exit(Duration::from_secs(2))
        .expect("child should exit");
    assert_eq!(exit.exit_code, Some(0));
}

/// Ensures truncated protocol data remains a decode error instead of a clean
/// close.
#[test]
fn recv_timeout_reports_partial_frame_as_decode_error() {
    let mut child = SupervisedChild::spawn(test_command(vec!["--partial-frame".to_owned()]))
        .expect("child should spawn");

    let error = child
        .recv_timeout(Duration::from_secs(1))
        .expect_err("partial frame should be a decode error");
    assert!(matches!(error, SupervisionError::Decode(_)));
    let _exit = child
        .wait_for_exit(Duration::from_secs(2))
        .expect("child should exit");
}

/// Ensures the bounded stdout reader can drain a child message burst without
/// loss.
#[test]
fn stdout_reader_handles_flood_without_unbounded_queueing() {
    let mut child = SupervisedChild::spawn(test_command(vec!["--flood".to_owned()]))
        .expect("child should spawn");

    for index in 0..128 {
        assert_eq!(
            child
                .recv_timeout(Duration::from_secs(1))
                .expect("flood message should decode"),
            ReceiveOutcome::Message(HarnessInputMessage::Ready(Ready {
                message: Some(index.to_string()),
            }))
        );
    }
    assert_eq!(
        child
            .recv_timeout(Duration::from_secs(1))
            .expect("clean close should decode"),
        ReceiveOutcome::Closed
    );
    let exit = child
        .wait_for_exit(Duration::from_secs(2))
        .expect("child should exit");
    assert_eq!(exit.exit_code, Some(0));
}

/// Ensures the spawn policy applies the configured child working directory.
#[test]
fn spawn_uses_configured_working_dir() {
    let working_dir =
        std::env::temp_dir().join(format!("tau-supervisor-cwd-{}", std::process::id()));
    fs::create_dir_all(&working_dir).expect("working dir should be created");

    let mut command = test_command(vec!["--report-cwd".to_owned()]);
    command.working_dir = Some(working_dir.clone());
    let mut child = SupervisedChild::spawn(command).expect("child should spawn");

    assert_eq!(
        child
            .recv_timeout(Duration::from_secs(1))
            .expect("cwd report should decode"),
        ReceiveOutcome::Message(HarnessInputMessage::Ready(Ready {
            message: Some(working_dir.display().to_string()),
        }))
    );
    let exit = child
        .wait_for_exit(Duration::from_secs(2))
        .expect("child should exit");
    assert_eq!(exit.exit_code, Some(0));
    fs::remove_dir_all(working_dir).expect("working dir should be removed");
}

/// Ensures explicit hard termination can clean up a child that ignores protocol
/// shutdown.
#[test]
fn terminate_kills_long_running_child() {
    let mut child = SupervisedChild::spawn(test_command(vec!["--sleep".to_owned()]))
        .expect("child should spawn");

    let exit = child
        .terminate(Duration::from_secs(2))
        .expect("child should terminate");
    assert!(exit.exit_code.is_none() || exit.exit_code != Some(0));
}

/// Ensures supervised children do not inherit parent `TAU_SECRET_*` values.
#[test]
fn spawned_child_does_not_inherit_tau_secret_env() {
    if std::env::var_os("TAU_SUPERVISOR_SECRET_ENV_SUBPROCESS").is_some() {
        let mut child =
            SupervisedChild::spawn(test_command(vec!["--report-secret-env".to_owned()]))
                .expect("child should spawn");
        assert_eq!(
            child
                .recv_timeout(Duration::from_secs(1))
                .expect("env report should decode"),
            ReceiveOutcome::Message(HarnessInputMessage::Ready(Ready {
                message: Some("absent".to_owned()),
            }))
        );
        let _exit = child
            .wait_for_exit(Duration::from_secs(2))
            .expect("child should exit");
        return;
    }

    let status = StdCommand::new(std::env::current_exe().expect("test binary path"))
        .arg("--exact")
        .arg("spawned_child_does_not_inherit_tau_secret_env")
        .arg("--nocapture")
        .env("TAU_SUPERVISOR_SECRET_ENV_SUBPROCESS", "1")
        .env("TAU_SECRET_REGRESSION", "must-not-leak")
        .status()
        .expect("env regression subprocess should run");
    assert!(status.success());
}
/// Ensures the supervisor exchanges lifecycle and tool events over child stdio.
#[test]
fn supervised_child_exchanges_protocol_events_over_stdio() {
    let command = ExtensionCommand {
        name: "test-child".into(),
        program: test_child_path(),
        args: Vec::new(),
        working_dir: None,
        stderr: StderrPolicy::Inherit,
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
    let result = expect_message(&mut child, "tool result");
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

/// Ensures a restarted child can emit the same tool registration after prior
/// exit.
#[test]
fn restarted_child_can_reregister_after_exit() {
    let command = ExtensionCommand {
        name: "test-child".into(),
        program: test_child_path(),
        args: Vec::new(),
        working_dir: None,
        stderr: StderrPolicy::Inherit,
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
