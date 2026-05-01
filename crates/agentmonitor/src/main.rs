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

    /// Override the Claude Code projects directory. Default:
    /// `~/.claude/projects`. Use this when your sessions live under a custom
    /// dotfiles path or behind a symlink that the default detection misses.
    #[arg(long, value_name = "PATH")]
    claude_root: Option<std::path::PathBuf>,

    /// Override the Codex sessions directory. Default: `~/.codex/sessions`.
    #[arg(long, value_name = "PATH")]
    codex_root: Option<std::path::PathBuf>,

    /// Override the Gemini CLI tmp directory. Default: `~/.gemini/tmp`.
    #[arg(long, value_name = "PATH")]
    gemini_root: Option<std::path::PathBuf>,

    /// Override the Hermes Agent state directory. Default: `~/.hermes`.
    #[arg(long, value_name = "PATH")]
    hermes_root: Option<std::path::PathBuf>,

    /// Override the OpenCode share directory. Default:
    /// `~/.local/share/opencode`.
    #[arg(long, value_name = "PATH")]
    opencode_root: Option<std::path::PathBuf>,

    /// Override the Claude Desktop local-agent-mode-sessions directory.
    /// Default (macOS):
    /// `~/Library/Application Support/Claude-3p/local-agent-mode-sessions`.
    #[arg(long, value_name = "PATH")]
    claude_desktop_root: Option<std::path::PathBuf>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.debug)?;
    install_panic_hook();

    // Persisted preferences win unless the user overrode on the CLI. The
    // default clap value (`2`) would otherwise always clobber a saved choice.
    let configured_interval = agentmonitor::settings::get().sample_interval.0;
    let sample_secs = if cli.sample_interval == 2 && configured_interval != 2 {
        configured_interval
    } else {
        cli.sample_interval
    };

    let mut config = Config {
        sample_interval: Duration::from_secs(sample_secs.max(1)),
        ..Config::default()
    };
    // CLI flags override the auto-detected defaults. Each is independent —
    // a user with custom Claude root but default Codex root works fine.
    if let Some(path) = cli.claude_root {
        config.claude_root = Some(path);
    }
    if let Some(path) = cli.codex_root {
        config.codex_root = Some(path);
    }
    if let Some(path) = cli.gemini_root {
        config.gemini_root = Some(path);
    }
    if let Some(path) = cli.hermes_root {
        config.hermes_root = Some(path);
    }
    if let Some(path) = cli.opencode_root {
        config.opencode_root = Some(path);
    }
    if let Some(path) = cli.claude_desktop_root {
        config.claude_desktop_root = Some(path);
    }

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

/// Install a panic hook that restores the terminal to a sane state before
/// printing the panic. Without this, a panic in raw-mode/alt-screen leaves
/// the user's terminal in raw mode (no echo, no line buffering) and the
/// panic info hidden behind the alternate screen — they have to blindly
/// type `reset` to recover.
///
/// The hook chains to the original (default) handler at the end so the
/// panic still aborts the process and `RUST_BACKTRACE=1` still works.
fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Best-effort terminal restoration — failures during panic are logged
        // but not propagated. Order mirrors `restore_terminal`.
        let _ = disable_raw_mode();
        let _ = execute!(io::stderr(), LeaveAlternateScreen, DisableMouseCapture);
        // Mirror to log file (no-op if --debug wasn't passed and there's no
        // active subscriber — `tracing::error!` becomes a cheap macro then).
        tracing::error!(panic = %info, "agent-monitor panicked");
        // Run the default handler last so the standard panic message + any
        // backtrace is printed AFTER the terminal is restored. Otherwise the
        // alternate screen would swallow the message before exit.
        original(info);
    }));
}
