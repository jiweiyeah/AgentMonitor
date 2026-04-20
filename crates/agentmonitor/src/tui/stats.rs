//! Pure aggregations over sessions and metrics. Kept free of ratatui types so
//! the dashboard/process renderers only deal with ready-to-paint data.

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use crate::adapter::types::{SessionMeta, TokenStats};
use crate::collector::metrics::MetricsStore;

/// One entry per project bucket in `top_projects`.
#[derive(Debug, Clone)]
pub struct ProjectRow {
    pub cwd: String,
    pub count: usize,
    pub latest: Option<DateTime<Utc>>,
}

/// Per-agent token rollup used by the dashboard's tokens strip.
#[derive(Debug, Clone)]
pub struct AgentTokenRow {
    pub agent: &'static str,
    pub sessions: usize,
    pub tokens: TokenStats,
}

/// Count sessions whose `updated_at` falls into each of the last `n_buckets`
/// hour-sized buckets ending at `now`. Index 0 is the oldest bucket, last
/// index is the most recent.
pub fn activity_buckets(
    sessions: &[SessionMeta],
    now: DateTime<Utc>,
    n_buckets: usize,
) -> Vec<u64> {
    let mut buckets = vec![0u64; n_buckets];
    if n_buckets == 0 {
        return buckets;
    }
    for s in sessions {
        let Some(ts) = s.updated_at else { continue };
        let delta = now.signed_duration_since(ts).num_seconds();
        if delta < 0 {
            // Session timestamp in the future (clock skew): count as "now".
            buckets[n_buckets - 1] += 1;
            continue;
        }
        let hours_ago = (delta / 3600) as usize;
        if hours_ago >= n_buckets {
            continue;
        }
        let idx = n_buckets - 1 - hours_ago;
        buckets[idx] += 1;
    }
    buckets
}

/// Rank distinct `cwd`s by session count, breaking ties by recency. Sessions
/// without a `cwd` are skipped so the list stays actionable.
pub fn top_projects(sessions: &[SessionMeta], n: usize) -> Vec<ProjectRow> {
    if n == 0 {
        return Vec::new();
    }
    let mut acc: HashMap<String, ProjectRow> = HashMap::new();
    for s in sessions {
        let Some(cwd) = &s.cwd else { continue };
        let key = cwd.display().to_string();
        let row = acc.entry(key.clone()).or_insert(ProjectRow {
            cwd: key,
            count: 0,
            latest: None,
        });
        row.count += 1;
        row.latest = match (row.latest, s.updated_at) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (None, b) => b,
            (a, None) => a,
        };
    }
    let mut rows: Vec<ProjectRow> = acc.into_values().collect();
    rows.sort_by(|a, b| b.count.cmp(&a.count).then(b.latest.cmp(&a.latest)));
    rows.truncate(n);
    rows
}

