//! Lightweight runtime instrumentation for the collector pipeline.
//!
//! These counters give the user (and us when triaging bug reports) a
//! window into what the background tasks are actually doing — how many
//! token-refresh passes have run, how often they hit the cache vs.
//! re-parsed, how busy fs_watch has been. The store itself is a bag of
//! atomics so instrumentation callers don't have to acquire any lock.
//!
//! All counters are best-effort. We don't fail the user-facing work if a
//! counter increment racing past `u64::MAX` would somehow matter — they're
//! diagnostic, not load-bearing.

use std::sync::atomic::{AtomicU64, Ordering};

/// Single-instance store. Cheap to clone via `Arc`.
#[derive(Debug, Default)]
pub struct DiagnosticsStore {
    /// Number of full token-refresh passes (`refresh_all`) that completed.
    /// Increments once per pass regardless of how many sessions were touched.
    token_refresh_passes: AtomicU64,
    /// Sum of elapsed-millis across all completed token-refresh passes.
    /// Divide by `token_refresh_passes` for average pass time.
    token_refresh_total_ms: AtomicU64,
    /// Number of sessions whose tokens / message_count actually changed in
    /// `write_back` (i.e. accepted updates, not regressions or no-ops).
    token_refresh_writes: AtomicU64,
    /// Number of `parse_meta_full` invocations served entirely from the
    /// `(path, mtime)` cache. High ratio = good; low ratio = cache thrash
    /// (often a sign of mtime instability or aggressive scan churn).
    token_cache_hits: AtomicU64,
    /// Number of `parse_meta_full` calls that fell through the cache and
    /// actually walked the file.
    token_cache_misses: AtomicU64,
    /// Total notify (Create/Modify/Remove) events seen by fs_watch since
    /// startup. Bursts up to thousands per second on heavy save activity.
    fs_watch_events: AtomicU64,
    /// Number of paths that survived the debounce window and were handed to
    /// `update_for_path`. Compared against `fs_watch_events` this gives a
    /// debounce-effectiveness ratio (paths_processed / events).
    fs_watch_paths_processed: AtomicU64,
    /// Reconcile (10 s) ticks completed. Mostly useful for confirming the
    /// background loop is alive when token totals look frozen.
    fs_watch_reconciles: AtomicU64,
}

impl DiagnosticsStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_token_refresh_pass(&self, elapsed_ms: u64, writes: usize) {
        self.token_refresh_passes.fetch_add(1, Ordering::Relaxed);
        self.token_refresh_total_ms
            .fetch_add(elapsed_ms, Ordering::Relaxed);
        self.token_refresh_writes
            .fetch_add(writes as u64, Ordering::Relaxed);
    }

    pub fn record_cache_hit(&self) {
        self.token_cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_cache_miss(&self) {
        self.token_cache_misses.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_fs_event(&self) {
        self.fs_watch_events.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_fs_path_processed(&self) {
        self.fs_watch_paths_processed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_fs_reconcile(&self) {
        self.fs_watch_reconciles.fetch_add(1, Ordering::Relaxed);
    }

    /// Snapshot all counters into a plain struct. Use this when assembling
    /// the Settings panel's diagnostics view — never touch the atomics
    /// directly from the renderer.
    pub fn snapshot(&self) -> DiagnosticsSnapshot {
        DiagnosticsSnapshot {
            token_refresh_passes: self.token_refresh_passes.load(Ordering::Relaxed),
            token_refresh_total_ms: self.token_refresh_total_ms.load(Ordering::Relaxed),
            token_refresh_writes: self.token_refresh_writes.load(Ordering::Relaxed),
            token_cache_hits: self.token_cache_hits.load(Ordering::Relaxed),
            token_cache_misses: self.token_cache_misses.load(Ordering::Relaxed),
            fs_watch_events: self.fs_watch_events.load(Ordering::Relaxed),
            fs_watch_paths_processed: self.fs_watch_paths_processed.load(Ordering::Relaxed),
            fs_watch_reconciles: self.fs_watch_reconciles.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DiagnosticsSnapshot {
    pub token_refresh_passes: u64,
    pub token_refresh_total_ms: u64,
    pub token_refresh_writes: u64,
    pub token_cache_hits: u64,
    pub token_cache_misses: u64,
    pub fs_watch_events: u64,
    pub fs_watch_paths_processed: u64,
    pub fs_watch_reconciles: u64,
}

impl DiagnosticsSnapshot {
    /// Average ms per token-refresh pass; 0 if no passes have completed.
    pub fn token_refresh_avg_ms(&self) -> f64 {
        if self.token_refresh_passes == 0 {
            0.0
        } else {
            self.token_refresh_total_ms as f64 / self.token_refresh_passes as f64
        }
    }

    /// Cache hit rate as a fraction in [0, 1]; 0 if no lookups have been
    /// recorded.
    pub fn token_cache_hit_rate(&self) -> f64 {
        let total = self.token_cache_hits + self.token_cache_misses;
        if total == 0 {
            0.0
        } else {
            self.token_cache_hits as f64 / total as f64
        }
    }

    /// Fraction of fs_watch events that survived debounce; 0 if none.
    /// Useful for confirming the path-aware debounce is collapsing bursts
    /// (a heavy editor save typically sits at 10-30%).
    pub fn fs_watch_debounce_ratio(&self) -> f64 {
        if self.fs_watch_events == 0 {
            0.0
        } else {
            self.fs_watch_paths_processed as f64 / self.fs_watch_events as f64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_snapshot_round_trip() {
        let s = DiagnosticsStore::new();
        s.record_token_refresh_pass(12, 5);
        s.record_token_refresh_pass(8, 3);
        s.record_cache_hit();
        s.record_cache_hit();
        s.record_cache_miss();
        s.record_fs_event();
        s.record_fs_event();
        s.record_fs_event();
        s.record_fs_path_processed();
        s.record_fs_reconcile();

        let snap = s.snapshot();
        assert_eq!(snap.token_refresh_passes, 2);
        assert_eq!(snap.token_refresh_total_ms, 20);
        assert_eq!(snap.token_refresh_writes, 8);
        assert_eq!(snap.token_cache_hits, 2);
        assert_eq!(snap.token_cache_misses, 1);
        assert_eq!(snap.fs_watch_events, 3);
        assert_eq!(snap.fs_watch_paths_processed, 1);
        assert_eq!(snap.fs_watch_reconciles, 1);
    }

    #[test]
    fn averages_handle_empty_division() {
        let s = DiagnosticsStore::new();
        let snap = s.snapshot();
        assert_eq!(snap.token_refresh_avg_ms(), 0.0);
        assert_eq!(snap.token_cache_hit_rate(), 0.0);
        assert_eq!(snap.fs_watch_debounce_ratio(), 0.0);
    }

    #[test]
    fn averages_compute_correctly() {
        let s = DiagnosticsStore::new();
        s.record_token_refresh_pass(20, 0);
        s.record_token_refresh_pass(40, 0);
        s.record_cache_hit();
        s.record_cache_hit();
        s.record_cache_hit();
        s.record_cache_miss();
        s.record_fs_event();
        s.record_fs_event();
        s.record_fs_event();
        s.record_fs_event();
        s.record_fs_path_processed();
        let snap = s.snapshot();
        assert!((snap.token_refresh_avg_ms() - 30.0).abs() < 1e-9);
        assert!((snap.token_cache_hit_rate() - 0.75).abs() < 1e-9);
        assert!((snap.fs_watch_debounce_ratio() - 0.25).abs() < 1e-9);
    }
}
