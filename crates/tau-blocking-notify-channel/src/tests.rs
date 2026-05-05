use std::thread;
use std::time::Duration;

use super::*;

#[test]
fn single_notify_wakes_receiver() {
    let (tx, rx) = channel();
    tx.notify();
    assert_eq!(rx.recv(), Ok(()));
}

#[test]
fn multiple_notifies_coalesce() {
    let (tx, rx) = channel();
    tx.notify();
    tx.notify();
    tx.notify();
    assert_eq!(rx.recv(), Ok(()));
    assert_eq!(rx.try_recv(), Ok(false));
}

#[test]
fn try_recv_returns_false_when_not_notified() {
    let (_tx, rx) = channel();
    assert_eq!(rx.try_recv(), Ok(false));
}

#[test]
fn try_recv_returns_true_and_resets() {
    let (tx, rx) = channel();
    tx.notify();
    assert_eq!(rx.try_recv(), Ok(true));
    assert_eq!(rx.try_recv(), Ok(false));
}

#[test]
fn recv_blocks_until_notified() {
    let (tx, rx) = channel();
    let handle = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        tx.notify();
    });
    assert_eq!(rx.recv(), Ok(()));
    handle.join().expect("sender thread panicked");
}

#[test]
fn multiple_senders() {
    let (tx, rx) = channel();
    let tx2 = tx.clone();

    let h1 = thread::spawn(move || {
        tx.notify();
    });
    let h2 = thread::spawn(move || {
        tx2.notify();
    });

    h1.join().expect("sender 1 panicked");
    h2.join().expect("sender 2 panicked");

    assert_eq!(rx.recv(), Ok(()));
    // Both senders are gone — channel is disconnected.
    assert_eq!(rx.try_recv(), Err(Disconnected));
}

#[test]
fn repeated_send_recv_cycles() {
    let (tx, rx) = channel();
    for _ in 0..100 {
        tx.notify();
        assert_eq!(rx.recv(), Ok(()));
        assert_eq!(rx.try_recv(), Ok(false));
    }
}

#[test]
fn disconnect_after_all_senders_dropped() {
    let (tx, rx) = channel();
    drop(tx);
    assert_eq!(rx.recv(), Err(Disconnected));
}

#[test]
fn disconnect_after_last_clone_dropped() {
    let (tx, rx) = channel();
    let tx2 = tx.clone();
    drop(tx);
    // Still one sender alive.
    assert_eq!(rx.try_recv(), Ok(false));
    drop(tx2);
    assert_eq!(rx.recv(), Err(Disconnected));
}

#[test]
fn try_recv_reports_disconnect() {
    let (tx, rx) = channel();
    drop(tx);
    assert_eq!(rx.try_recv(), Err(Disconnected));
}

#[test]
fn notification_takes_priority_over_disconnect() {
    let (tx, rx) = channel();
    tx.notify();
    drop(tx);
    // Notification delivered first despite disconnect.
    assert_eq!(rx.recv(), Ok(()));
    // Now disconnected.
    assert_eq!(rx.recv(), Err(Disconnected));
}

#[test]
fn recv_unblocks_on_disconnect() {
    let (tx, rx) = channel();
    let handle = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        drop(tx);
    });
    assert_eq!(rx.recv(), Err(Disconnected));
    handle.join().expect("sender thread panicked");
}