/// Sum token stats per agent id. Preserves the order given by `agents` so the
/// UI always lists them in the same order, even when some have zero sessions.
pub fn tokens_by_agent(sessions: &[SessionMeta], agents: &[&'static str]) -> Vec<AgentTokenRow> {
    let mut rows: Vec<AgentTokenRow> = agents
        .iter()
        .map(|id| AgentTokenRow {
            agent: id,
            sessions: 0,
            tokens: TokenStats::default(),
        })
        .collect();
    for s in sessions {
        let Some(row) = rows.iter_mut().find(|r| r.agent == s.agent) else {
            continue;
        };
        row.sessions += 1;
        row.tokens.input += s.tokens.input;
        row.tokens.output += s.tokens.output;
        row.tokens.cache_read += s.tokens.cache_read;
        row.tokens.cache_creation += s.tokens.cache_creation;
    }
    rows
}

/// Sum of per-PID latest RSS in each of the last `n_buckets` time buckets.
///
/// For each bucket we take each PID's *latest* sample that fell inside it and
/// sum those across PIDs — that's total live-memory at time T rather than
/// sample volume. PIDs with no sample in a bucket are simply absent from the
/// sum, so the series ramps up naturally as processes appear.
pub fn aggregate_rss_buckets(
    store: &MetricsStore,
    now_unix: u64,
    bucket_secs: u64,
    n_buckets: usize,
) -> Vec<u64> {
    if n_buckets == 0 || bucket_secs == 0 {
        return Vec::new();
    }
    let newest_bucket = now_unix / bucket_secs;
    let oldest_bucket = newest_bucket.saturating_sub(n_buckets as u64 - 1);

    // per_bucket[pid] -> rss_kb of the latest sample in that bucket.
    let mut per_bucket: Vec<HashMap<u32, u64>> =
        (0..n_buckets).map(|_| HashMap::new()).collect();

    for entry in store.snapshot() {
        for sample in entry.samples.iter() {
            let bucket = sample.unix_ts / bucket_secs;
            if bucket < oldest_bucket || bucket > newest_bucket {
                continue;
            }
            let idx = (bucket - oldest_bucket) as usize;
            per_bucket[idx].insert(entry.pid, sample.rss_kb);
        }
    }
    per_bucket
        .into_iter()
        .map(|m| m.values().sum())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use chrono::TimeZone;

    fn session(agent: &'static str, cwd: Option<&str>, updated: Option<DateTime<Utc>>) -> SessionMeta {
        SessionMeta {
            agent,
            id: "x".into(),
            path: PathBuf::from("/tmp/x.jsonl"),
            cwd: cwd.map(PathBuf::from),
            model: None,
            version: None,
            git_branch: None,
            started_at: None,
            updated_at: updated,
            message_count: 0,
            tokens: TokenStats::default(),
            status: Default::default(),
            byte_offset: 0,
            size_bytes: 0,
        }
    }

    #[test]
    fn activity_buckets_places_sessions_in_correct_hour() {
        let now = Utc.with_ymd_and_hms(2026, 4, 20, 12, 0, 0).unwrap();
        let sessions = vec![
            session("claude", None, Some(now)),
            session("claude", None, Some(now - chrono::Duration::minutes(30))),
            session("claude", None, Some(now - chrono::Duration::hours(5))),
            session("claude", None, Some(now - chrono::Duration::hours(25))), // out of range
        ];
        let b = activity_buckets(&sessions, now, 24);
        assert_eq!(b.len(), 24);
        assert_eq!(b[23], 2, "last bucket holds samples from the last hour");
        assert_eq!(b[23 - 5], 1);
        assert_eq!(b.iter().sum::<u64>(), 3, "25h-old sample dropped");
    }

    #[test]
    fn activity_buckets_handles_future_timestamps() {
        let now = Utc.with_ymd_and_hms(2026, 4, 20, 12, 0, 0).unwrap();
        let s = vec![session("claude", None, Some(now + chrono::Duration::minutes(5)))];
        let b = activity_buckets(&s, now, 24);
        assert_eq!(b[23], 1, "future-skewed timestamps land in the current bucket");
    }

    #[test]
    fn top_projects_sorts_by_count_then_recency() {
        let t = Utc.with_ymd_and_hms(2026, 4, 20, 12, 0, 0).unwrap();
        let sessions = vec![
            session("claude", Some("/a"), Some(t)),
            session("claude", Some("/a"), Some(t - chrono::Duration::hours(1))),
            session("claude", Some("/b"), Some(t)),
            session("claude", None, Some(t)), // skipped: no cwd
        ];
        let rows = top_projects(&sessions, 5);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].cwd, "/a");
        assert_eq!(rows[0].count, 2);
        assert_eq!(rows[1].cwd, "/b");
    }

    #[test]
    fn tokens_by_agent_preserves_order_and_sums() {
        let t = Utc.with_ymd_and_hms(2026, 4, 20, 12, 0, 0).unwrap();
        let mut a = session("claude", None, Some(t));
        a.tokens.input = 100;
        a.tokens.output = 50;
        let mut b = session("codex", None, Some(t));
        b.tokens.input = 10;
        b.tokens.cache_read = 5;
        let rows = tokens_by_agent(&[a, b], &["claude", "codex"]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].agent, "claude");
        assert_eq!(rows[0].tokens.input, 100);
        assert_eq!(rows[1].agent, "codex");
        assert_eq!(rows[1].tokens.cache_read, 5);
    }
}
