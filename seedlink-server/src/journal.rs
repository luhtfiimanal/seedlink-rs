//! On-disk journal backing the ring buffer, enabling clients to resume
//! with `DATA <seq>` across server restarts.
//!
//! Layout: a directory of append-only segment files named
//! `journal-<16-hex-digit index>.slj`. The segment index increases
//! monotonically and never wraps, so lexicographic file order equals
//! chronological order even when sequence numbers wrap at the v3
//! 24-bit boundary.
//!
//! Segment format:
//!
//! ```text
//! [0..8]   segment magic  b"SLJRNL1\n"
//! then per record:
//!   [0..2]   record magic  b"SR"
//!   [2..10]  sequence      u64 LE
//!   [10]     network len   u8
//!   [11]     station len   u8
//!   [12..16] payload len   u32 LE
//!   [..]     network bytes, station bytes, payload bytes
//!   [..+4]   CRC-32 (IEEE) u32 LE over bytes [2 .. end of payload]
//! ```
//!
//! Recovery reads segments in order and stops consuming a segment at the
//! first truncated or corrupt record (the tail lost in a crash), then
//! continues with the next segment. Only a single process may write a
//! journal directory at a time.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use tracing::{debug, warn};

use crate::store::MAX_PAYLOAD_LEN;

const SEGMENT_MAGIC: &[u8; 8] = b"SLJRNL1\n";
const RECORD_MAGIC: &[u8; 2] = b"SR";
const RECORD_HEAD_LEN: usize = 16;
const SEGMENT_EXT: &str = "slj";

/// When to fsync journal appends to disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncPolicy {
    /// fsync after every record. Maximum durability; on restart the ring
    /// resumes exactly where it left off. May cost ~1 ms per push on
    /// slow media (SD cards).
    EveryRecord,
    /// fsync every N records (and on segment rotation). Up to N-1 records
    /// may be lost on power failure; on recovery the sequence counter is
    /// advanced past the potentially lost window so stale sequence numbers
    /// are never reused for different data.
    EveryN(u32),
    /// Never fsync explicitly (except on segment rotation) — leave flushing
    /// to the OS page cache. Fastest, weakest durability.
    Os,
}

impl SyncPolicy {
    /// How many sequence numbers to skip past the last durable record on
    /// recovery, so records lost in a crash can never alias new data.
    pub(crate) fn recovery_seq_margin(self) -> u64 {
        match self {
            SyncPolicy::EveryRecord => 0,
            SyncPolicy::EveryN(n) => u64::from(n),
            SyncPolicy::Os => 4096,
        }
    }
}

/// A record recovered from the journal.
pub(crate) struct RecoveredRecord {
    pub sequence: u64,
    pub network: String,
    pub station: String,
    pub payload: Vec<u8>,
}

/// Result of opening a journal directory.
pub(crate) struct Recovery {
    pub journal: Journal,
    /// Records in chronological (append) order, at most `capacity` newest.
    pub records: Vec<RecoveredRecord>,
    /// Sequence of the last durable record, if any.
    pub last_seq: Option<u64>,
}

struct Segment {
    index: u64,
    records: u32,
}

/// Append-only segmented journal writer.
pub(crate) struct Journal {
    dir: PathBuf,
    writer: BufWriter<File>,
    current_index: u64,
    current_records: u32,
    records_per_segment: u32,
    /// Closed (rotated) segments, oldest first.
    closed: VecDeque<Segment>,
    capacity: usize,
    sync: SyncPolicy,
    unsynced: u32,
}

impl Journal {
    /// Open (or create) a journal directory, recovering existing records.
    ///
    /// `capacity` is the ring capacity: recovery returns at most this many
    /// newest records, and on-disk eviction keeps at least this many.
    pub fn open(dir: &Path, capacity: usize, sync: SyncPolicy) -> io::Result<Recovery> {
        fs::create_dir_all(dir)?;

        let records_per_segment = (capacity / 4).clamp(64, 65_536) as u32;

        // Enumerate segments in index order.
        let mut indices: Vec<u64> = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            if let Some(idx) = parse_segment_name(&entry.file_name().to_string_lossy()) {
                indices.push(idx);
            }
        }
        indices.sort_unstable();

