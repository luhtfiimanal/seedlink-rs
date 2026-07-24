use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use seedlink_rs_protocol::SequenceNumber;
use tokio::sync::Notify;
use tracing::{error, info};

use crate::error::{Result, ServerError};
use crate::journal::{Journal, SyncPolicy};
use crate::select::SelectPattern;
use crate::time::{TimeWindow, Timestamp};

/// Maximum accepted payload size. miniSEED v2 records are typically 512 bytes;
/// miniSEED v3 records are variable-length. 16 MiB is a generous sanity bound.
pub const MAX_PAYLOAD_LEN: usize = 1 << 24;

const SEQ_MODULUS: u64 = SequenceNumber::V3_MAX + 1;
const SEQ_HALF_WINDOW: u64 = SEQ_MODULUS / 2;

/// Modular "is `seq` strictly after `cursor`" over the 24-bit v3 sequence
/// space. Sequence numbers wrap from `V3_MAX` back to 1, so a plain `>`
/// comparison stalls every stream after a wrap (~19 days at 10 records/s
/// with a persistent journal).
fn seq_is_after(seq: u64, cursor: u64) -> bool {
    let dist = seq.wrapping_sub(cursor) & SequenceNumber::V3_MAX;
    dist != 0 && dist < SEQ_HALF_WINDOW
}

/// The sequence following `seq` (wraps V3_MAX → 1; 0 is never assigned).
fn seq_next(seq: u64) -> u64 {
    if seq >= SequenceNumber::V3_MAX {
        1
    } else {
        seq + 1
    }
}

/// Advance `seq` by `n` steps with wrap.
fn seq_advance(mut seq: u64, n: u64) -> u64 {
    for _ in 0..n {
        seq = seq_next(seq);
    }
    seq
}

/// A single record in the ring buffer.
#[derive(Clone, Debug)]
pub struct Record {
    pub sequence: SequenceNumber,
    pub network: String,
    pub station: String,
    pub payload: Vec<u8>,
}

/// Station subscription filter (network + station + optional SELECT/TIME filters).
#[derive(Clone, Debug)]
pub(crate) struct Subscription {
    pub network: String,
    pub station: String,
    pub select_patterns: Vec<SelectPattern>,
    pub time_window: Option<TimeWindow>,
}

impl Subscription {
    /// Check if a payload matches this subscription's SELECT patterns.
    ///
    /// Empty `select_patterns` → match all (no SELECT = all channels).
    /// Non-empty → any pattern matches = pass (OR logic).
    pub fn matches_channel(&self, payload: &[u8]) -> bool {
        if self.select_patterns.is_empty() {
            return true;
        }
        self.select_patterns
            .iter()
            .any(|p| p.matches_payload(payload))
    }

    /// Check if a payload's BTime timestamp falls within the TIME window.
    ///
    /// - `None` time_window → pass all (no TIME = no filter)
    /// - `Some(tw)` → parse BTime from payload, check `tw.contains()`
    /// - Unparseable BTime → reject (return false)
    pub fn matches_time(&self, payload: &[u8]) -> bool {
        let Some(ref tw) = self.time_window else {
            return true;
        };
        match Timestamp::from_mseed_payload(payload) {
            Some(ts) => tw.contains(ts),
            None => false,
        }
    }
}

/// Station info returned by `DataStore::station_info()`.
#[derive(Clone, Debug)]
pub(crate) struct StationInfo {
    pub network: String,
    pub station: String,
    pub begin_seq: u64,
    pub end_seq: u64,
}

/// Stream info returned by `DataStore::stream_info()`.
#[derive(Clone, Debug)]
pub(crate) struct StreamInfo {
    pub network: String,
    pub station: String,
    pub channel: String,
    pub location: String,
    pub type_code: String,
    pub begin_seq: u64,
    pub end_seq: u64,
}

struct Ring {
    buf: VecDeque<Record>,
    capacity: usize,
    next_seq: u64,
    journal: Option<Journal>,
}

impl Ring {
    fn new(capacity: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(capacity),
            capacity,
            next_seq: 1,
            journal: None,
        }
    }

    /// Push a record: journal first (if configured), then memory.
    ///
    /// Returns the assigned sequence and whether the journal append succeeded
    /// (`true` when no journal is configured).
    fn push(
        &mut self,
        network: String,
        station: String,
        payload: Vec<u8>,
    ) -> (SequenceNumber, bool) {
        let seq = SequenceNumber::new(self.next_seq);

        let durable = match self.journal.as_mut() {
            Some(j) => match j.append(seq.value(), &network, &station, &payload) {
                Ok(()) => true,
                Err(e) => {
                    error!(error = %e, sequence = seq.value(), "journal append failed");
                    false
                }
            },
            None => true,
        };

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

        self.next_seq = seq_next(self.next_seq);

        (seq, durable)
    }

    fn read_since(&self, cursor: u64, subscriptions: &[Subscription]) -> Vec<Record> {
        let cursor = cursor & SequenceNumber::V3_MAX;
        self.buf
            .iter()
            .filter(|r| cursor == 0 || seq_is_after(r.sequence.value(), cursor))
            .filter(|r| {
                subscriptions.iter().any(|s| {
                    s.network.eq_ignore_ascii_case(&r.network)
                        && s.station.eq_ignore_ascii_case(&r.station)
                        && s.matches_channel(&r.payload)
                        && s.matches_time(&r.payload)
                })
            })
            .cloned()
            .collect()
    }
}

