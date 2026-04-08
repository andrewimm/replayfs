mod config;
mod daemon;
mod error;
mod log;
mod replay;

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Context;
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
        /// Path to data directory (contains PID file and socket) [default: .replayfs]
        #[arg(short, long, default_value = ".replayfs")]
        data_dir: PathBuf,
    },
    /// Show daemon status
    Status {
        /// Path to data directory [default: .replayfs]
        #[arg(short, long, default_value = ".replayfs")]
        data_dir: PathBuf,
    },
    /// Replay filesystem state from log
    Replay {
        /// Path to data directory (contains log.ndjson and blobs/) [default: .replayfs]
        #[arg(short, long, default_value = ".replayfs")]
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
        /// Override package.json dependency versions (repeatable, format: package=version)
        #[arg(long = "pkg-override", value_name = "PKG=VERSION")]
        pkg_overrides: Vec<String>,
        /// JSON file of package version overrides (object of "package": "version" pairs)
        #[arg(long = "pkg-override-file", value_name = "FILE")]
        pkg_override_file: Option<PathBuf>,
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
            pkg_overrides,
            pkg_override_file,
        } => {
            let mut overrides = parse_pkg_overrides(&pkg_overrides)?;
            if let Some(file) = pkg_override_file {
                let content = std::fs::read_to_string(&file)
                    .with_context(|| format!("failed to read override file: {}", file.display()))?;
                let file_overrides: HashMap<String, String> = serde_json::from_str(&content)
                    .with_context(|| format!("failed to parse override file: {}", file.display()))?;
                overrides.extend(file_overrides);
            }
            replay::replay(&data_dir, &output, until_seq, until_ms, realtime, &overrides)?;
        }
    }

    Ok(())
}

fn parse_pkg_overrides(raw: &[String]) -> anyhow::Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    for entry in raw {
        let (key, value) = entry
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("invalid --pkg-override format: '{}' (expected PKG=VERSION)", entry))?;
        map.insert(key.to_string(), value.to_string());
    }
    Ok(map)
}