        let mut records: VecDeque<RecoveredRecord> = VecDeque::new();
        let mut closed: VecDeque<Segment> = VecDeque::new();
        let mut last_segment_clean = true;
        let mut last_segment_records = 0u32;

        for &idx in &indices {
            let path = segment_path(dir, idx);
            let (recs, clean) = read_segment(&path)?;
            last_segment_clean = clean;
            last_segment_records = recs.len() as u32;
            closed.push_back(Segment {
                index: idx,
                records: recs.len() as u32,
            });
            for r in recs {
                records.push_back(r);
                if records.len() > capacity {
                    records.pop_front();
                }
            }
        }

        let last_seq = records.back().map(|r| r.sequence);

        // Decide where to write next: append to the last segment only if it
        // is fully valid and not full — appending after a corrupt tail would
        // put unreachable records behind garbage.
        let (current_index, current_records, reuse) = match indices.last() {
            Some(&last_idx) if last_segment_clean && last_segment_records < records_per_segment => {
                closed.pop_back();
                (last_idx, last_segment_records, true)
            }
            Some(&last_idx) => (last_idx + 1, 0, false),
            None => (0, 0, false),
        };

        let path = segment_path(dir, current_index);
        let file = if reuse {
            let mut f = OpenOptions::new().append(true).open(&path)?;
            f.seek(SeekFrom::End(0))?;
            f
        } else {
            let mut f = OpenOptions::new()
                .create_new(true)
                .append(true)
                .open(&path)?;
            f.write_all(SEGMENT_MAGIC)?;
            f.sync_data()?;
            sync_dir(dir);
            f
        };

        let journal = Journal {
            dir: dir.to_path_buf(),
            writer: BufWriter::new(file),
            current_index,
            current_records,
            records_per_segment,
            closed,
            capacity,
            sync,
            unsynced: 0,
        };

        debug!(
            dir = %dir.display(),
            recovered = records.len(),
            last_seq = ?last_seq,
            "journal opened"
        );

        Ok(Recovery {
            journal,
            records: records.into(),
            last_seq,
        })
    }

    /// Append one record. Callers must pass `payload.len() <= MAX_PAYLOAD_LEN`
    /// and name lengths that fit in `u8` (enforced by the store).
    pub fn append(
        &mut self,
        sequence: u64,
        network: &str,
        station: &str,
        payload: &[u8],
    ) -> io::Result<()> {
        let net = network.as_bytes();
        let sta = station.as_bytes();

        let mut head = [0u8; RECORD_HEAD_LEN];
        head[0..2].copy_from_slice(RECORD_MAGIC);
        head[2..10].copy_from_slice(&sequence.to_le_bytes());
        head[10] = net.len() as u8;
        head[11] = sta.len() as u8;
        head[12..16].copy_from_slice(&(payload.len() as u32).to_le_bytes());

        let mut crc = Crc32::new();
        crc.update(&head[2..]);
        crc.update(net);
        crc.update(sta);
        crc.update(payload);

        self.writer.write_all(&head)?;
        self.writer.write_all(net)?;
        self.writer.write_all(sta)?;
        self.writer.write_all(payload)?;
        self.writer.write_all(&crc.finalize().to_le_bytes())?;
        self.writer.flush()?;

        self.current_records += 1;
        self.unsynced += 1;

        let must_sync = match self.sync {
            SyncPolicy::EveryRecord => true,
            SyncPolicy::EveryN(n) => self.unsynced >= n.max(1),
            SyncPolicy::Os => false,
        };
        if must_sync {
            self.writer.get_ref().sync_data()?;
            self.unsynced = 0;
        }

        if self.current_records >= self.records_per_segment {
            self.rotate()?;
        }
        Ok(())
    }

    /// Close the current segment, open the next one, evict old segments.
    fn rotate(&mut self) -> io::Result<()> {
        self.writer.flush()?;
        self.writer.get_ref().sync_data()?;
        self.unsynced = 0;

        self.closed.push_back(Segment {
            index: self.current_index,
            records: self.current_records,
        });

        self.current_index += 1;
        self.current_records = 0;
        let path = segment_path(&self.dir, self.current_index);
        let mut file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&path)?;
        file.write_all(SEGMENT_MAGIC)?;
        file.sync_data()?;
        self.writer = BufWriter::new(file);

        // Evict oldest segments while the remaining closed ones still hold
        // at least `capacity` records.
        let mut total: u64 = self.closed.iter().map(|s| u64::from(s.records)).sum();
        while let Some(oldest) = self.closed.front() {
            if total - u64::from(oldest.records) < self.capacity as u64 {
                break;
            }
            let seg = self.closed.pop_front().expect("front checked above");
            total -= u64::from(seg.records);
            let path = segment_path(&self.dir, seg.index);
            if let Err(e) = fs::remove_file(&path) {
                warn!(path = %path.display(), error = %e, "failed to evict journal segment");
            }
        }
        sync_dir(&self.dir);
        Ok(())
    }
}

