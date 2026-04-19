use std::io::{self, Stdout};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use agentmonitor::app::App;
use agentmonitor::config::Config;
use agentmonitor::event::{run_event_loop, EventLoopOptions};

#[derive(Parser, Debug)]
#[command(
    name = "agent-monitor",
    version,
    about = "TUI monitor for Claude Code / Codex sessions and processes"
)]
struct Cli {
    /// Print sessions once to stdout and exit (used for cold-start benchmarks).
    #[arg(long)]
    once_and_exit: bool,

    /// Override the process sampling interval (seconds).
    #[arg(long, default_value_t = 2)]
    sample_interval: u64,

    /// Enable verbose tracing to `$XDG_CACHE_HOME/agent-monitor.log`.
    #[arg(long)]
    debug: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.debug)?;

    let config = Config {
        sample_interval: Duration::from_secs(cli.sample_interval.max(1)),
        ..Config::default()
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .context("failed to build tokio runtime")?;

    runtime.block_on(async move {
        let app = App::new(config).await?;
        if cli.once_and_exit {
            app.print_snapshot();
            return Ok::<_, anyhow::Error>(());
        }
        run_tui(app).await
    })
}

async fn run_tui(app: App) -> Result<()> {
    let mut terminal = setup_terminal().context("failed to enter raw mode")?;
    let options = EventLoopOptions::default();
    let result = run_event_loop(&mut terminal, app, options).await;
    restore_terminal(&mut terminal).ok();
    result
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

fn init_tracing(debug: bool) -> Result<()> {
    if !debug {
        return Ok(());
    }
    let dirs = directories::ProjectDirs::from("dev", "agentmonitor", "agent-monitor")
        .context("failed to resolve cache dir")?;
    std::fs::create_dir_all(dirs.cache_dir()).ok();
    let log_path = dirs.cache_dir().join("agent-monitor.log");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    tracing_subscriber::fmt()
        .with_writer(file)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_ansi(false)
        .init();
    Ok(())
}
