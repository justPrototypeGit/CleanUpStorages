//! Where a scan's time goes.
//!
//! Phase timings and counters accumulate into atomics, so the same accumulator works unchanged
//! when #23 hashes on worker threads. Cost per file is two `Instant::now()` calls and a relaxed
//! `fetch_add` — nanoseconds against millisecond-scale disk I/O.

use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;

/// A disjoint slice of scan work. No timer for one phase may be alive inside another's scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Walk,
    SkipCheck,
    Hash,
    DbWrite,
    Archive,
}

impl Phase {
    const COUNT: usize = 5;
    fn index(self) -> usize {
        match self {
            Phase::Walk => 0,
            Phase::SkipCheck => 1,
            Phase::Hash => 2,
            Phase::DbWrite => 3,
            Phase::Archive => 4,
        }
    }
}

pub const BUCKET_COUNT: usize = 7;

/// Human labels for the size histogram, in bucket order.
pub const BUCKET_LABELS: [&str; BUCKET_COUNT] = [
    "0",
    "1B-4KiB",
    "4KiB-64KiB",
    "64KiB-1MiB",
    "1MiB-16MiB",
    "16MiB-256MiB",
    ">256MiB",
];

/// Bucket index for a file size. Upper bounds are exclusive: 4096 lands in `4KiB-64KiB`.
/// The 64 KiB boundary is deliberate — it is both the current read-buffer size and the line the
/// seek-bound hypothesis turns on.
pub fn bucket_for(size_bytes: i64) -> usize {
    match size_bytes {
        s if s <= 0 => 0,
        s if s < 4_096 => 1,
        s if s < 65_536 => 2,
        s if s < 1_048_576 => 3,
        s if s < 16_777_216 => 4,
        s if s < 268_435_456 => 5,
        _ => 6,
    }
}

/// Accumulator for one scan. `Send + Sync` so #23 can share it across workers via `Arc`.
pub struct ScanMetrics {
    started: Instant,
    phase_ns: [AtomicU64; Phase::COUNT],
    files_seen: AtomicU64,
    bytes_hashed: AtomicU64,
    bytes_skipped: AtomicU64,
    histogram: [AtomicU64; BUCKET_COUNT],
}

impl Default for ScanMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl ScanMetrics {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            phase_ns: Default::default(),
            files_seen: AtomicU64::new(0),
            bytes_hashed: AtomicU64::new(0),
            bytes_skipped: AtomicU64::new(0),
            histogram: Default::default(),
        }
    }

    /// Start timing `phase`. The elapsed time is added when the returned guard drops.
    pub fn timer(&self, phase: Phase) -> PhaseTimer<'_> {
        PhaseTimer {
            metrics: self,
            phase,
            started: Instant::now(),
        }
    }

    /// Count a file the walk considered — including one about to be skipped, because a skipped
    /// file still costs a seek and a stat, which is the cost under investigation.
    pub fn record_file_seen(&self, size_bytes: i64) {
        self.files_seen.fetch_add(1, Relaxed);
        self.histogram[bucket_for(size_bytes)].fetch_add(1, Relaxed);
    }

    pub fn add_bytes_hashed(&self, n: i64) {
        self.bytes_hashed.fetch_add(n.max(0) as u64, Relaxed);
    }

    pub fn add_bytes_skipped(&self, n: i64) {
        self.bytes_skipped.fetch_add(n.max(0) as u64, Relaxed);
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        let ms = |i: usize| self.phase_ns[i].load(Relaxed) / 1_000_000;
        let mut histogram = [0u64; BUCKET_COUNT];
        for (i, slot) in self.histogram.iter().enumerate() {
            histogram[i] = slot.load(Relaxed);
        }
        MetricsSnapshot {
            wall_ms: self.started.elapsed().as_millis() as u64,
            walk_ms: ms(0),
            skip_check_ms: ms(1),
            hash_ms: ms(2),
            db_write_ms: ms(3),
            archive_ms: ms(4),
            files_seen: self.files_seen.load(Relaxed),
            bytes_hashed: self.bytes_hashed.load(Relaxed),
            bytes_skipped: self.bytes_skipped.load(Relaxed),
            histogram,
        }
    }
}