fn segment_path(dir: &Path, index: u64) -> PathBuf {
    dir.join(format!("journal-{index:016x}.{SEGMENT_EXT}"))
}

fn parse_segment_name(name: &str) -> Option<u64> {
    let stem = name
        .strip_prefix("journal-")?
        .strip_suffix(&format!(".{SEGMENT_EXT}"))?;
    if stem.len() != 16 {
        return None;
    }
    u64::from_str_radix(stem, 16).ok()
}

/// Read one segment. Returns records read and whether the segment was fully
/// clean (no truncated/corrupt tail).
fn read_segment(path: &Path) -> io::Result<(Vec<RecoveredRecord>, bool)> {
    let mut data = Vec::new();
    File::open(path)?.read_to_end(&mut data)?;

    if data.len() < SEGMENT_MAGIC.len() || &data[..SEGMENT_MAGIC.len()] != SEGMENT_MAGIC {
        warn!(path = %path.display(), "journal segment has bad magic, skipping");
        return Ok((Vec::new(), false));
    }

    let mut records = Vec::new();
    let mut pos = SEGMENT_MAGIC.len();
    let clean = loop {
        if pos == data.len() {
            break true; // exact end — clean
        }
        let Some(head) = data.get(pos..pos + RECORD_HEAD_LEN) else {
            break false; // truncated head
        };
        if &head[0..2] != RECORD_MAGIC {
            warn!(path = %path.display(), offset = pos, "bad record magic, dropping tail");
            break false;
        }
        let sequence = u64::from_le_bytes(head[2..10].try_into().expect("8 bytes"));
        let net_len = head[10] as usize;
        let sta_len = head[11] as usize;
        let payload_len = u32::from_le_bytes(head[12..16].try_into().expect("4 bytes")) as usize;
        if payload_len > MAX_PAYLOAD_LEN {
            warn!(path = %path.display(), offset = pos, "implausible payload length, dropping tail");
            break false;
        }
        let body_len = net_len + sta_len + payload_len;
        let total = RECORD_HEAD_LEN + body_len + 4;
        let Some(rest) = data.get(pos + RECORD_HEAD_LEN..pos + total) else {
            break false; // truncated body/crc
        };
        let (body, crc_bytes) = rest.split_at(body_len);

        let mut crc = Crc32::new();
        crc.update(&head[2..]);
        crc.update(body);
        let stored = u32::from_le_bytes(crc_bytes.try_into().expect("4 bytes"));
        if crc.finalize() != stored {
            warn!(path = %path.display(), offset = pos, "CRC mismatch, dropping tail");
            break false;
        }

        let (net, rest) = body.split_at(net_len);
        let (sta, payload) = rest.split_at(sta_len);
        records.push(RecoveredRecord {
            sequence,
            network: String::from_utf8_lossy(net).into_owned(),
            station: String::from_utf8_lossy(sta).into_owned(),
            payload: payload.to_vec(),
        });
        pos += total;
    };

    Ok((records, clean))
}

