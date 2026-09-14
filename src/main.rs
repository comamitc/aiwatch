use std::{path::PathBuf, time::Duration};

use anyhow::{Result, bail};
use clap::Parser;
use crossterm::terminal;
use limitwatch::{
    config::AppConfig,
    demo,
    model::{DashboardSnapshot, Provider},
    output,
    poller::{PollCoordinator, loading_snapshot},
    ui,
};
use tokio::sync::{mpsc, watch};

#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Print one static snapshot and exit.
    #[arg(long)]
    once: bool,

    /// Print one JSON snapshot and exit.
    #[arg(long)]
    json: bool,

    /// Render synthetic, secret-free demonstration data.
    #[arg(long)]
    demo: bool,

    /// Read configuration from this path.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Include only these providers (comma-separated).
    #[arg(long, value_delimiter = ',')]
    provider: Vec<Provider>,

    /// Provider poll interval in seconds. Minimum: 60.
    #[arg(long)]
    interval: Option<u64>,

    /// Disable local SQLite history and sparklines.
    #[arg(long)]
    no_history: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.demo {
        let mut snapshot = demo::snapshot();
        if !cli.provider.is_empty() {
            snapshot
                .accounts
                .retain(|account| cli.provider.contains(&account.provider));
        }
        return present_demo(snapshot, &cli).await;
    }

    let mut config = AppConfig::load(cli.config.as_deref(), cli.interval)?;
    config.retain_providers(&cli.provider);
    if config.accounts.is_empty() {
        bail!(
            "no provider credentials discovered; log in with claude, codex, or grok, or configure account credential paths in ~/.config/limitwatch/config.toml"
        );
    }

    let history_path = (!cli.no_history).then_some(config.history_path.as_path());
    let mut coordinator = PollCoordinator::new(config.accounts.clone(), history_path)?;
    if cli.once || cli.json {
        let snapshot = coordinator.poll_once().await;
        if cli.json {
            println!("{}", serde_json::to_string_pretty(&snapshot)?);
        } else {
            let width = terminal::size().map_or(100, |(width, _)| width as usize);
            print!("{}", output::render_text(&snapshot, width));
        }
        return Ok(());
    }

    let initial = loading_snapshot(&config.accounts);
    let (snapshot_tx, snapshot_rx) = watch::channel(initial);
    let (refresh_tx, refresh_rx) = mpsc::channel(1);
    let poll_interval = Duration::from_secs(config.poll_interval_secs);
    let poller = tokio::spawn(coordinator.run(snapshot_tx, refresh_rx, poll_interval));
    let result = ui::run(snapshot_rx, refresh_tx, poll_interval).await;
    poller.abort();
    result
}

async fn present_demo(snapshot: DashboardSnapshot, cli: &Cli) -> Result<()> {
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&snapshot)?);
        return Ok(());
    }
    if cli.once {
        let width = terminal::size().map_or(100, |(width, _)| width as usize);
        print!("{}", output::render_text(&snapshot, width));
        return Ok(());
    }

    let poll_interval = Duration::from_secs(cli.interval.unwrap_or(60).max(60));
    let (_snapshot_guard, snapshot_rx) = watch::channel(snapshot);
    let (refresh_tx, _refresh_guard) = mpsc::channel(1);
    ui::run(snapshot_rx, refresh_tx, poll_interval).await
}
