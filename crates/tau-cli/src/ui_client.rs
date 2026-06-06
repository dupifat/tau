//! Shared UI socket client helpers.

use std::io::{self, BufReader, BufWriter};
use std::os::unix::net::UnixStream;
use std::path::Path;

use tau_proto::{
    ClientKind, EventSelector, HarnessInputMessage, Hello, PROTOCOL_VERSION, PeerInputReader,
    PeerOutputWriter, Subscribe,
};

pub(crate) type UiInputReader = PeerInputReader<BufReader<UnixStream>>;
pub(crate) type UiOutputWriter = PeerOutputWriter<BufWriter<UnixStream>>;

pub(crate) fn connect_ui_client(
    socket_path: &Path,
    client_name: impl Into<tau_proto::ExtensionName>,
) -> io::Result<(UiInputReader, UiOutputWriter)> {
    let stream = UnixStream::connect(socket_path)?;
    let read_stream = stream.try_clone()?;
    let mut writer = PeerOutputWriter::new(BufWriter::new(stream));
    send_hello(&mut writer, client_name)?;
    let reader = PeerInputReader::new(BufReader::new(read_stream));
    Ok((reader, writer))
}

pub(crate) fn connect_ui_writer(
    socket_path: &Path,
    client_name: impl Into<tau_proto::ExtensionName>,
) -> io::Result<UiOutputWriter> {
    let stream = UnixStream::connect(socket_path)?;
    let mut writer = PeerOutputWriter::new(BufWriter::new(stream));
    send_hello(&mut writer, client_name)?;
    Ok(writer)
}

pub(crate) fn hello_message(
    client_name: impl Into<tau_proto::ExtensionName>,
) -> HarnessInputMessage {
    HarnessInputMessage::Hello(Hello {
        protocol_version: PROTOCOL_VERSION,
        client_name: client_name.into(),
        client_kind: ClientKind::Ui,
    })
}

pub(crate) fn chat_subscription_selectors() -> Vec<EventSelector> {
    vec![
        EventSelector::Prefix("ui.".to_owned()),
        EventSelector::Prefix("action.".to_owned()),
        EventSelector::Prefix("agent.".to_owned()),
        EventSelector::Prefix("session.".to_owned()),
        EventSelector::Prefix("provider.".to_owned()),
        EventSelector::Prefix("tool.".to_owned()),
        EventSelector::Prefix("extension.".to_owned()),
        EventSelector::Prefix("harness.".to_owned()),
        EventSelector::Prefix("shell.".to_owned()),
        EventSelector::Prefix("term.".to_owned()),
    ]
}

pub(crate) fn subscribe_message(selectors: Vec<EventSelector>) -> HarnessInputMessage {
    HarnessInputMessage::Subscribe(Subscribe { selectors })
}

pub(crate) fn send_hello(
    writer: &mut UiOutputWriter,
    client_name: impl Into<tau_proto::ExtensionName>,
) -> io::Result<()> {
    send_message(writer, &hello_message(client_name))
}

pub(crate) fn subscribe(
    writer: &mut UiOutputWriter,
    selectors: Vec<EventSelector>,
) -> io::Result<()> {
    send_message(writer, &subscribe_message(selectors))
}

pub(crate) fn send_message(
    writer: &mut UiOutputWriter,
    message: &HarnessInputMessage,
) -> io::Result<()> {
    writer.write_message(message).map_err(io::Error::other)?;
    writer.flush()
}