/// fsync a directory so segment create/delete are durable. Best-effort.
fn sync_dir(dir: &Path) {
    if let Ok(f) = File::open(dir)
        && let Err(e) = f.sync_all()
    {
        debug!(dir = %dir.display(), error = %e, "dir fsync failed");
    }
}

/// Minimal CRC-32 (IEEE 802.3, reflected) — zero-dependency.
struct Crc32 {
    state: u32,
}

impl Crc32 {
    fn new() -> Self {
        Self { state: 0xFFFF_FFFF }
    }

    fn update(&mut self, data: &[u8]) {
        for &byte in data {
            let mut cur = (self.state ^ u32::from(byte)) & 0xFF;
            for _ in 0..8 {
                cur = if cur & 1 != 0 {
                    (cur >> 1) ^ 0xEDB8_8320
                } else {
                    cur >> 1
                };
            }
            self.state = (self.state >> 8) ^ cur;
        }
    }

    fn finalize(self) -> u32 {
        self.state ^ 0xFFFF_FFFF
    }
}

/// Test-only helpers shared across the crate's test modules.
#[cfg(test)]
pub(crate) mod testutil {
    use std::fs;
    use std::path::PathBuf;

    /// Self-cleaning temp dir without external deps.
    pub(crate) struct TempDir(pub PathBuf);