struct StoreInner {
    ring: Mutex<Ring>,
    notify: Notify,
    journal_healthy: AtomicBool,
}

/// Thread-safe data store backed by an in-memory ring buffer, optionally
/// mirrored to an on-disk journal for restart-safe `DATA <seq>` resume.
///
/// Clone is cheap (Arc).
#[derive(Clone)]
pub struct DataStore(Arc<StoreInner>);

impl DataStore {
    /// Create a new in-memory store with the given ring buffer capacity.
    ///
    /// Records do not survive a restart; clients resuming with `DATA <seq>`
    /// after a restart start from an empty ring. Use [`DataStore::with_journal`]
    /// for restart-safe resume.
    pub fn new(capacity: usize) -> Self {
        Self(Arc::new(StoreInner {
            ring: Mutex::new(Ring::new(capacity)),
            notify: Notify::new(),
            journal_healthy: AtomicBool::new(true),
        }))
    }

    /// Create a store whose ring buffer is journaled to `dir`.
    ///
    /// On open, existing journaled records are recovered into the ring and
    /// the sequence counter continues where it left off, so SeedLink clients
    /// can resume with `DATA <seq>` across server restarts.
    ///
    /// After a crash with [`SyncPolicy::EveryN`] or [`SyncPolicy::Os`], the
    /// sequence counter is additionally advanced past the potentially lost
    /// tail window so a stale sequence number never aliases different data.
    ///
    /// Only one process may use a journal directory at a time.
    pub fn with_journal(capacity: usize, dir: impl AsRef<Path>, sync: SyncPolicy) -> Result<Self> {
        let recovery = Journal::open(dir.as_ref(), capacity, sync).map_err(ServerError::Journal)?;

        let mut ring = Ring::new(capacity);
        ring.journal = Some(recovery.journal);
        for r in recovery.records {
            ring.buf.push_back(Record {
                sequence: SequenceNumber::new(r.sequence),
                network: r.network,
                station: r.station,
                payload: r.payload,
            });
        }
        if let Some(last) = recovery.last_seq {
            ring.next_seq = seq_advance(last, 1 + sync.recovery_seq_margin());
        }

        info!(
            dir = %dir.as_ref().display(),
            recovered = ring.buf.len(),
            next_seq = ring.next_seq,
            "journaled data store opened"
        );

        Ok(Self(Arc::new(StoreInner {
            ring: Mutex::new(ring),
            notify: Notify::new(),
            journal_healthy: AtomicBool::new(true),
        })))
    }

    /// Push a miniSEED record into the ring buffer.
    ///
    /// `payload` is typically a 512-byte miniSEED v2 record, but any length
    /// in `1..=MAX_PAYLOAD_LEN` is accepted (variable-length miniSEED v3
    /// records stream to SeedLink v4 clients; v3 connections can only carry
    /// 512-byte records on the wire and skip others).
    ///
    /// Returns the assigned sequence number.
    ///
    /// # Errors
    ///
    /// [`ServerError::EmptyPayload`] / [`ServerError::PayloadTooLarge`] if the
    /// payload is rejected — nothing is stored. Network/station names longer
    /// than 255 bytes are rejected as [`ServerError::PayloadTooLarge`].
    ///
    /// A journal write failure does **not** fail the push: the record is
    /// still live-streamed from memory, the failure is logged, and
    /// [`DataStore::journal_healthy`] flips to `false` until an append
    /// succeeds again (e.g. disk space freed).
    pub fn push(&self, network: &str, station: &str, payload: &[u8]) -> Result<SequenceNumber> {
        if payload.is_empty() {
            return Err(ServerError::EmptyPayload);
        }
        if payload.len() > MAX_PAYLOAD_LEN {
            return Err(ServerError::PayloadTooLarge {
                len: payload.len(),
                max: MAX_PAYLOAD_LEN,
            });
        }
        if network.len() > u8::MAX as usize || station.len() > u8::MAX as usize {
            return Err(ServerError::PayloadTooLarge {
                len: network.len().max(station.len()),
                max: u8::MAX as usize,
            });
        }

        let (seq, durable) = self.0.ring.lock().unwrap().push(
            network.to_owned(),
            station.to_owned(),
            payload.to_vec(),
        );

        let was_healthy = self.0.journal_healthy.swap(durable, Ordering::Relaxed);
        if durable && !was_healthy {
            info!("journal recovered — appends succeeding again");
        }

        self.0.notify.notify_waiters();
        Ok(seq)
    }

