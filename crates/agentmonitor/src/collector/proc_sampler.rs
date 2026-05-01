use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};
use tokio::time::interval;

use crate::adapter::DynAdapter;
use crate::collector::metrics::{MetricsStore, ProcessSample};

struct Candidate {
    agent_id: &'static str,
    ppid: Option<u32>,
    name: String,
    cmd: String,
    cwd: Option<String>,
    started_unix: u64,
    rss_kb: u64,
    cpu: f32,
}

/// Background sampler that refreshes agent-owned processes every `interval`.
pub async fn run(
    adapters: Vec<DynAdapter>,
    store: Arc<MetricsStore>,
    tick: Duration,
    dirty: Arc<tokio::sync::Notify>,
) {
    let refresh_kind = ProcessRefreshKind::new()
        .with_cpu()
        .with_memory()
        .with_cmd(sysinfo::UpdateKind::OnlyIfNotSet)
        .with_exe(sysinfo::UpdateKind::OnlyIfNotSet)
        .with_cwd(sysinfo::UpdateKind::OnlyIfNotSet);
    let mut system = System::new_with_specifics(RefreshKind::new().with_processes(refresh_kind));

    let mut ticker = interval(tick);
    // Discard the immediate first tick so startup doesn't double-sample.
    ticker.tick().await;

    loop {
        ticker.tick().await;
        system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind);

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let mut candidates: HashMap<u32, Candidate> = HashMap::new();
        for (pid, proc) in system.processes() {
            let cmd: Vec<String> = proc
                .cmd()
                .iter()
                .map(|s| s.to_string_lossy().into())
                .collect();
            let exe = proc.exe();
            let Some(agent_id) = adapters
                .iter()
                .find(|a| a.matches_process(&cmd, exe))
                .map(|a| a.id())
            else {
                continue;
            };
            let pid_u32 = pid.as_u32();
            candidates.insert(
                pid_u32,
                Candidate {
                    agent_id,
                    ppid: proc.parent().map(|p| p.as_u32()),
                    name: proc.name().to_string_lossy().to_string(),
                    cmd: cmd.join(" "),
                    cwd: proc.cwd().map(|p| p.display().to_string()),
                    started_unix: proc.start_time(),
                    rss_kb: proc.memory() / 1024,
                    cpu: proc.cpu_usage(),
                },
            );
        }

        // Suppress wrappers: if my parent is also claimed by the same adapter, it's
        // the launcher (e.g. the `codex` shell/node shim forking the real node),
        // and we only want the leaf so one session maps to one PID.
        let mut wrapper_pids: HashSet<u32> = HashSet::new();
        for cand in candidates.values() {
            if let Some(ppid) = cand.ppid {
                if let Some(parent) = candidates.get(&ppid) {
                    if parent.agent_id == cand.agent_id {
                        wrapper_pids.insert(ppid);
                    }
                }
            }
        }

        let mut alive = HashSet::new();
        for (pid, cand) in candidates {
            if wrapper_pids.contains(&pid) {
                continue;
            }
            // macOS-only: ask the kernel for the GUI app / terminal that
            // originated this PID. On Linux/Windows this returns None. The
            // call is one syscall + two libproc helpers — cheap enough to do
            // every tick instead of caching, and refreshing handles the case
            // where an early tick raced before the kernel had populated the
            // entry for a brand-new PID.
            let responsible = crate::collector::responsible::for_pid(pid);
            let sample = ProcessSample {
                unix_ts: now,
                rss_kb: cand.rss_kb,
                cpu: cand.cpu,
            };
            store.upsert(
                pid,
                cand.agent_id,
                cand.name,
                cand.cmd,
                cand.cwd,
                cand.started_unix,
                responsible,
                sample,
            );
            alive.insert(pid);
        }
        store.retain_alive(&alive);
        dirty.notify_one();
    }
}
