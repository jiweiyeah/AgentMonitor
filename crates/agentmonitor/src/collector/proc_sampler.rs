use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};
use tokio::time::interval;

use crate::adapter::DynAdapter;
use crate::collector::metrics::{MetricsStore, ProcessSample};

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
    let mut system = System::new_with_specifics(
        RefreshKind::new().with_processes(refresh_kind),
    );

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

        let mut alive = HashSet::new();
        for (pid, proc) in system.processes() {
            let cmd: Vec<String> = proc.cmd().iter().map(|s| s.to_string_lossy().into()).collect();
            let exe = proc.exe();
            let agent = adapters
                .iter()
                .find(|a| a.matches_process(&cmd, exe))
                .map(|a| a.id());
            let Some(agent_id) = agent else {
                continue;
            };
            let pid_u32 = pid.as_u32();
            let rss_kb = proc.memory() / 1024;
            let cpu = proc.cpu_usage();
            let name = proc.name().to_string_lossy().to_string();
            let cmd_str = cmd.join(" ");
            let cwd = proc.cwd().map(|p| p.display().to_string());
            let started_unix = proc.start_time();
            let sample = ProcessSample {
                unix_ts: now,
                rss_kb,
                cpu,
            };
            store.upsert(pid_u32, agent_id, name, cmd_str, cwd, started_unix, sample);
            alive.insert(pid_u32);
        }
        store.retain_alive(&alive);
        dirty.notify_one();
    }
}