/// Adds its lifetime to one phase when dropped.
pub struct PhaseTimer<'a> {
    metrics: &'a ScanMetrics,
    phase: Phase,
    started: Instant,
}

impl Drop for PhaseTimer<'_> {
    fn drop(&mut self) {
        let ns = self.started.elapsed().as_nanos() as u64;
        self.metrics.phase_ns[self.phase.index()].fetch_add(ns, Relaxed);
    }
}

/// A point-in-time copy of the accumulator. Milliseconds throughout — nanosecond precision on a
/// multi-hour scan is noise.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct MetricsSnapshot {
    pub wall_ms: u64,
    pub walk_ms: u64,
    pub skip_check_ms: u64,
    pub hash_ms: u64,
    pub db_write_ms: u64,
    pub archive_ms: u64,
    pub files_seen: u64,
    pub bytes_hashed: u64,
    pub bytes_skipped: u64,
    pub histogram: [u64; BUCKET_COUNT],
}

impl MetricsSnapshot {
    pub fn total_phase_ms(&self) -> u64 {
        self.walk_ms + self.skip_check_ms + self.hash_ms + self.db_write_ms + self.archive_ms
    }

    /// Sum-of-phases over wall-clock. ~1.0 while the pipeline is sequential; after #23 this is the
    /// overlap actually achieved — the single number that says whether parallelising worked.
    pub fn overlap_ratio(&self) -> f64 {
        if self.wall_ms == 0 {
            return 0.0;
        }
        self.total_phase_ms() as f64 / self.wall_ms as f64
    }

    fn mb_per_s(bytes: u64, ms: u64) -> f64 {
        if ms == 0 {
            return 0.0;
        }
        (bytes as f64 / 1_048_576.0) / (ms as f64 / 1000.0)
    }

    /// Overall throughput including every phase.
    pub fn overall_mb_per_s(&self) -> f64 {
        Self::mb_per_s(self.bytes_hashed + self.bytes_skipped, self.wall_ms)
    }

    /// Throughput while actually hashing — the number #24 would move.
    pub fn hashing_mb_per_s(&self) -> f64 {
        Self::mb_per_s(self.bytes_hashed, self.hash_ms)
    }

    pub fn files_per_s(&self) -> f64 {
        if self.wall_ms == 0 {
            return 0.0;
        }
        self.files_seen as f64 / (self.wall_ms as f64 / 1000.0)
    }

