use std::io::{BufReader, BufWriter};
use std::thread;
use std::time::Duration;

use tau_proto::{
    ClientKind, Disconnect, HarnessInputMessage, HarnessInputReader, HarnessOutputMessage,
    HarnessOutputWriter, Hello, PROTOCOL_VERSION,
};
use tempfile::TempDir;

use super::*;

#[test]
fn later_attached_client_can_exchange_protocol_events_over_unix_socket() {
    let tempdir = TempDir::new().expect("tempdir should exist");
    let socket_path = tempdir.path().join("tau.sock");
    let listener = SocketListener::bind(&socket_path).expect("listener should bind");

    let client_thread = thread::spawn({
        let socket_path = socket_path.clone();
        move || {
            let mut client = SocketPeer::connect(socket_path).expect("client should connect");
            client
                .send(&HarnessInputMessage::Hello(Hello {
                    protocol_version: PROTOCOL_VERSION,
                    client_name: "client".into(),
                    client_kind: ClientKind::Ui,
                }))
                .expect("client hello should send");
            client
                .recv_timeout(Duration::from_secs(1))
                .expect("client should read response")
                .expect("response should arrive")
        }
    });

    let (stream, _) = listener
        .listener
        .accept()
        .expect("server should accept client");
    let read_stream = stream.try_clone().expect("stream should clone");
    let mut server_reader = HarnessInputReader::new(BufReader::new(read_stream));
    let mut server_writer = HarnessOutputWriter::new(BufWriter::new(stream));
    let hello = server_reader
        .read_message()
        .expect("server should read hello")
        .expect("hello should arrive");
    assert_eq!(
        hello,
        HarnessInputMessage::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            client_name: "client".into(),
            client_kind: ClientKind::Ui,
        })
    );
    server_writer
        .write_message(&HarnessOutputMessage::Disconnect(Disconnect {
            reason: Some("server".to_owned()),
        }))
        .expect("server disconnect should send");
    server_writer.flush().expect("server response should flush");

    let response = client_thread.join().expect("client thread should finish");
    assert_eq!(
        response,
        HarnessOutputMessage::Disconnect(Disconnect {
            reason: Some("server".to_owned()),
        })
    );
}
