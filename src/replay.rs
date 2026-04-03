use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::error::DaemonError;
use crate::log::{LogEntry, LogRow, Operation, CURRENT_SCHEMA_VERSION};

pub(crate) struct FileState {
    pub content_hash: Option<String>,
}

pub fn replay(
    data_dir: &Path,
    output: &Path,
    until_seq: Option<u64>,
    until_ms: Option<u64>,
    realtime: bool,
) -> Result<()> {
    let log_path = data_dir.join("log.ndjson");
    let blob_dir = data_dir.join("blobs");

    let file = fs::File::open(&log_path)
        .with_context(|| format!("failed to open log: {}", log_path.display()))?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();

    // Read and validate header
    let header_line = lines
        .next()
        .ok_or_else(|| anyhow::anyhow!("log file is empty"))?
        .context("failed to read header line")?;

    let header: LogRow =
        serde_json::from_str(&header_line).context("failed to parse log header")?;

    match header {
        LogRow::Header {
            schema_version,
            watch_dir,
        } => {
            if schema_version > CURRENT_SCHEMA_VERSION {
                return Err(DaemonError::UnsupportedSchemaVersion(
                    schema_version,
                    CURRENT_SCHEMA_VERSION,
                )
                .into());
            }
            println!(
                "replaying log (schema v{}, watched: {})",
                schema_version, watch_dir
            );
        }
        _ => bail!("expected header as first log line"),
    }

    fs::create_dir_all(output)
        .with_context(|| format!("failed to create output dir: {}", output.display()))?;

    let mut state: BTreeMap<String, FileState> = BTreeMap::new();
    let mut count = 0u64;
    let mut last_ms: Option<u64> = None;

    for line_result in lines {
        let line = line_result.context("failed to read log line")?;
        if line.trim().is_empty() {
            continue;
        }

        let row: LogRow = serde_json::from_str(&line).context("failed to parse log entry")?;
        let entry = match row {
            LogRow::Event(e) => e,
            _ => continue,
        };

        // Check stop conditions
        if let Some(max_seq) = until_seq {
            if entry.seq > max_seq {
                break;
            }
        }
        if let Some(max_ms) = until_ms {
            if entry.elapsed_ms > max_ms {
                break;
            }
        }

        // In realtime mode, sleep for the delta between events (skip first)
        if realtime {
            if let Some(prev_ms) = last_ms {
                let delta = entry.elapsed_ms.saturating_sub(prev_ms);
                if delta > 0 {
                    thread::sleep(Duration::from_millis(delta));
                }
            }
            last_ms = Some(entry.elapsed_ms);
        }

        apply_entry(&mut state, &entry);
        count += 1;

        if realtime {
            materialize_entry(&entry, &state, output, &blob_dir)?;
        }
    }

    if !realtime {
        materialize_all(&state, output, &blob_dir)?;
    }

    println!(
        "replayed {} events into {}",
        count,
        output.display()
    );

    Ok(())
}

fn materialize_entry(
    entry: &LogEntry,
    state: &BTreeMap<String, FileState>,
    output: &Path,
    blob_dir: &Path,
) -> Result<()> {
    match entry.op {
        Operation::Create | Operation::Modify => {
            let out_path = output.join(&entry.path);
            if let Some(parent) = out_path.parent() {
                ensure_dir(parent)?;
            }
            if let Some(fs) = state.get(&entry.path) {
                write_blob(&out_path, &fs.content_hash, blob_dir, &entry.path)?;
            }
        }
        Operation::Delete => {
            let out_path = output.join(&entry.path);
            if out_path.is_dir() {
                fs::remove_dir_all(&out_path).ok();
            } else if out_path.exists() {
                fs::remove_file(&out_path).ok();
            }
        }
        Operation::Rename => {
            if let Some(dest) = &entry.dest_path {
                let from_path = output.join(&entry.path);
                let to_path = output.join(dest);
                if let Some(parent) = to_path.parent() {
                    ensure_dir(parent)?;
                }
                if from_path.exists() {
                    fs::rename(&from_path, &to_path)?;
                } else if let Some(fs) = state.get(dest) {
                    write_blob(&to_path, &fs.content_hash, blob_dir, dest)?;
                }
            }
        }
    }
    Ok(())
}

fn materialize_all(
    state: &BTreeMap<String, FileState>,
    output: &Path,
    blob_dir: &Path,
) -> Result<()> {
    for (rel_path, file_state) in state {
        let out_path = output.join(rel_path);
        if let Some(parent) = out_path.parent() {
            ensure_dir(parent)?;
        }
        write_blob(&out_path, &file_state.content_hash, blob_dir, rel_path)?;
    }
    Ok(())
}

/// Create a directory path, removing any files that conflict with needed directories.
fn ensure_dir(path: &Path) -> Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    // Walk from the root to find any file blocking a needed directory
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        if current.is_file() {
            fs::remove_file(&current)
                .with_context(|| format!("failed to remove file blocking directory: {}", current.display()))?;
        }
    }
    fs::create_dir_all(path)
        .with_context(|| format!("failed to create directory: {}", path.display()))?;
    Ok(())
}

fn write_blob(
    out_path: &Path,
    content_hash: &Option<String>,
    blob_dir: &Path,
    rel_path: &str,
) -> Result<()> {
    let Some(hash) = content_hash else {
        // No snapshot — nothing to write
        return Ok(());
    };

    let prefix = &hash[..2];
    let blob_path = blob_dir.join(prefix).join(hash);
    if !blob_path.exists() {
        eprintln!(
            "warning: blob missing for {} (hash {}), skipping",
            rel_path, hash
        );
        return Ok(());
    }

    // Remove anything at the target path (could be a dir from a previous state)
    if out_path.is_dir() {
        fs::remove_dir_all(out_path)?;
    } else if out_path.exists() {
        fs::remove_file(out_path)?;
    }

    fs::copy(&blob_path, out_path)
        .with_context(|| format!("failed to copy blob to {}", out_path.display()))?;

    Ok(())
}

// Exposed for testing
pub(crate) fn apply_entry(state: &mut BTreeMap<String, FileState>, entry: &LogEntry) {
    match entry.op {
        Operation::Create | Operation::Modify => {
            state.insert(
                entry.path.clone(),
                FileState {
                    content_hash: entry.content_hash.clone(),
                },
            );
        }
        Operation::Delete => {
            state.remove(&entry.path);
        }
        Operation::Rename => {
            if let Some(dest) = &entry.dest_path {
                // Remove source from state
                let old = state.remove(&entry.path);
                // If the rename event has a content_hash, use it (fresh snapshot
                // of the destination). Otherwise fall back to the source's hash.
                let content_hash = if entry.content_hash.is_some() {
                    entry.content_hash.clone()
                } else {
                    old.and_then(|s| s.content_hash)
                };
                state.insert(
                    dest.clone(),
                    FileState { content_hash },
                );
            }
        }
    }
}
