//! Thread-safe runtime event sequencer used by ackable event delivery.
//!
//! The harness still assigns one globally monotonic [`EventLogSeq`] to every
//! committed runtime event, but the sequencer does not retain event payloads.
//! Replay comes from semantic state instead: durable session/agent stores,
//! current harness snapshots, and the append-only `events.jsonl` debug trace.

#[cfg(test)]
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[cfg(test)]
use tau_proto::{ConnectionId, Event};
use tau_proto::{EventLogSeq, UnixMicros};

/// One committed event captured by the test-only observer.
#[cfg(test)]
#[derive(Clone, Debug)]
pub(crate) struct LogEntry {
    pub seq: EventLogSeq,
    pub recorded_at: UnixMicros,
    pub source: Option<ConnectionId>,
    pub event: Event,
}

struct EventLogInner {
    next_seq: EventLogSeq,
    #[cfg(test)]
    entries: BTreeMap<EventLogSeq, LogEntry>,
}

/// Thread-safe runtime event sequencer.
///
/// Production builds keep only the next sequence counter. Tests also keep a
/// small observer log so existing behavioral assertions can inspect what the
/// harness committed without introducing a production retention path.
pub(crate) struct EventLog {
    inner: Mutex<EventLogInner>,
}

impl EventLog {
    /// Creates an empty sequencer.
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(EventLogInner {
                next_seq: EventLogSeq::new(0),
                #[cfg(test)]
                entries: BTreeMap::new(),
            }),
        })
    }

    /// Reserves the next harness runtime event-log sequence.
    ///
    /// Durable-history replay uses this path: replayed transcript facts already
    /// live in agent logs, but their runtime deliveries still need fresh
    /// globally monotonic [`EventLogSeq`] values rather than reusing persisted
    /// per-agent/per-session sequences.
    pub(crate) fn reserve_seq(&self) -> EventLogSeq {
        let mut inner = self.inner.lock().expect("event log mutex poisoned");
        let seq = inner.next_seq;
        inner.next_seq = inner.next_seq.next();
        seq
    }

    /// Assigns a sequence and wall-clock timestamp for one live committed
    /// event.
    ///
    /// Stamping happens at the publish chokepoint so the wire delivery, any
    /// durable semantic record, and the debug JSONL line all carry the same
    /// timestamp. The timestamp is returned to the caller and is not retained
    /// in production memory.
    pub(crate) fn append(&self) -> (EventLogSeq, UnixMicros) {
        let recorded_at = UnixMicros::now();
        let seq = self.reserve_seq();
        (seq, recorded_at)
    }

    /// Records a committed event for test assertions only.
    #[cfg(test)]
    pub(crate) fn record_for_test(
        &self,
        seq: EventLogSeq,
        recorded_at: UnixMicros,
        source: Option<ConnectionId>,
        event: Event,
    ) {
        let mut inner = self.inner.lock().expect("event log mutex poisoned");
        inner.entries.insert(
            seq,
            LogEntry {
                seq,
                recorded_at,
                source,
                event,
            },
        );
    }

    /// Returns the first test-observed entry with seq >= `from`, or `None` if
    /// no such entry exists yet.
    #[cfg(test)]
    pub(crate) fn get_next_from(&self, from: EventLogSeq) -> Option<LogEntry> {
        let inner = self.inner.lock().expect("event log mutex poisoned");
        inner
            .entries
            .range(from..)
            .next()
            .map(|(_, entry)| entry.clone())
    }

    /// Returns the next runtime event-log sequence. Used by tests to assert
    /// that no event-log sequence was consumed across a section of code.
    #[cfg(test)]
    pub(crate) fn next_seq(&self) -> EventLogSeq {
        self.inner
            .lock()
            .expect("event log mutex poisoned")
            .next_seq
    }
}

#[cfg(test)]
mod tests;
