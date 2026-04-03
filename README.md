# replayfs

A filesystem watcher that records every change to a directory and lets you replay them later. Think of it as a flight recorder for your files.

replayfs monitors a directory tree, logs every create, modify, delete, and rename event to an append-only NDJSON file, and snapshots file contents into a content-addressed blob store. You can then replay the log to reconstruct the directory state at any point in time.

## Building

Requires Rust 1.70+.

```
cargo build --release
```

The binary is at `target/release/replayfs`.

## Usage

### Configuration

Create a TOML config file:

```toml
watch_dir = "/path/to/your/project"

# Optional. Defaults to {watch_dir}/.replayfs
data_dir = "/path/to/your/project/.replayfs"

# Optional. Glob patterns to ignore (relative to watch_dir)
ignore = [
    ".git",
    "node_modules",
    "target",
    "*.swp",
]

# Optional. Skip snapshotting files larger than this (bytes). Default: 100MB
max_snapshot_size = 104857600
```

### Start watching

```
# Run in the foreground (ctrl-c to stop)
replayfs start --config config.toml --foreground

# Run as a background daemon
replayfs start --config config.toml
```

### Check status

```
replayfs status --data-dir /path/to/your/project/.replayfs
```

### Stop the daemon

```
replayfs stop --data-dir /path/to/your/project/.replayfs
```

### Replay

Reconstruct the full directory state:

```
replayfs replay --data-dir /path/to/your/project/.replayfs --output /tmp/replay
```

Reconstruct state up to a specific point:

```
# Stop after sequence number 100
replayfs replay --data-dir .replayfs --output /tmp/replay --until-seq 100

# Stop after 5000ms from recording start
replayfs replay --data-dir .replayfs --output /tmp/replay --until-ms 5000
```

Replay with original timing (files appear in the output directory at the same pace they were originally changed):

```
replayfs replay --data-dir .replayfs --output /tmp/replay --realtime
```

This can be combined with `--until-seq` or `--until-ms` to replay a specific window in real time.

### Portable archives

The `.replayfs` directory is fully self-contained — it holds both the event log and all file snapshots. You can zip it up, move it to another machine, and replay there:

```
# On the original machine
zip -r replayfs-archive.zip /path/to/your/project/.replayfs

# On another machine
unzip replayfs-archive.zip
replayfs replay --data-dir .replayfs --output ./restored
```

This works cross-platform (macOS and Linux) since the archive format is just NDJSON and content-addressed blobs on disk.

## How it works

### Watching

replayfs uses the `notify` crate (backed by FSEvents on macOS) to recursively watch a directory. When an event arrives:

1. The path is checked against the ignore list and data directory (to avoid feedback loops).
2. For creates and modifies, the file contents are read, hashed with SHA-256, and stored in a content-addressed blob store under `{data_dir}/blobs/{hash[0..2]}/{hash}`. Duplicate content is stored only once.
3. A JSON event is appended to `{data_dir}/log.ndjson`.

### Log format

The log is NDJSON (one JSON object per line). The first line is always a metadata header:

```json
{"type":"header","schema_version":1,"watch_dir":"/absolute/path"}
```

Subsequent lines are events:

```json
{"type":"event","seq":1,"elapsed_ms":42,"op":"create","path":"src/main.rs","content_hash":"ab3f...","size":1024}
{"type":"event","seq":2,"elapsed_ms":100,"op":"modify","path":"src/main.rs","content_hash":"cd5e...","size":1048}
{"type":"event","seq":3,"elapsed_ms":200,"op":"rename","path":"old.rs","dest_path":"new.rs"}
{"type":"event","seq":4,"elapsed_ms":300,"op":"delete","path":"tmp.rs"}
```

- `seq` — monotonically increasing sequence number
- `elapsed_ms` — milliseconds since the daemon started
- `op` — `create`, `modify`, `delete`, or `rename`
- `path` — file path relative to the watched directory
- `content_hash` — SHA-256 hex digest (present for create/modify when snapshotted)
- `size` — file size in bytes at snapshot time

### Replaying

The replay command reads the log sequentially and builds up a map of `path -> content_hash`. Creates and modifies insert or update entries; deletes remove them; renames move them. At the end (or at the requested stop point), each surviving entry is materialized by copying its blob to the output directory.

### Data directory layout

```
{data_dir}/
  log.ndjson          # Append-only event log
  blobs/
    ab/ab3f...        # Content-addressed file snapshots
  replayfs.pid        # PID of running daemon
  replayfs.sock       # Unix socket for daemon control
```

## Tests

```
cargo test
```

Unit tests cover the blob store (hashing, dedup, size limits) and log serialization. Integration tests exercise the full replay pipeline: create, modify, delete, rename, `--until-seq`, and schema version rejection.
