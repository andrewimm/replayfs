use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;

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

    // Process events
    let mut state: BTreeMap<String, FileState> = BTreeMap::new();
    let mut count = 0u64;

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

        apply_entry(&mut state, &entry);
        count += 1;
    }

    // Materialize to output directory
    fs::create_dir_all(output)
        .with_context(|| format!("failed to create output dir: {}", output.display()))?;

    let mut materialized = 0u64;
    for (rel_path, file_state) in &state {
        let out_path = output.join(rel_path);
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }

        if let Some(hash) = &file_state.content_hash {
            let prefix = &hash[..2];
            let blob_path = blob_dir.join(prefix).join(hash);
            if blob_path.exists() {
                fs::copy(&blob_path, &out_path).with_context(|| {
                    format!("failed to copy blob to {}", out_path.display())
                })?;
                materialized += 1;
            } else {
                // Blob missing — create empty placeholder
                fs::File::create(&out_path)?;
                eprintln!(
                    "warning: blob missing for {} (hash {}), created empty file",
                    rel_path, hash
                );
            }
        } else {
            // No snapshot — create empty placeholder
            fs::File::create(&out_path)?;
        }
    }

    println!(
        "replayed {} events, materialized {} files into {}",
        count,
        materialized,
        output.display()
    );

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
                if let Some(fs) = state.remove(&entry.path) {
                    state.insert(dest.clone(), fs);
                } else {
                    // Source wasn't tracked (existed before recording started) —
                    // insert dest with no content
                    state.insert(
                        dest.clone(),
                        FileState {
                            content_hash: None,
                        },
                    );
                }
            }
        }
    }
}
