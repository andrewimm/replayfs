use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tracing::{debug, error, info, warn};

use crate::config::Config;
use crate::daemon::snapshot;
use crate::log::{LogWriter, Operation};

pub fn run(config: &Config, shutdown: Arc<AtomicBool>) -> Result<()> {
    let watch_dir = &config.watch_dir;
    let data_dir = &config.data_dir;
    let blob_dir = data_dir.join("blobs");
    let log_path = data_dir.join("log.ndjson");

    std::fs::create_dir_all(&blob_dir)
        .with_context(|| format!("failed to create blob dir: {}", blob_dir.display()))?;

    let mut log_writer =
        LogWriter::create(&log_path, watch_dir).context("failed to create log writer")?;

    let ignore_set = build_ignore_set(&config.ignore, watch_dir)?;
    let data_dir_canon = data_dir
        .canonicalize()
        .unwrap_or_else(|_| data_dir.clone());

    let (tx, rx) = std::sync::mpsc::channel();

    let mut watcher = RecommendedWatcher::new(tx, notify::Config::default())
        .context("failed to create filesystem watcher")?;

    watcher
        .watch(watch_dir, RecursiveMode::Recursive)
        .with_context(|| format!("failed to watch directory: {}", watch_dir.display()))?;

    info!("watching {}", watch_dir.display());

    while !shutdown.load(Ordering::Relaxed) {
        match rx.recv_timeout(std::time::Duration::from_millis(250)) {
            Ok(Ok(event)) => {
                handle_event(
                    &event,
                    watch_dir,
                    &data_dir_canon,
                    &ignore_set,
                    &blob_dir,
                    config.max_snapshot_size,
                    &mut log_writer,
                );
            }
            Ok(Err(e)) => {
                error!("watcher error: {}", e);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                warn!("watcher channel disconnected");
                break;
            }
        }
    }

    info!("watcher shutting down");
    Ok(())
}

fn handle_event(
    event: &notify::Event,
    watch_dir: &Path,
    data_dir: &Path,
    ignore_set: &GlobSet,
    blob_dir: &Path,
    max_snapshot_size: u64,
    log_writer: &mut LogWriter,
) {
    let op = match event.kind {
        EventKind::Create(_) => Operation::Create,
        EventKind::Modify(notify::event::ModifyKind::Data(_)) => Operation::Modify,
        EventKind::Modify(notify::event::ModifyKind::Name(
            notify::event::RenameMode::Both,
        )) => Operation::Rename,
        EventKind::Remove(_) => Operation::Delete,
        _ => return,
    };

    // For rename, we expect two paths (from, to)
    if op == Operation::Rename && event.paths.len() == 2 {
        let from = &event.paths[0];
        let to = &event.paths[1];

        if should_ignore(from, watch_dir, data_dir, ignore_set)
            && should_ignore(to, watch_dir, data_dir, ignore_set)
        {
            return;
        }

        let from_rel = match relative_path(from, watch_dir) {
            Some(r) => r,
            None => return,
        };
        let to_rel = match relative_path(to, watch_dir) {
            Some(r) => r,
            None => return,
        };

        if let Err(e) = log_writer.append(
            Operation::Rename,
            from_rel,
            Some(to_rel),
            None,
            None,
        ) {
            error!("failed to write log entry: {}", e);
        }
        return;
    }

    // Single-path events
    for path in &event.paths {
        if should_ignore(path, watch_dir, data_dir, ignore_set) {
            continue;
        }

        let rel = match relative_path(path, watch_dir) {
            Some(r) => r,
            None => continue,
        };

        let (content_hash, size) = if matches!(op, Operation::Create | Operation::Modify) {
            match snapshot::snapshot_file(path, blob_dir, max_snapshot_size) {
                Ok((hash, sz)) => (Some(hash), Some(sz)),
                Err(e) => {
                    debug!("snapshot skipped for {}: {}", path.display(), e);
                    (None, None)
                }
            }
        } else {
            (None, None)
        };

        if let Err(e) = log_writer.append(op, rel, None, content_hash, size) {
            error!("failed to write log entry: {}", e);
        }
    }
}

