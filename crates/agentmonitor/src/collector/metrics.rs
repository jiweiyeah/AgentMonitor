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
    /// macOS "responsible PID" — the GUI app or terminal that originated this
    /// process, even after the immediate parent has exited and PPID has been
    /// reset to 1 (launchd). `None` on non-macOS or when the kernel refused
    /// the lookup (rare; SIP-protected processes mostly).
    pub responsible_pid: Option<u32>,
    /// Short executable name of the responsible process (e.g. `ghostty`,
    /// `Code`, `iTerm2`). Empty string is normalized to `None` so callers can
    /// uniformly fall back to "-".
    pub responsible_name: Option<String>,
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
        responsible: Option<crate::collector::responsible::Responsible>,
        sample: ProcessSample,
    ) {
        // Pre-extract before we take the write lock so the borrow checker
        // doesn't have to reason about the moved `responsible` inside the
        // closure that or_insert_with passes through.
        let (resp_pid, resp_name) = match &responsible {
            Some(r) => (
                Some(r.pid),
                if r.name.is_empty() {
                    None
                } else {
                    Some(r.name.clone())
                },
            ),
            None => (None, None),
        };

        let mut map = self.inner.write();
        let entry = map.entry(pid).or_insert_with(|| ProcessEntry {
            agent,
            pid,
            name: name.clone(),
            cmd: cmd.clone(),
            cwd: cwd.clone(),
            started_unix,
            samples: VecDeque::with_capacity(self.capacity),
            responsible_pid: resp_pid,
            responsible_name: resp_name.clone(),
        });
        entry.name = name;
        entry.cmd = cmd;
        entry.cwd = cwd;
        // Responsible PID is invariant for a process's lifetime, but we
        // refresh it anyway so a kernel that *did* refuse the first call
        // (and returned None) can be picked up on a later tick once the
        // process has stabilized. Cheap — single syscall caller-side.
        if resp_pid.is_some() {
            entry.responsible_pid = resp_pid;
            entry.responsible_name = resp_name;
        }
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
