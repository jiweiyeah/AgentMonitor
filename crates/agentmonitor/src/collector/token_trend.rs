//! Tracks the cumulative token total across all sessions over time so the
//! Dashboard can show a "tokens burned per minute" sparkline alongside RSS.
//!
//! Records are produced by `collector::token_refresh::write_back` after every
//! accepted update — only successful, monotone-nondecreasing writes feed in,
//! so the trend never goes backward. Each entry stores the *sum* of all
//! sessions' tokens at that moment; the consumer differences adjacent buckets
//! to get rate-of-change.
//!
//! Capacity is bounded so the sample VecDeque can't grow without limit:
//! we keep the most recent ~1 hour's samples (configurable). Older samples
//! are dropped on insert.

use std::collections::VecDeque;
use std::time::{Duration, SystemTime};

use parking_lot::RwLock;

#[derive(Debug)]
pub struct TokenTrend {
    inner: RwLock<Inner>,
}

#[derive(Debug)]
struct Inner {
    samples: VecDeque<Sample>,
    capacity: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct Sample {
    pub at: SystemTime,
    /// Cumulative total across all sessions at this moment.
    pub total_tokens: u64,
}

impl TokenTrend {
    /// Defaults to ~1 hour of room at 10 samples / minute.
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: RwLock::new(Inner {
                samples: VecDeque::with_capacity(capacity),
                capacity,
            }),
        }
    }

    /// Append a new sample. The argument is a *cumulative* total; we never
    /// regress, so a smaller-than-last value is silently dropped (treats
    /// stale or partial reads the same way `token_refresh::write_back`
    /// does — see CLAUDE.md §2).
    pub fn record(&self, at: SystemTime, total_tokens: u64) {
        let mut inner = self.inner.write();
        if let Some(last) = inner.samples.back() {
            if total_tokens < last.total_tokens {
                return;
            }
            // De-dup: if nothing changed, no need to clutter the queue.
            if total_tokens == last.total_tokens {
                return;
            }
        }
        if inner.samples.len() == inner.capacity && inner.capacity > 0 {
            inner.samples.pop_front();
        }
        inner.samples.push_back(Sample { at, total_tokens });
    }

    /// Return per-bucket *deltas* over the last `n_buckets * bucket` window
    /// ending at `now`. Each entry is the number of tokens added during that
    /// time slot (i.e. the difference between the latest sample in this
    /// bucket and the latest sample at-or-before this bucket's start).
    ///
    /// Empty buckets receive 0; this lets the Dashboard sparkline render
    /// idle periods as flat without special-casing on the renderer side.
    pub fn buckets(&self, now: SystemTime, bucket: Duration, n_buckets: usize) -> Vec<u64> {
        if n_buckets == 0 || bucket.is_zero() {
            return Vec::new();
        }
        let inner = self.inner.read();
        if inner.samples.is_empty() {
            return vec![0; n_buckets];
        }
        let mut out = vec![0u64; n_buckets];
        // Latest known total at-or-before each bucket boundary.
        let mut prev_total = inner
            .samples
            .iter()
            .find(|s| s.at <= now.checked_sub(bucket * n_buckets as u32).unwrap_or(now))
            .map(|s| s.total_tokens)
            .unwrap_or(inner.samples.front().map(|s| s.total_tokens).unwrap_or(0));
        for (i, slot) in out.iter_mut().enumerate() {
            let bucket_end = now
                .checked_sub(bucket * (n_buckets - 1 - i) as u32)
                .unwrap_or(now);
            // Find the latest sample whose timestamp is <= bucket_end.
            let latest_in_or_before: Option<u64> = inner
                .samples
                .iter()
                .rev()
                .find(|s| s.at <= bucket_end)
                .map(|s| s.total_tokens);
            if let Some(t) = latest_in_or_before {
                *slot = t.saturating_sub(prev_total);
                prev_total = t;
            } else {
                *slot = 0;
            }
        }
        out
    }

    /// Convenience: total tokens added in the last `window`.
    pub fn rate_in_window(&self, now: SystemTime, window: Duration) -> u64 {
        let inner = self.inner.read();
        if inner.samples.is_empty() {
            return 0;
        }
        let cutoff = now.checked_sub(window).unwrap_or(now);
        let earliest = inner
            .samples
            .iter()
            .find(|s| s.at >= cutoff)
            .map(|s| s.total_tokens)
            .or_else(|| inner.samples.front().map(|s| s.total_tokens))
            .unwrap_or(0);
        let latest = inner
            .samples
            .back()
            .map(|s| s.total_tokens)
            .unwrap_or(earliest);
        latest.saturating_sub(earliest)
    }

    pub fn len(&self) -> usize {
        self.inner.read().samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for TokenTrend {
    fn default() -> Self {
        // 1 hour at ~10 samples/min — generous enough that idle minutes still
        // appear on the sparkline.
        Self::new(720)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn ts(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn record_rejects_regression_and_dedups() {
        let t = TokenTrend::new(10);
        t.record(ts(0), 100);
        t.record(ts(10), 100); // dedup
        t.record(ts(20), 50); // regression — dropped
        t.record(ts(30), 200);
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn record_evicts_oldest_when_at_capacity() {
        let t = TokenTrend::new(3);
        for i in 1..=5u64 {
            t.record(ts(i * 10), i * 100);
        }
        assert_eq!(t.len(), 3);
        // The first two samples (100, 200) should have been evicted.
        let inner = t.inner.read();
        let totals: Vec<u64> = inner.samples.iter().map(|s| s.total_tokens).collect();
        assert_eq!(totals, vec![300, 400, 500]);
    }

    #[test]
    fn buckets_compute_per_minute_deltas() {
        let t = TokenTrend::new(100);
        // Three increments at +0, +30, +60 seconds: 100, 250, 300.
        t.record(ts(0), 100);
        t.record(ts(30), 250); // +150 in second 30s window
        t.record(ts(60), 300); // +50 more
                               // Two 30s buckets ending at t=60 should show [150, 50].
        let deltas = t.buckets(ts(60), Duration::from_secs(30), 2);
        assert_eq!(deltas, vec![150, 50]);
    }

    #[test]
    fn buckets_return_zeros_for_idle_window() {
        let t = TokenTrend::new(10);
        // No samples at all: every bucket is 0.
        assert_eq!(
            t.buckets(ts(100), Duration::from_secs(10), 3),
            vec![0, 0, 0]
        );
    }

    #[test]
    fn rate_in_window_sums_recent_growth() {
        let t = TokenTrend::new(10);
        t.record(ts(0), 100);
        t.record(ts(60), 150);
        t.record(ts(120), 250);
        // Last 90 seconds → growth from t=30 (no sample, so use earliest >=30
        // which is t=60 with total 150) to latest 250 → 100.
        assert_eq!(t.rate_in_window(ts(120), Duration::from_secs(90)), 100);
    }
}
