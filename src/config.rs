use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Deserialize)]
struct RawConfig {
    watch_dir: PathBuf,
    data_dir: Option<PathBuf>,
    max_snapshot_size: Option<u64>,
    #[serde(default)]
    ignore: Vec<String>,
}

pub struct Config {
    pub watch_dir: PathBuf,
    pub data_dir: PathBuf,
    pub max_snapshot_size: u64,
    pub ignore: Vec<String>,
}

const DEFAULT_MAX_SNAPSHOT_SIZE: u64 = 100 * 1024 * 1024; // 100MB

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config file: {}", path.display()))?;
        let raw: RawConfig =
            toml::from_str(&contents).with_context(|| "failed to parse config file")?;

        let watch_dir = raw
            .watch_dir
            .canonicalize()
            .with_context(|| format!("watch_dir does not exist: {}", raw.watch_dir.display()))?;

        let data_dir = match raw.data_dir {
            Some(d) => d,
            None => watch_dir.join(".replayfs"),
        };

        Ok(Config {
            watch_dir,
            data_dir,
            max_snapshot_size: raw.max_snapshot_size.unwrap_or(DEFAULT_MAX_SNAPSHOT_SIZE),
            ignore: raw.ignore,
        })
    }
}
