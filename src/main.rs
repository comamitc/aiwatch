use std::{ffi::OsString, path::PathBuf, time::Duration};

use aiwatch::{
    accounts::AccountManager,
    config::AppConfig,
    demo,
    model::{DashboardSnapshot, Provider},
    output,
    poller::{PollCoordinator, loading_snapshot},
    ui,
};
use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use crossterm::terminal;
use tokio::sync::{mpsc, watch};

#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

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

    /// Provider poll interval in seconds. Minimum: 300.
    #[arg(long)]
    interval: Option<u64>,

    /// Disable local SQLite history and sparklines.
    #[arg(long)]
    no_history: bool,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Create, authenticate, launch, and list isolated provider accounts.
    Account {
        #[command(subcommand)]
        command: AccountCommand,
    },
}

#[derive(Debug, Subcommand)]
enum AccountCommand {
    /// Create an isolated account and run the provider login flow.
    Add { provider: Provider, name: String },
    /// Re-run login for an existing isolated account.
    Login { provider: Provider, name: String },
    /// Launch the provider CLI inside an isolated account.
    Run {
        provider: Provider,
        name: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// List isolated accounts managed by aiwatch.
    List,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(Commands::Account { command }) = &cli.command {
        return run_account_command(command);
    }

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
            "no provider credentials discovered; run `aiwatch account add <provider> <name>` or configure account credential paths in ~/.config/aiwatch/config.toml"
        );
    }

    let history_path = (!cli.no_history).then_some(config.history_path.as_path());
    let mut coordinator = PollCoordinator::new(config.accounts.clone(), history_path)?;
    if cli.once || cli.json {
        let snapshot = coordinator.poll_once().await;
        if cli.json {
            println!("{}", serde_json::to_string_pretty(&snapshot)?);
        } else {
            let width = terminal::size().map_or(160, |(width, _)| width as usize);
            print!(
                "{}",
                output::render_text(
                    &snapshot,
                    width,
                    Duration::from_secs(config.poll_interval_secs)
                )
            );
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

fn run_account_command(command: &AccountCommand) -> Result<()> {
    let manager = AccountManager::new()?;
    match command {
        AccountCommand::Add { provider, name } => {
            manager.add(*provider, name)?;
            println!("Added {provider} account '{name}'.");
        }
        AccountCommand::Login { provider, name } => {
            manager.login(*provider, name)?;
            println!("Authenticated {provider} account '{name}'.");
        }
        AccountCommand::Run {
            provider,
            name,
            args,
        } => {
            manager.launch(*provider, name, args)?;
        }
        AccountCommand::List => {
            let accounts = manager.managed_accounts();
            if accounts.is_empty() {
                println!("No managed accounts.");
            } else {
                for account in accounts {
                    println!("{}\t{}", account.provider, account.name);
                }
            }
        }
    }
    Ok(())
}

async fn present_demo(snapshot: DashboardSnapshot, cli: &Cli) -> Result<()> {
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&snapshot)?);
        return Ok(());
    }
    let poll_interval = Duration::from_secs(cli.interval.unwrap_or(60).max(60));
    if cli.once {
        let width = terminal::size().map_or(160, |(width, _)| width as usize);
        print!("{}", output::render_text(&snapshot, width, poll_interval));
        return Ok(());
    }
    let (_snapshot_guard, snapshot_rx) = watch::channel(snapshot);
    let (refresh_tx, _refresh_guard) = mpsc::channel(1);
    ui::run(snapshot_rx, refresh_tx, poll_interval).await
}
