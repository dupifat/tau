use std::io::Write as _;
use std::os::unix::net::UnixListener;
use std::time::Duration;
use std::{fs, thread};

use tau_proto::{
    ClientKind, Disconnect, HarnessInputMessage, HarnessOutputMessage, Hello, PROTOCOL_VERSION,
};
use tempfile::TempDir;

use super::*;

/// Ensures the public listener accept API uses server-side protocol direction:
/// accepted clients read peer input messages and write harness output messages.
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
        }
    });

    let mut accepted = listener.accept().expect("server should accept client");
    let hello = accepted
        .recv()
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
    accepted
        .send(&HarnessOutputMessage::Disconnect(Disconnect {
            reason: Some("server".to_owned()),
        }))
        .expect("server disconnect should send");

    let response = client_thread.join().expect("client thread should finish");
    assert_eq!(
        response,
        SocketReceive::Message {
            message: HarnessOutputMessage::Disconnect(Disconnect {
                reason: Some("server".to_owned()),
            }),
        }
    );
}

/// Ensures truncated protocol output is reported as decode failure instead of
/// being collapsed with timeout or clean close outcomes.
#[test]
fn partial_frame_close_is_decode_error() {
    let tempdir = TempDir::new().expect("tempdir should exist");
    let socket_path = tempdir.path().join("tau.sock");
    let listener = SocketListener::bind(&socket_path).expect("listener should bind");

    let client_thread = thread::spawn({
        let socket_path = socket_path.clone();
        move || {
            let mut client = SocketPeer::connect(socket_path).expect("client should connect");
            client.recv_timeout(Duration::from_secs(1))
        }
    });

    let (mut stream, _) = listener
        .listener
        .accept()
        .expect("server should accept client");
    stream
        .write_all(&[0x9f])
        .expect("partial cbor should write");
    drop(stream);

    let result = client_thread.join().expect("client thread should finish");
    assert!(matches!(result, Err(SocketTransportError::Decode { .. })));
}

/// Ensures binding refuses to unlink a pre-existing regular file at the socket
/// path and leaves its contents intact.
#[test]
fn bind_refuses_existing_non_socket_path() {
    let tempdir = TempDir::new().expect("tempdir should exist");
    let socket_path = tempdir.path().join("tau.sock");
    fs::write(&socket_path, b"keep me").expect("regular file should be written");

    let error = match SocketListener::bind(&socket_path) {
        Ok(_) => panic!("bind should refuse file"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        SocketTransportError::RefuseNonSocketPath { .. }
    ));
    assert_eq!(
        fs::read(&socket_path).expect("regular file should remain"),
        b"keep me"
    );
}

/// Ensures binding refuses to replace a socket path that is already accepting
/// connections.
#[test]
fn bind_refuses_active_socket_path() {
    let tempdir = TempDir::new().expect("tempdir should exist");
    let socket_path = tempdir.path().join("tau.sock");
    let active = UnixListener::bind(&socket_path).expect("active listener should bind");

    let error = match SocketListener::bind(&socket_path) {
        Ok(_) => panic!("bind should refuse active socket"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        SocketTransportError::ActiveSocketExists { .. }
    ));
    assert!(socket_path.exists(), "active socket should remain");

    drop(active);
    fs::remove_file(&socket_path).expect("active socket should clean up");
}

/// Ensures binding removes an inactive stale socket left behind by a previous
/// listener and replaces it with a usable listener.
#[test]
fn bind_replaces_inactive_stale_socket_path() {
    let tempdir = TempDir::new().expect("tempdir should exist");
    let socket_path = tempdir.path().join("tau.sock");
    let stale = UnixListener::bind(&socket_path).expect("stale listener should bind");
    drop(stale);

    let listener = SocketListener::bind(&socket_path).expect("stale socket should be replaced");
    assert_eq!(listener.path(), socket_path.as_path());
}

/// Ensures dropping a listener does not unlink a different socket that replaced
/// its original path after binding.
#[test]
fn drop_does_not_remove_replacement_socket() {
    let tempdir = TempDir::new().expect("tempdir should exist");
    let socket_path = tempdir.path().join("tau.sock");
    let listener = SocketListener::bind(&socket_path).expect("listener should bind");

    fs::remove_file(&socket_path).expect("original socket path should be removable");
    let replacement = UnixListener::bind(&socket_path).expect("replacement should bind");

    drop(listener);
    assert!(socket_path.exists(), "replacement socket should remain");

    drop(replacement);
    fs::remove_file(&socket_path).expect("replacement socket should clean up");
}

/// Ensures dropping a connected peer shuts down the background reader thread
/// even when the remote side remains open and idle.
#[test]
fn dropping_peer_stops_background_reader() {
    let tempdir = TempDir::new().expect("tempdir should exist");
    let socket_path = tempdir.path().join("tau.sock");
    let listener = SocketListener::bind(&socket_path).expect("listener should bind");

    let client_thread = thread::spawn({
        let socket_path = socket_path.clone();
        move || {
            let client = SocketPeer::connect(socket_path).expect("client should connect");
            drop(client);
        }
    });

    let (_stream, _) = listener
        .listener
        .accept()
        .expect("server should accept client");
    client_thread.join().expect("client drop should not hang");
}
