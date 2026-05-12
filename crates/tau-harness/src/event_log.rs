//! Thread-safe append-only in-memory event log used by client follower
//! threads for replay + live delivery.
//!
//! The log grows unbounded over a daemon's lifetime: entries are
//! never reclaimed. Followers poll via [`EventLog::get_next_from`]
//! and never block, so no condvar is needed.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tau_proto::{ConnectionId, Event, UnixMicros};

/// Monotonically increasing sequence number for log entries.
pub(crate) type EventSeq = u64;

/// One entry in the event log.
///
/// `recorded_at` is stamped by [`EventLog::append`] at the moment
/// the entry is created. It matches the value carried on the wire
/// `LogEvent` envelope and the value persisted to the durable
/// per-session log — sampling the clock here once and threading the
/// same value through every downstream observer keeps offline timing
/// analyses consistent with what live subscribers saw.
#[derive(Clone, Debug)]
pub(crate) struct LogEntry {
    pub seq: EventSeq,
    // Read by tests; live readers consult the wire envelope or the
    // durable record instead. Kept on the in-memory entry so future
    // replay paths that want to surface original timestamps don't
    // have to re-derive them.
    #[allow(dead_code)]
    pub recorded_at: UnixMicros,
    pub source: Option<ConnectionId>,
    pub event: Event,
}

struct EventLogInner {
    entries: BTreeMap<EventSeq, LogEntry>,
    next_seq: EventSeq,
}

/// Thread-safe append-only event log.
///
/// Consumers track their own position and call
/// [`EventLog::get_next_from`] in a loop. The log does not track
/// subscribers, nor does it prune itself.
pub(crate) struct EventLog {
    inner: Mutex<EventLogInner>,
}

impl EventLog {
    /// Creates an empty event log.
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(EventLogInner {
                entries: BTreeMap::new(),
                next_seq: 0,
            }),
        })
    }

    /// Appends an event and returns its sequence number alongside the
    /// wall-clock timestamp stamped on the entry.
    ///
    /// Stamping happens here (the single chokepoint every event passes
    /// through on its way to the bus) so the value the wire `LogEvent`
    /// envelope carries, the value followers see on replay, and the
    /// value persisted to disk are all the same micros — offline
    /// timing analyses agree with what live consumers saw.
    pub(crate) fn append(
        &self,
        source: Option<ConnectionId>,
        event: Event,
    ) -> (EventSeq, UnixMicros) {
        let recorded_at = UnixMicros::now();
        let mut inner = self.inner.lock().expect("event log mutex poisoned");
        let seq = inner.next_seq;
        inner.next_seq += 1;
        inner.entries.insert(
            seq,
            LogEntry {
                seq,
                recorded_at,
                source,
                event,
            },
        );
        (seq, recorded_at)
    }

    /// Returns the first entry with seq >= `from`, or `None` if no such
    /// entry exists yet.
    pub(crate) fn get_next_from(&self, from: EventSeq) -> Option<LogEntry> {
        let inner = self.inner.lock().expect("event log mutex poisoned");
        inner
            .entries
            .range(from..)
            .next()
            .map(|(_, entry)| entry.clone())
    }

    /// Returns the sequence number that the next appended entry will
    /// receive. Used by tests to assert that no event was logged
    /// across a section of code.
    #[cfg(test)]
    pub(crate) fn next_seq(&self) -> EventSeq {
        self.inner
            .lock()
            .expect("event log mutex poisoned")
            .next_seq
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(message: &str) -> Event {
        Event::HarnessInfo(tau_proto::HarnessInfo {
            message: message.to_owned(),
            level: tau_proto::HarnessInfoLevel::Normal,
        })
    }

    #[test]
    fn append_and_get() {
        let log = EventLog::new();
        let (seq, recorded_at) = log.append(Some("conn-1".into()), info("hello"));
        assert_eq!(seq, 0);
        assert!(recorded_at.get() > 0, "append should stamp wall-clock time");

        let entry = log.get_next_from(0).expect("entry should exist");
        assert_eq!(entry.seq, 0);
        assert_eq!(entry.recorded_at, recorded_at);
        assert_eq!(entry.source, Some("conn-1".into()));

        assert!(log.get_next_from(1).is_none());
    }

    #[test]
    fn get_next_from_skips_earlier() {
        let log = EventLog::new();
        log.append(None, info("a"));
        log.append(None, info("b"));
        log.append(None, info("c"));

        let entry = log.get_next_from(1).expect("entry should exist");
        assert_eq!(entry.seq, 1);
        let Event::HarnessInfo(info) = &entry.event else {
            panic!("expected HarnessInfo");
        };
        assert_eq!(info.message, "b");
    }
}