    /// Multi-line human report. Every divisor is guarded, so a zero-length scan prints zeroes
    /// rather than `NaN`.
    pub fn report(&self) -> String {
        let pct = |ms: u64| {
            if self.wall_ms == 0 {
                0.0
            } else {
                ms as f64 * 100.0 / self.wall_ms as f64
            }
        };
        let mut out = format!(
            "Where the time went ({} ms wall):\n\
               walk        {:>8} ms  {:>5.1}%\n\
               skip_check  {:>8} ms  {:>5.1}%\n\
               hash        {:>8} ms  {:>5.1}%\n\
               db_write    {:>8} ms  {:>5.1}%\n\
               archive     {:>8} ms  {:>5.1}%\n\
               accounted   {:>8} ms  {:>5.1}%\n",
            self.wall_ms,
            self.walk_ms,
            pct(self.walk_ms),
            self.skip_check_ms,
            pct(self.skip_check_ms),
            self.hash_ms,
            pct(self.hash_ms),
            self.db_write_ms,
            pct(self.db_write_ms),
            self.archive_ms,
            pct(self.archive_ms),
            self.total_phase_ms(),
            // One definition of sum-of-phases ÷ wall, shared with every other caller.
            self.overlap_ratio() * 100.0,
        );
        out.push_str(&format!(
            "Rates: {:.1} files/s · {:.1} MB/s overall · {:.1} MB/s while hashing\n",
            self.files_per_s(),
            self.overall_mb_per_s(),
            self.hashing_mb_per_s(),
        ));
        out.push_str("File sizes seen:");
        for (i, label) in BUCKET_LABELS.iter().enumerate() {
            if self.histogram[i] > 0 {
                out.push_str(&format!(" {}={}", label, self.histogram[i]));
            }
        }
        out.push('\n');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_are_half_open_on_the_upper_bound() {
        assert_eq!(bucket_for(0), 0);
        assert_eq!(bucket_for(1), 1);
        assert_eq!(bucket_for(4095), 1);
        assert_eq!(bucket_for(4096), 2, "4 KiB starts the 4KiB-64KiB bucket");
        assert_eq!(bucket_for(65535), 2);
        assert_eq!(bucket_for(65536), 3, "64 KiB starts the 64KiB-1MiB bucket");
        assert_eq!(bucket_for(1_048_575), 3);
        assert_eq!(bucket_for(1_048_576), 4);
        assert_eq!(bucket_for(16_777_215), 4);
        assert_eq!(bucket_for(16_777_216), 5);
        assert_eq!(bucket_for(268_435_455), 5);
        assert_eq!(bucket_for(268_435_456), 6);
        assert_eq!(bucket_for(i64::MAX), 6);
    }

    #[test]
    fn labels_cover_every_bucket() {
        assert_eq!(BUCKET_LABELS.len(), BUCKET_COUNT);
        assert_eq!(bucket_for(i64::MAX), BUCKET_COUNT - 1);
    }

    #[test]
    fn recording_files_fills_counters_and_histogram() {
        let m = ScanMetrics::new();
        m.record_file_seen(0);
        m.record_file_seen(5_000);
        m.record_file_seen(5_000);
        m.add_bytes_hashed(10_000);
        m.add_bytes_skipped(7);
        let s = m.snapshot();
        assert_eq!(s.files_seen, 3);
        assert_eq!(s.histogram[0], 1, "the empty file");
        assert_eq!(s.histogram[2], 2, "two 5 KB files");
        assert_eq!(s.bytes_hashed, 10_000);
        assert_eq!(s.bytes_skipped, 7);
    }

    #[test]
    fn timer_accumulates_into_its_own_phase_only() {
        let m = ScanMetrics::new();
        {
            let _t = m.timer(Phase::Hash);
            std::thread::sleep(std::time::Duration::from_millis(12));
        }
        let s = m.snapshot();
        assert!(s.hash_ms >= 10, "expected >=10ms, got {}", s.hash_ms);
        assert_eq!(s.walk_ms, 0);
        assert_eq!(s.skip_check_ms, 0);
        assert_eq!(s.db_write_ms, 0);
        assert_eq!(s.archive_ms, 0);
    }

    #[test]
    fn phases_never_exceed_wall_clock() {
        let m = ScanMetrics::new();
        {
            let _t = m.timer(Phase::Walk);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        {
            let _t = m.timer(Phase::DbWrite);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let s = m.snapshot();
        assert!(
            s.total_phase_ms() <= s.wall_ms,
            "phases {} exceeded wall {}",
            s.total_phase_ms(),
            s.wall_ms
        );
    }

    #[test]
    fn metrics_are_shareable_across_threads() {
        // Guards the property #23 depends on: this must compile and stay true.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ScanMetrics>();
    }

    #[test]
    fn overlap_ratio_is_phases_over_wall_and_survives_a_zero_wall() {
        let s = MetricsSnapshot {
            wall_ms: 200,
            walk_ms: 50,
            hash_ms: 150,
            ..Default::default()
        };
        assert!((s.overlap_ratio() - 1.0).abs() < 1e-9);

        // A scan too short to register a millisecond must report 0, not NaN or infinity.
        let z = MetricsSnapshot {
            wall_ms: 0,
            hash_ms: 5,
            ..Default::default()
        };
        assert_eq!(z.overlap_ratio(), 0.0);
        assert!(z.report().contains("accounted"));
    }

    #[test]
    fn report_names_every_phase_and_is_divide_by_zero_safe() {
        let s = MetricsSnapshot::default();
        let out = s.report();
        for name in ["walk", "skip_check", "hash", "db_write", "archive"] {
            assert!(out.contains(name), "report missing {name}: {out}");
        }
    }
}