fn should_ignore(path: &Path, watch_dir: &Path, data_dir: &Path, ignore_set: &GlobSet) -> bool {
    // Ignore anything inside data_dir
    if path.starts_with(data_dir) {
        return true;
    }

    if let Some(rel) = path.strip_prefix(watch_dir).ok() {
        let rel_str = rel.to_string_lossy();
        ignore_set.is_match(rel_str.as_ref())
    } else {
        true // outside watch dir
    }
}

fn relative_path(path: &Path, watch_dir: &Path) -> Option<String> {
    path.strip_prefix(watch_dir)
        .ok()
        .map(|r| r.to_string_lossy().into_owned())
}

fn build_ignore_set(patterns: &[String], _watch_dir: &Path) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        // Match the pattern itself (e.g. ".next", "*.swp")
        builder.add(
            Glob::new(pattern)
                .with_context(|| format!("invalid ignore pattern: {pattern}"))?,
        );
        // Match it nested anywhere (e.g. "sub/.next")
        builder.add(
            Glob::new(&format!("**/{pattern}"))
                .with_context(|| format!("invalid ignore pattern: {pattern}"))?,
        );
        // Match anything inside it as a directory prefix (e.g. ".next/cache/foo")
        if let Ok(g) = Glob::new(&format!("{pattern}/**")) {
            builder.add(g);
        }
        if let Ok(g) = Glob::new(&format!("**/{pattern}/**")) {
            builder.add(g);
        }
    }
    Ok(builder.build()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ignore_set(patterns: &[&str]) -> GlobSet {
        let patterns: Vec<String> = patterns.iter().map(|s| s.to_string()).collect();
        build_ignore_set(&patterns, Path::new("/unused")).unwrap()
    }

    fn is_ignored(set: &GlobSet, path: &str) -> bool {
        set.is_match(path)
    }

    #[test]
    fn ignores_directory_and_contents() {
        let set = ignore_set(&[".next"]);

        assert!(is_ignored(&set, ".next"));
        assert!(is_ignored(&set, ".next/cache"));
        assert!(is_ignored(&set, ".next/cache/build/abc.js"));
        assert!(is_ignored(&set, "sub/.next"));
        assert!(is_ignored(&set, "sub/.next/cache"));

        assert!(!is_ignored(&set, "src/main.rs"));
        assert!(!is_ignored(&set, "next.config.js"));
    }

    #[test]
    fn ignores_file_extension_pattern() {
        let set = ignore_set(&["*.swp"]);

        assert!(is_ignored(&set, "file.swp"));
        assert!(is_ignored(&set, "src/file.swp"));
        assert!(is_ignored(&set, "deep/nested/dir/file.swp"));

        assert!(!is_ignored(&set, "file.txt"));
        assert!(!is_ignored(&set, "swp"));
    }

    #[test]
    fn ignores_multiple_patterns() {
        let set = ignore_set(&[".git", "node_modules", "*.tmp"]);

        assert!(is_ignored(&set, ".git/config"));
        assert!(is_ignored(&set, "node_modules/react/index.js"));
        assert!(is_ignored(&set, "src/draft.tmp"));

        assert!(!is_ignored(&set, "src/app.tsx"));
    }

    #[test]
    fn should_ignore_filters_data_dir() {
        let set = ignore_set(&[]);
        let watch_dir = Path::new("/project");
        let data_dir = Path::new("/project/.replayfs");

        assert!(should_ignore(
            Path::new("/project/.replayfs/log.ndjson"),
            watch_dir,
            data_dir,
            &set
        ));
        assert!(!should_ignore(
            Path::new("/project/src/main.rs"),
            watch_dir,
            data_dir,
            &set
        ));
    }
}