    /// `false` while journal appends are failing (e.g. disk full). Records
    /// keep streaming from memory but will not survive a restart. Always
    /// `true` for stores without a journal.
    pub fn journal_healthy(&self) -> bool {
        self.0.journal_healthy.load(Ordering::Relaxed)
    }

    /// Read all records after `cursor` that match the given subscriptions.
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

    /// Enumerate unique stations in the ring with min/max sequence numbers.
    pub(crate) fn station_info(&self) -> Vec<StationInfo> {
        let ring = self.0.ring.lock().unwrap();
        // Key: (network, station) → (begin_seq, end_seq)
        let mut map: BTreeMap<(String, String), (u64, u64)> = BTreeMap::new();
        for r in &ring.buf {
            let key = (r.network.clone(), r.station.clone());
            let seq = r.sequence.value();
            map.entry(key)
                .and_modify(|(begin, end)| {
                    if seq < *begin {
                        *begin = seq;
                    }
                    if seq > *end {
                        *end = seq;
                    }
                })
                .or_insert((seq, seq));
        }
        map.into_iter()
            .map(|((network, station), (begin_seq, end_seq))| StationInfo {
                network,
                station,
                begin_seq,
                end_seq,
            })
            .collect()
    }

    /// Enumerate unique streams in the ring with channel detail extracted from payload bytes.
    pub(crate) fn stream_info(&self) -> Vec<StreamInfo> {
        type StreamKey = (String, String, String, String);
        type StreamVal = (String, u64, u64);

        let ring = self.0.ring.lock().unwrap();
        // Key: (network, station, location, channel) → (type_code, begin_seq, end_seq)
        let mut map: BTreeMap<StreamKey, StreamVal> = BTreeMap::new();
        for r in &ring.buf {
            if r.payload.len() < 20 {
                continue;
            }
            let location = String::from_utf8_lossy(&r.payload[13..15]).to_string();
            let channel = String::from_utf8_lossy(&r.payload[15..18]).to_string();
            let type_code = String::from_utf8_lossy(&r.payload[6..7]).to_string();
            let key = (r.network.clone(), r.station.clone(), location, channel);
            let seq = r.sequence.value();
            map.entry(key)
                .and_modify(|(tc, begin, end)| {
                    // Keep latest type code
                    *tc = type_code.clone();
                    if seq < *begin {
                        *begin = seq;
                    }
                    if seq > *end {
                        *end = seq;
                    }
                })
                .or_insert((type_code, seq, seq));
        }
        map.into_iter()
            .map(
                |((network, station, location, channel), (type_code, begin_seq, end_seq))| {
                    StreamInfo {
                        network,
                        station,
                        channel,
                        location,
                        type_code,
                        begin_seq,
                        end_seq,
                    }
                },
            )
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seedlink_rs_protocol::frame::v3;

    fn dummy_payload() -> Vec<u8> {
        vec![0u8; v3::PAYLOAD_LEN]
    }

    fn sub(network: &str, station: &str) -> Subscription {
        Subscription {
            network: network.into(),
            station: station.into(),
            select_patterns: vec![],
            time_window: None,
        }
    }

    #[test]
    fn push_assigns_increasing_sequences() {
        let store = DataStore::new(100);
        let s1 = store.push("IU", "ANMO", &dummy_payload()).unwrap();
        let s2 = store.push("IU", "ANMO", &dummy_payload()).unwrap();
        let s3 = store.push("GE", "WLF", &dummy_payload()).unwrap();
        assert_eq!(s1.value(), 1);
        assert_eq!(s2.value(), 2);
        assert_eq!(s3.value(), 3);
    }

    #[test]
    fn read_since_filters_by_subscription() {
        let store = DataStore::new(100);
        store.push("IU", "ANMO", &dummy_payload()).unwrap();
        store.push("GE", "WLF", &dummy_payload()).unwrap();
        store.push("IU", "ANMO", &dummy_payload()).unwrap();

        let subs = vec![sub("IU", "ANMO")];

        let records = store.read_since(0, &subs);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].sequence.value(), 1);
        assert_eq!(records[1].sequence.value(), 3);
    }

    #[test]
    fn read_since_respects_cursor() {
        let store = DataStore::new(100);
        store.push("IU", "ANMO", &dummy_payload()).unwrap();
        store.push("IU", "ANMO", &dummy_payload()).unwrap();
        store.push("IU", "ANMO", &dummy_payload()).unwrap();

        let subs = vec![sub("IU", "ANMO")];

        let records = store.read_since(2, &subs);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].sequence.value(), 3);
    }

    #[test]
    fn eviction_on_capacity() {
        let store = DataStore::new(3);
        for _ in 0..5 {
            store.push("IU", "ANMO", &dummy_payload()).unwrap();
        }

        let subs = vec![sub("IU", "ANMO")];

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

        let s1 = store.push("IU", "ANMO", &dummy_payload()).unwrap();
        let s2 = store.push("IU", "ANMO", &dummy_payload()).unwrap();

        assert_eq!(s1.value(), SequenceNumber::V3_MAX);
        assert_eq!(s2.value(), 1); // wrapped
    }

    #[test]
    fn read_since_survives_sequence_wrap() {
        let store = DataStore::new(10);
        {
            let mut ring = store.0.ring.lock().unwrap();
            ring.next_seq = SequenceNumber::V3_MAX - 1;
        }
        // Sequences: V3_MAX-1, V3_MAX, 1, 2
        for _ in 0..4 {
            store.push("IU", "ANMO", &dummy_payload()).unwrap();
        }
        let subs = vec![sub("IU", "ANMO")];

        // Client resumes from V3_MAX-1: must see V3_MAX, then post-wrap 1 and 2.
        let records = store.read_since(SequenceNumber::V3_MAX - 1, &subs);
        let seqs: Vec<u64> = records.iter().map(|r| r.sequence.value()).collect();
        assert_eq!(seqs, vec![SequenceNumber::V3_MAX, 1, 2]);

        // Client resumes from post-wrap seq 1: only 2 remains.
        let records = store.read_since(1, &subs);
        let seqs: Vec<u64> = records.iter().map(|r| r.sequence.value()).collect();
        assert_eq!(seqs, vec![2]);
    }

    #[test]
    fn push_rejects_empty_payload() {
        let store = DataStore::new(10);
        assert!(matches!(
            store.push("IU", "ANMO", &[]),
            Err(ServerError::EmptyPayload)
        ));
    }

    #[test]
    fn push_rejects_oversized_payload() {
        let store = DataStore::new(10);
        let huge = vec![0u8; MAX_PAYLOAD_LEN + 1];
        assert!(matches!(
            store.push("IU", "ANMO", &huge),
            Err(ServerError::PayloadTooLarge { .. })
        ));
    }

    #[test]
    fn push_accepts_variable_length_payloads() {
        let store = DataStore::new(10);
        store.push("IU", "ANMO", &[0u8; 100]).unwrap();
        store.push("IU", "ANMO", &[0u8; 4096]).unwrap();
        let records = store.read_since(0, &[sub("IU", "ANMO")]);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].payload.len(), 100);
        assert_eq!(records[1].payload.len(), 4096);
    }

    #[test]
    fn journaled_store_resumes_after_restart() {
        let tmp = crate::journal::testutil::TempDir::new("store-restart");
        {
            let store = DataStore::with_journal(100, &tmp.0, SyncPolicy::EveryRecord).unwrap();
            for _ in 0..5 {
                store.push("IU", "ANMO", &dummy_payload()).unwrap();
            }
            assert!(store.journal_healthy());
        }
        // "Restart": reopen from the same directory.
        let store = DataStore::with_journal(100, &tmp.0, SyncPolicy::EveryRecord).unwrap();
        let records = store.read_since(3, &[sub("IU", "ANMO")]);
        let seqs: Vec<u64> = records.iter().map(|r| r.sequence.value()).collect();
        assert_eq!(seqs, vec![4, 5], "resume from seq 3 must yield 4 and 5");

        // New pushes continue the sequence, no reuse.
        let s = store.push("IU", "ANMO", &dummy_payload()).unwrap();
        assert_eq!(s.value(), 6);
    }

    #[test]
    fn journaled_store_advances_seq_margin_for_every_n() {
        let tmp = crate::journal::testutil::TempDir::new("store-margin");
        {
            let store = DataStore::with_journal(100, &tmp.0, SyncPolicy::EveryN(10)).unwrap();
            // With EveryN(10), records are still written+flushed per push;
            // margin protects against the fsync window after a power cut.
            store.push("IU", "ANMO", &dummy_payload()).unwrap();
        }
        let store = DataStore::with_journal(100, &tmp.0, SyncPolicy::EveryN(10)).unwrap();
        let s = store.push("IU", "ANMO", &dummy_payload()).unwrap();
        assert_eq!(s.value(), 1 + 1 + 10, "next = last + 1 + margin(10)");
    }
}