    impl TempDir {
        pub fn new(tag: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("seedlink-rs-journal-{}-{tag}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::TempDir;
    use super::*;

    #[test]
    fn crc32_known_vector() {
        // CRC-32("123456789") = 0xCBF43926 (IEEE check value)
        let mut crc = Crc32::new();
        crc.update(b"123456789");
        assert_eq!(crc.finalize(), 0xCBF4_3926);
    }

    #[test]
    fn append_and_recover_roundtrip() {
        let tmp = TempDir::new("roundtrip");
        {
            let rec = Journal::open(&tmp.0, 100, SyncPolicy::EveryRecord).unwrap();
            assert!(rec.records.is_empty());
            let mut j = rec.journal;
            j.append(1, "IU", "ANMO", &[0xAA; 512]).unwrap();
            j.append(2, "IU", "ANMO", &[0xBB; 512]).unwrap();
            j.append(3, "GE", "WLF", &[0xCC; 256]).unwrap();
        }
        let rec = Journal::open(&tmp.0, 100, SyncPolicy::EveryRecord).unwrap();
        assert_eq!(rec.records.len(), 3);
        assert_eq!(rec.last_seq, Some(3));
        assert_eq!(rec.records[0].sequence, 1);
        assert_eq!(rec.records[0].network, "IU");
        assert_eq!(rec.records[0].station, "ANMO");
        assert_eq!(rec.records[0].payload, vec![0xAA; 512]);
        assert_eq!(rec.records[2].sequence, 3);
        assert_eq!(rec.records[2].payload.len(), 256);
    }

    #[test]
    fn recovery_keeps_only_capacity_newest() {
        let tmp = TempDir::new("capacity");
        {
            let mut j = Journal::open(&tmp.0, 100, SyncPolicy::EveryRecord)
                .unwrap()
                .journal;
            for seq in 1..=10u64 {
                j.append(seq, "IU", "ANMO", &[seq as u8; 64]).unwrap();
            }
        }
        let rec = Journal::open(&tmp.0, 3, SyncPolicy::EveryRecord).unwrap();
        assert_eq!(rec.records.len(), 3);
        assert_eq!(rec.records[0].sequence, 8);
        assert_eq!(rec.records[2].sequence, 10);
        assert_eq!(rec.last_seq, Some(10));
    }

    #[test]
    fn truncated_tail_is_dropped() {
        let tmp = TempDir::new("truncated");
        {
            let mut j = Journal::open(&tmp.0, 100, SyncPolicy::EveryRecord)
                .unwrap()
                .journal;
            j.append(1, "IU", "ANMO", &[1; 128]).unwrap();
            j.append(2, "IU", "ANMO", &[2; 128]).unwrap();
        }
        // Chop bytes off the end — simulates a crash mid-write.
        let seg = segment_path(&tmp.0, 0);
        let len = fs::metadata(&seg).unwrap().len();
        let f = OpenOptions::new().write(true).open(&seg).unwrap();
        f.set_len(len - 10).unwrap();

        let rec = Journal::open(&tmp.0, 100, SyncPolicy::EveryRecord).unwrap();
        assert_eq!(rec.records.len(), 1);
        assert_eq!(rec.last_seq, Some(1));

        // The reopened journal must NOT append after the garbage tail —
        // it should have started a fresh segment.
        let mut j = rec.journal;
        j.append(100, "IU", "ANMO", &[3; 128]).unwrap();
        drop(j);
        let rec = Journal::open(&tmp.0, 100, SyncPolicy::EveryRecord).unwrap();
        let seqs: Vec<u64> = rec.records.iter().map(|r| r.sequence).collect();
        assert_eq!(seqs, vec![1, 100]);
    }

    #[test]
    fn corrupt_crc_drops_tail() {
        let tmp = TempDir::new("crc");
        {
            let mut j = Journal::open(&tmp.0, 100, SyncPolicy::EveryRecord)
                .unwrap()
                .journal;
            j.append(1, "IU", "ANMO", &[1; 128]).unwrap();
            j.append(2, "IU", "ANMO", &[2; 128]).unwrap();
        }
        // Flip a payload byte of record 2.
        let seg = segment_path(&tmp.0, 0);
        let mut data = fs::read(&seg).unwrap();
        let rec1_total = RECORD_HEAD_LEN + 2 + 4 + 128 + 4;
        let idx = SEGMENT_MAGIC.len() + rec1_total + RECORD_HEAD_LEN + 2 + 4 + 60;
        data[idx] ^= 0xFF;
        fs::write(&seg, &data).unwrap();

        let rec = Journal::open(&tmp.0, 100, SyncPolicy::EveryRecord).unwrap();
        assert_eq!(rec.records.len(), 1);
        assert_eq!(rec.last_seq, Some(1));
    }

    #[test]
    fn rotation_evicts_but_keeps_at_least_capacity() {
        let tmp = TempDir::new("evict");
        // capacity 100 → records_per_segment clamps to 64
        let mut j = Journal::open(&tmp.0, 100, SyncPolicy::Os).unwrap().journal;
        for seq in 1..=1000u64 {
            j.append(seq, "IU", "ANMO", &[0; 16]).unwrap();
        }
        drop(j);

        let n_segments = fs::read_dir(&tmp.0).unwrap().count();
        assert!(
            n_segments <= 5,
            "old segments must be evicted, found {n_segments}"
        );

        let rec = Journal::open(&tmp.0, 100, SyncPolicy::Os).unwrap();
        assert!(
            rec.records.len() >= 100,
            "must retain at least capacity records, got {}",
            rec.records.len()
        );
        assert_eq!(rec.last_seq, Some(1000));
    }

    #[test]
    fn sync_policy_margins() {
        assert_eq!(SyncPolicy::EveryRecord.recovery_seq_margin(), 0);
        assert_eq!(SyncPolicy::EveryN(50).recovery_seq_margin(), 50);
        assert_eq!(SyncPolicy::Os.recovery_seq_margin(), 4096);
    }
}
