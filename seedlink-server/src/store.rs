use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use seedlink_rs_protocol::SequenceNumber;
use seedlink_rs_protocol::frame::v3;
use tokio::sync::Notify;

/// A single record in the ring buffer.
#[derive(Clone, Debug)]
pub struct Record {
    pub sequence: SequenceNumber,
    pub network: String,
    pub station: String,
    pub payload: Vec<u8>,
}

/// Station subscription filter (network + station).
#[derive(Clone, Debug)]
pub(crate) struct Subscription {
    pub network: String,
    pub station: String,
}

struct Ring {
    buf: VecDeque<Record>,
    capacity: usize,
    next_seq: u64,
}

impl Ring {
    fn new(capacity: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(capacity),
            capacity,
            next_seq: 1,
        }
    }

    fn push(&mut self, network: String, station: String, payload: Vec<u8>) -> SequenceNumber {
        let seq = SequenceNumber::new(self.next_seq);

        self.buf.push_back(Record {
            sequence: seq,
            network,
            station,
            payload,
        });

        // Evict oldest if over capacity
        if self.buf.len() > self.capacity {
            self.buf.pop_front();
        }

        // Advance and wrap at V3_MAX back to 1
        self.next_seq += 1;
        if self.next_seq > SequenceNumber::V3_MAX {
            self.next_seq = 1;
        }

        seq
    }

    fn read_since(&self, cursor: u64, subscriptions: &[Subscription]) -> Vec<Record> {
        self.buf
            .iter()
            .filter(|r| r.sequence.value() > cursor)
            .filter(|r| {
                subscriptions.iter().any(|s| {
                    s.network.eq_ignore_ascii_case(&r.network)
                        && s.station.eq_ignore_ascii_case(&r.station)
                })
            })
            .cloned()
            .collect()
    }
}

struct StoreInner {
    ring: Mutex<Ring>,
    notify: Notify,
}

/// Thread-safe data store backed by an in-memory ring buffer.
///
/// Clone is cheap (Arc).
#[derive(Clone)]
pub struct DataStore(Arc<StoreInner>);

impl DataStore {
    /// Create a new store with the given ring buffer capacity.
    pub fn new(capacity: usize) -> Self {
        Self(Arc::new(StoreInner {
            ring: Mutex::new(Ring::new(capacity)),
            notify: Notify::new(),
        }))
    }

    /// Push a miniSEED record into the ring buffer.
    ///
    /// Payload must be exactly 512 bytes (miniSEED v2 record size).
    /// Returns the assigned sequence number.
    ///
    /// # Panics
    ///
    /// Panics if `payload.len() != 512`.
    pub fn push(&self, network: &str, station: &str, payload: &[u8]) -> SequenceNumber {
        assert_eq!(
            payload.len(),
            v3::PAYLOAD_LEN,
            "payload must be exactly {} bytes, got {}",
            v3::PAYLOAD_LEN,
            payload.len()
        );

        let seq = self.0.ring.lock().unwrap().push(
            network.to_owned(),
            station.to_owned(),
            payload.to_vec(),
        );

        self.0.notify.notify_waiters();
        seq
    }

    /// Read all records with sequence > cursor that match the given subscriptions.
    pub(crate) fn read_since(&self, cursor: u64, subscriptions: &[Subscription]) -> Vec<Record> {
        self.0
            .ring
            .lock()
            .unwrap()
            .read_since(cursor, subscriptions)
    }

    /// Returns a future that completes when new data is pushed.
    ///
    /// **Important:** call this *before* `read_since()` to avoid missing
    /// pushes that happen between read and wait.
    pub(crate) fn notified(&self) -> tokio::sync::futures::Notified<'_> {
        self.0.notify.notified()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_payload() -> Vec<u8> {
        vec![0u8; v3::PAYLOAD_LEN]
    }

    #[test]
    fn push_assigns_increasing_sequences() {
        let store = DataStore::new(100);
        let s1 = store.push("IU", "ANMO", &dummy_payload());
        let s2 = store.push("IU", "ANMO", &dummy_payload());
        let s3 = store.push("GE", "WLF", &dummy_payload());
        assert_eq!(s1.value(), 1);
        assert_eq!(s2.value(), 2);
        assert_eq!(s3.value(), 3);
    }

    #[test]
    fn read_since_filters_by_subscription() {
        let store = DataStore::new(100);
        store.push("IU", "ANMO", &dummy_payload());
        store.push("GE", "WLF", &dummy_payload());
        store.push("IU", "ANMO", &dummy_payload());

        let subs = vec![Subscription {
            network: "IU".into(),
            station: "ANMO".into(),
        }];

        let records = store.read_since(0, &subs);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].sequence.value(), 1);
        assert_eq!(records[1].sequence.value(), 3);
    }

    #[test]
    fn read_since_respects_cursor() {
        let store = DataStore::new(100);
        store.push("IU", "ANMO", &dummy_payload());
        store.push("IU", "ANMO", &dummy_payload());
        store.push("IU", "ANMO", &dummy_payload());

        let subs = vec![Subscription {
            network: "IU".into(),
            station: "ANMO".into(),
        }];

        let records = store.read_since(2, &subs);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].sequence.value(), 3);
    }

    #[test]
    fn eviction_on_capacity() {
        let store = DataStore::new(3);
        for _ in 0..5 {
            store.push("IU", "ANMO", &dummy_payload());
        }

        let subs = vec![Subscription {
            network: "IU".into(),
            station: "ANMO".into(),
        }];

        let records = store.read_since(0, &subs);
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].sequence.value(), 3);
        assert_eq!(records[1].sequence.value(), 4);
        assert_eq!(records[2].sequence.value(), 5);
    }

    #[test]
    fn sequence_wraps_at_v3_max() {
        let store = DataStore::new(10);
        // Manually set next_seq near V3_MAX
        {
            let mut ring = store.0.ring.lock().unwrap();
            ring.next_seq = SequenceNumber::V3_MAX;
        }

        let s1 = store.push("IU", "ANMO", &dummy_payload());
        let s2 = store.push("IU", "ANMO", &dummy_payload());

        assert_eq!(s1.value(), SequenceNumber::V3_MAX);
        assert_eq!(s2.value(), 1); // wrapped
    }

    #[test]
    #[should_panic(expected = "payload must be exactly 512 bytes")]
    fn push_rejects_wrong_payload_size() {
        let store = DataStore::new(10);
        store.push("IU", "ANMO", &[0u8; 100]);
    }
}
