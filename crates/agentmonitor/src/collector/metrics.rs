use std::collections::{HashMap, VecDeque};

use parking_lot::RwLock;

#[derive(Debug, Clone, Copy)]
pub struct ProcessSample {
    pub unix_ts: u64,
    pub rss_kb: u64,
    pub cpu: f32,
}

#[derive(Debug, Clone)]
pub struct ProcessEntry {
    pub agent: &'static str,
    pub pid: u32,
    pub name: String,
    pub cmd: String,
    pub cwd: Option<String>,
    pub started_unix: u64,
    pub samples: VecDeque<ProcessSample>,
}

impl ProcessEntry {
    pub fn latest_rss_kb(&self) -> u64 {
        self.samples.back().map(|s| s.rss_kb).unwrap_or(0)
    }
    pub fn latest_cpu(&self) -> f32 {
        self.samples.back().map(|s| s.cpu).unwrap_or(0.0)
    }
    pub fn rss_history(&self) -> Vec<u64> {
        self.samples.iter().map(|s| s.rss_kb).collect()
    }
}

/// Lock-free-ish metrics store: a single parking_lot RwLock wraps a HashMap.
/// Writers touch it once per sample interval; readers read on each frame.
#[derive(Debug)]
pub struct MetricsStore {
    inner: RwLock<HashMap<u32, ProcessEntry>>,
    capacity: usize,
}

impl MetricsStore {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
            capacity,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn upsert(
        &self,
        pid: u32,
        agent: &'static str,
        name: String,
        cmd: String,
        cwd: Option<String>,
        started_unix: u64,
        sample: ProcessSample,
    ) {
        let mut map = self.inner.write();
        let entry = map.entry(pid).or_insert_with(|| ProcessEntry {
            agent,
            pid,
            name: name.clone(),
            cmd: cmd.clone(),
            cwd: cwd.clone(),
            started_unix,
            samples: VecDeque::with_capacity(self.capacity),
        });
        entry.name = name;
        entry.cmd = cmd;
        entry.cwd = cwd;
        entry.samples.push_back(sample);
        while entry.samples.len() > self.capacity {
            entry.samples.pop_front();
        }
    }

    pub fn retain_alive(&self, alive: &std::collections::HashSet<u32>) {
        let mut map = self.inner.write();
        map.retain(|pid, _| alive.contains(pid));
    }

    pub fn snapshot(&self) -> Vec<ProcessEntry> {
        let map = self.inner.read();
        let mut v: Vec<_> = map.values().cloned().collect();
        v.sort_by_key(|b| std::cmp::Reverse(b.latest_rss_kb()));
        v
    }

    pub fn total_rss_kb(&self) -> u64 {
        self.inner.read().values().map(|e| e.latest_rss_kb()).sum()
    }
}
