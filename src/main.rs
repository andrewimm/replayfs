mod config;
mod daemon;
mod error;
mod log;
mod replay;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "replayfs", about = "Filesystem watcher and replay tool")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the filesystem watcher daemon
    Start {
        /// Path to config file (TOML)
        #[arg(short, long)]
        config: PathBuf,
        /// Run in foreground (don't daemonize)
        #[arg(long)]
        foreground: bool,
    },
    /// Stop the running daemon
    Stop {
        /// Path to data directory (contains PID file and socket)
        #[arg(short, long)]
        data_dir: PathBuf,
    },
    /// Show daemon status
    Status {
        /// Path to data directory
        #[arg(short, long)]
        data_dir: PathBuf,
    },
    /// Replay filesystem state from log
    Replay {
        /// Path to data directory (contains log.ndjson and blobs/)
        #[arg(short, long)]
        data_dir: PathBuf,
        /// Output directory to reconstruct into
        #[arg(short, long)]
        output: PathBuf,
        /// Replay up to this sequence number
        #[arg(long)]
        until_seq: Option<u64>,
        /// Replay up to this elapsed time in milliseconds
        #[arg(long)]
        until_ms: Option<u64>,
        /// Replay events with original timing (sleeps between events)
        #[arg(long)]
        realtime: bool,
    },
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    match cli.command {
        Command::Start { config, foreground } => {
            let cfg = config::Config::load(&config)?;
            daemon::start(cfg, foreground)?;
        }
        Command::Stop { data_dir } => {
            daemon::stop(&data_dir)?;
        }
        Command::Status { data_dir } => {
            daemon::status(&data_dir)?;
        }
        Command::Replay {
            data_dir,
            output,
            until_seq,
            until_ms,
            realtime,
        } => {
            replay::replay(&data_dir, &output, until_seq, until_ms, realtime)?;
        }
    }

    Ok(())
}
