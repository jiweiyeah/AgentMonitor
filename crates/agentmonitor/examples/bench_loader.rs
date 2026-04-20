use agentmonitor::adapter::{adapter_for_path, ClaudeAdapter, CodexAdapter, DynAdapter};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let adapters: Vec<DynAdapter> = vec![
        Arc::new(ClaudeAdapter::new(Some(PathBuf::from(
            "/Users/yjw/.claude/projects",
        )))),
        Arc::new(CodexAdapter::new(Some(PathBuf::from(
            "/Users/yjw/.codex/sessions",
        )))),
    ];
    let args: Vec<String> = std::env::args().collect();
    let path = PathBuf::from(args.get(1).cloned().unwrap_or_else(|| {
        "/Users/yjw/.claude/projects/-Users-yjw-code-projects-AgentMonitor/383e09fe-ff1d-44d5-aeef-78f616f3a621.jsonl".into()
    }));
    let adapter = adapter_for_path(&adapters, &path)
        .expect("no adapter")
        .clone();
    let size = std::fs::metadata(&path)?.len();
    let t0 = Instant::now();
    let events = adapter.load_conversation(&path).await?;
    let elapsed = t0.elapsed();
    let total_blocks: usize = events.iter().map(|e| e.blocks.len()).sum();
    println!("file  {}", path.display());
    println!("size  {:.2} MB", size as f64 / 1024.0 / 1024.0);
    println!(
        "events {}  blocks {}  elapsed {:.2}ms",
        events.len(),
        total_blocks,
        elapsed.as_secs_f64() * 1000.0
    );
    Ok(())
}
