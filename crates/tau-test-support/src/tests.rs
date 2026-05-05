use std::time::Duration;

use super::*;

#[test]
fn runtime_supports_embedded_and_daemon_scenarios() {
    let runtime = TestRuntime::new().expect("runtime should be created");

    let embedded = runtime
        .run_embedded("session-1", "hello")
        .expect("embedded run should succeed");
    assert!(!embedded.is_empty(), "response should not be empty");

    let daemon = runtime.spawn_daemon("session-2", Some(1));
    runtime
        .wait_until_ready(Duration::from_secs(2))
        .expect("daemon socket should appear");
    let attached = runtime
        .send_daemon_message("session-2", "hello")
        .expect("daemon message should succeed");
    assert!(!attached.is_empty(), "response should not be empty");
    daemon.join().expect("daemon should exit cleanly");
}
