use std::error::Error;
use std::io::{BufReader, BufWriter, Write};

use tau_proto::{
    CborValue, ClientKind, Event, HarnessInputMessage, HarnessOutputMessage, Hello,
    PROTOCOL_VERSION, PeerInputReader, PeerOutputWriter, Ready, Subscribe, ToolRegister,
    ToolResult, ToolSpec,
};

const EXIT_IMMEDIATELY_ARG: &str = "--exit-immediately";
const PARTIAL_FRAME_ARG: &str = "--partial-frame";
const FLOOD_ARG: &str = "--flood";

fn main() -> Result<(), Box<dyn Error>> {
    match std::env::args().nth(1).as_deref() {
        Some(EXIT_IMMEDIATELY_ARG) => return Ok(()),
        Some(PARTIAL_FRAME_ARG) => {
            std::io::stdout().write_all(&[0x81])?;
            return Ok(());
        }
        Some(FLOOD_ARG) => {
            let stdout = std::io::stdout();
            let mut writer = PeerOutputWriter::new(BufWriter::new(stdout.lock()));
            for index in 0..128 {
                writer.write_message(&HarnessInputMessage::Ready(Ready {
                    message: Some(index.to_string()),
                }))?;
            }
            writer.flush()?;
            return Ok(());
        }
        _ => {}
    }
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = PeerInputReader::new(BufReader::new(stdin.lock()));
    let mut writer = PeerOutputWriter::new(BufWriter::new(stdout.lock()));

    writer.write_message(&HarnessInputMessage::Hello(Hello {
        protocol_version: PROTOCOL_VERSION,
        client_name: "test-child".into(),
        client_kind: ClientKind::Tool,
    }))?;
    writer.write_message(&HarnessInputMessage::Subscribe(Subscribe {
        selectors: vec![tau_proto::EventSelector::Exact(
            tau_proto::EventName::TOOL_STARTED,
        )],
    }))?;
    writer.write_message(&HarnessInputMessage::emit(Event::ToolRegister(
        ToolRegister {
            tool: ToolSpec {
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
        },
    )))?;
    writer.write_message(&HarnessInputMessage::Ready(Ready {
        message: Some("ready".to_owned()),
    }))?;
    writer.flush()?;

    loop {
        let Some(message) = reader.read_message()? else {
            return Ok(());
        };
        match message {
            HarnessOutputMessage::Deliver(delivery) => {
                // Tool invocations are execution triggers; replay-marked
                // frames re-send history and must not re-run them.
                if delivery.is_replay() {
                    continue;
                }
                let Event::ToolStarted(invoke) = delivery.into_event() else {
                    continue;
                };
                if invoke.tool_name != tau_proto::ToolName::new("echo") {
                    continue;
                }
                writer.write_message(&HarnessInputMessage::emit(Event::ToolResult(
                    ToolResult {
                        call_id: invoke.call_id,
                        tool_name: invoke.tool_name,
                        tool_type: tau_proto::ToolType::Function,
                        result: match invoke.arguments {
                            CborValue::Null => CborValue::Text("null".to_owned()),
                            value => value,
                        },
                        kind: tau_proto::ToolResultKind::Final,
                        display: None,
                        originator: tau_proto::PromptOriginator::User,
                    },
                )))?;
                writer.flush()?;
            }
            HarnessOutputMessage::Disconnect(_) => return Ok(()),
            _ => {}
        }
    }
}
