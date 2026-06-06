use super::*;

#[test]
fn extension_reader_waits_for_initialized_ack() {
    let (reader_stream, writer_stream) = UnixStream::pair().expect("stream pair");
    let (tx, rx) = mpsc::channel();
    let (initialized_tx, initialized_rx) = mpsc::channel();
    spawn_reader_thread_after_initialized("conn-test".into(), reader_stream, tx, initialized_rx);

    let mut writer = tau_proto::HarnessInputWriter::new(BufWriter::new(writer_stream));
    writer
        .write_message(&tau_proto::HarnessInputMessage::Hello(tau_proto::Hello {
            protocol_version: tau_proto::PROTOCOL_VERSION,
            client_name: "test-extension".into(),
            client_kind: tau_proto::ClientKind::Tool,
        }))
        .expect("write hello");
    writer.flush().expect("flush hello");

    assert!(matches!(
        rx.recv_timeout(Duration::from_millis(50)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));

    initialized_tx.send(()).expect("send initialized ack");
    let event = rx
        .recv_timeout(Duration::from_secs(1))
        .expect("reader forwards after initialized ack");
    match event {
        HarnessEvent::FromConnection {
            connection_id,
            message,
        } => {
            assert_eq!(connection_id.as_str(), "conn-test");
            assert!(matches!(
                message.as_ref(),
                tau_proto::HarnessInputMessage::Hello(_)
            ));
        }
        HarnessEvent::Disconnected { .. }
        | HarnessEvent::NewClient(_)
        | HarnessEvent::Command(_) => panic!("unexpected harness event"),
    }
}
