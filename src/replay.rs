use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{BufRead, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tracing::info;

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
    pkg_overrides: &HashMap<String, String>,
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

    // Count total events for progress display
    let total_events = {
        let f = fs::File::open(&log_path)?;
        BufReader::new(f).lines().count().saturating_sub(1) // subtract header
    };

    let mut state: BTreeMap<String, FileState> = BTreeMap::new();
    let mut count = 0u64;
    let mut last_ms: Option<u64> = None;
    let mut needs_install = false;

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
                    sleep_with_countdown(delta, &entry);
                }
            }
            last_ms = Some(entry.elapsed_ms);
        }

        print_status(&entry, total_events);

        apply_entry(&mut state, &entry);
        count += 1;

        if realtime {
            materialize_entry(&entry, &state, output, &blob_dir)?;
            if entry.op == Operation::Install {
                clear_status();
                apply_pkg_overrides(output, &entry.path, pkg_overrides)?;
                run_install(output, &entry.path)?;
            }
        }

        if entry.op == Operation::Install {
            needs_install = true;
        }
    }

    clear_status();

    if !realtime {
        print_status_msg("materializing files...");
        materialize_all(&state, output, &blob_dir)?;
        if needs_install {
            clear_status();
            apply_pkg_overrides(output, "package.json", pkg_overrides)?;
            run_install(output, "package.json")?;
        }
        clear_status();
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
        Operation::Install => {} // handled in main loop
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

fn op_symbol(op: Operation) -> &'static str {
    match op {
        Operation::Create => "+",
        Operation::Modify => "~",
        Operation::Delete => "-",
        Operation::Rename => ">",
        Operation::Install => "*",
    }
}

fn op_label(op: Operation) -> &'static str {
    match op {
        Operation::Create => "create",
        Operation::Modify => "modify",
        Operation::Delete => "delete",
        Operation::Rename => "rename",
        Operation::Install => "install",
    }
}

fn print_status(entry: &LogEntry, total: usize) {
    let sym = op_symbol(entry.op);
    let label = op_label(entry.op);
    let elapsed = format_duration(entry.elapsed_ms);
    let path = if entry.op == Operation::Install {
        "installing dependencies...".to_string()
    } else if entry.op == Operation::Rename {
        if let Some(dest) = &entry.dest_path {
            format!("{} -> {}", entry.path, dest)
        } else {
            entry.path.clone()
        }
    } else {
        entry.path.clone()
    };

    eprint!(
        "\r\x1b[K  [{}/{}] {} {} {} ({})",
        entry.seq, total, sym, label, path, elapsed
    );
    let _ = std::io::stderr().flush();
}

fn print_status_msg(msg: &str) {
    eprint!("\r\x1b[K  {}", msg);
    let _ = std::io::stderr().flush();
}

fn clear_status() {
    eprint!("\r\x1b[K");
    let _ = std::io::stderr().flush();
}

fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let mins = ms / 60_000;
        let secs = (ms % 60_000) / 1000;
        format!("{}m{:02}s", mins, secs)
    }
}

fn sleep_with_countdown(delta_ms: u64, next_entry: &LogEntry) {
    if delta_ms <= 500 {
        thread::sleep(Duration::from_millis(delta_ms));
        return;
    }

    let sym = op_symbol(next_entry.op);
    let path = &next_entry.path;
    let start = std::time::Instant::now();
    let total = Duration::from_millis(delta_ms);

    while start.elapsed() < total {
        let remaining = total.saturating_sub(start.elapsed());
        let secs = remaining.as_secs_f64();
        eprint!(
            "\r\x1b[K  \x1b[2mwaiting {:.1}s\x1b[0m  {} next: {} {}",
            secs, sym, op_label(next_entry.op), path
        );
        let _ = std::io::stderr().flush();

        let sleep_step = remaining.min(Duration::from_millis(100));
        thread::sleep(sleep_step);
    }
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
        Operation::Install => {} // handled separately during materialization
    }
}

fn apply_pkg_overrides(
    output_dir: &Path,
    package_json_rel: &str,
    overrides: &HashMap<String, String>,
) -> Result<()> {
    if overrides.is_empty() {
        return Ok(());
    }

    let pkg_path = output_dir.join(package_json_rel);
    if !pkg_path.exists() {
        return Ok(());
    }

    let content = fs::read_to_string(&pkg_path)
        .with_context(|| format!("failed to read {}", pkg_path.display()))?;
    let mut pkg: serde_json::Value =
        serde_json::from_str(&content).with_context(|| "failed to parse package.json")?;

    let mut changed = false;

    // Replace matching versions in dependencies and devDependencies
    for section in &["dependencies", "devDependencies"] {
        if let Some(deps) = pkg.get_mut(*section).and_then(|v| v.as_object_mut()) {
            for (key, value) in overrides {
                if deps.contains_key(key) {
                    println!("  overriding {}.{}: {} -> {}", section, key, deps[key], value);
                    deps.insert(key.clone(), serde_json::Value::String(value.clone()));
                    changed = true;
                }
            }
        }
    }

    // Inject all overrides into the "overrides" section
    let overrides_section = pkg
        .as_object_mut()
        .unwrap()
        .entry("overrides")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if let Some(obj) = overrides_section.as_object_mut() {
        for (key, value) in overrides {
            println!("  overrides.{}: {}", key, value);
            obj.insert(key.clone(), serde_json::Value::String(value.clone()));
            changed = true;
        }
    }

    if changed {
        let output = serde_json::to_string_pretty(&pkg)?;
        fs::write(&pkg_path, output + "\n")
            .with_context(|| format!("failed to write {}", pkg_path.display()))?;
    }

    Ok(())
}

fn detect_package_manager(dir: &Path) -> &'static str {
    if dir.join("pnpm-lock.yaml").exists() {
        "pnpm"
    } else if dir.join("bun.lockb").exists() || dir.join("bun.lock").exists() {
        "bun"
    } else {
        "npm"
    }
}

fn run_install(output_dir: &Path, package_json_rel: &str) -> Result<()> {
    // Run install in the directory containing the package.json
    let package_json_path = output_dir.join(package_json_rel);
    let install_dir = package_json_path
        .parent()
        .unwrap_or(output_dir);

    let pm = detect_package_manager(install_dir);
    info!("running {} install in {}", pm, install_dir.display());
    println!("running `{} install` in {}", pm, install_dir.display());

    let status = Command::new(pm)
        .arg("install")
        .current_dir(install_dir)
        .status()
        .with_context(|| format!("failed to run `{} install`", pm))?;

    if !status.success() {
        bail!("`{} install` failed with exit code {:?}", pm, status.code());
    }

    Ok(())
}
