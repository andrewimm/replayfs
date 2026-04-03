use std::fs;
use std::io::Write;
use std::path::Path;

use sha2::{Digest, Sha256};
use tempfile::tempdir;

/// Helper: write a blob to the content-addressed store, return its hash.
fn write_blob(blob_dir: &Path, content: &[u8]) -> String {
    let hash = hex::encode(Sha256::digest(content));
    let prefix = &hash[..2];
    let dir = blob_dir.join(prefix);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(&hash), content).unwrap();
    hash
}

/// Helper: write an NDJSON log file from raw lines.
fn write_log(log_path: &Path, lines: &[&str]) {
    let mut f = fs::File::create(log_path).unwrap();
    for line in lines {
        writeln!(f, "{}", line).unwrap();
    }
}

#[test]
fn end_to_end_replay_creates_files() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let blob_dir = data_dir.join("blobs");
    let output = dir.path().join("output");

    fs::create_dir_all(&blob_dir).unwrap();

    let h1 = write_blob(&blob_dir, b"hello world");
    let h2 = write_blob(&blob_dir, b"updated content");

    write_log(
        &data_dir.join("log.ndjson"),
        &[
            &format!(r#"{{"type":"header","schema_version":1,"watch_dir":"/tmp/test"}}"#),
            &format!(r#"{{"type":"event","seq":1,"elapsed_ms":10,"op":"create","path":"a.txt","content_hash":"{}","size":11}}"#, h1),
            &format!(r#"{{"type":"event","seq":2,"elapsed_ms":20,"op":"create","path":"sub/b.txt","content_hash":"{}","size":15}}"#, h2),
        ],
    );

    // Run replay via the binary
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_replayfs"))
        .args(["replay", "-d", data_dir.to_str().unwrap(), "-o", output.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success());

    assert_eq!(fs::read_to_string(output.join("a.txt")).unwrap(), "hello world");
    assert_eq!(fs::read_to_string(output.join("sub/b.txt")).unwrap(), "updated content");
}

#[test]
fn replay_handles_modify_and_delete() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let blob_dir = data_dir.join("blobs");
    let output = dir.path().join("output");

    fs::create_dir_all(&blob_dir).unwrap();

    let h1 = write_blob(&blob_dir, b"version 1");
    let h2 = write_blob(&blob_dir, b"version 2");

    write_log(
        &data_dir.join("log.ndjson"),
        &[
            r#"{"type":"header","schema_version":1,"watch_dir":"/tmp/test"}"#,
            &format!(r#"{{"type":"event","seq":1,"elapsed_ms":10,"op":"create","path":"file.txt","content_hash":"{}","size":9}}"#, h1),
            &format!(r#"{{"type":"event","seq":2,"elapsed_ms":20,"op":"modify","path":"file.txt","content_hash":"{}","size":9}}"#, h2),
            r#"{"type":"event","seq":3,"elapsed_ms":30,"op":"create","path":"temp.txt","content_hash":null,"size":0}"#,
            r#"{"type":"event","seq":4,"elapsed_ms":40,"op":"delete","path":"temp.txt"}"#,
        ],
    );

    let status = std::process::Command::new(env!("CARGO_BIN_EXE_replayfs"))
        .args(["replay", "-d", data_dir.to_str().unwrap(), "-o", output.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success());

    // file.txt should have version 2
    assert_eq!(fs::read_to_string(output.join("file.txt")).unwrap(), "version 2");
    // temp.txt should not exist (was deleted)
    assert!(!output.join("temp.txt").exists());
}

#[test]
fn replay_handles_rename() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let blob_dir = data_dir.join("blobs");
    let output = dir.path().join("output");

    fs::create_dir_all(&blob_dir).unwrap();

    let h1 = write_blob(&blob_dir, b"content");

    write_log(
        &data_dir.join("log.ndjson"),
        &[
            r#"{"type":"header","schema_version":1,"watch_dir":"/tmp/test"}"#,
            &format!(r#"{{"type":"event","seq":1,"elapsed_ms":10,"op":"create","path":"old.txt","content_hash":"{}","size":7}}"#, h1),
            r#"{"type":"event","seq":2,"elapsed_ms":20,"op":"rename","path":"old.txt","dest_path":"new.txt"}"#,
        ],
    );

    let status = std::process::Command::new(env!("CARGO_BIN_EXE_replayfs"))
        .args(["replay", "-d", data_dir.to_str().unwrap(), "-o", output.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success());

    assert!(!output.join("old.txt").exists());
    assert_eq!(fs::read_to_string(output.join("new.txt")).unwrap(), "content");
}

#[test]
fn replay_until_seq_stops_early() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let blob_dir = data_dir.join("blobs");
    let output = dir.path().join("output");

    fs::create_dir_all(&blob_dir).unwrap();

    let h1 = write_blob(&blob_dir, b"first");
    let h2 = write_blob(&blob_dir, b"second");

    write_log(
        &data_dir.join("log.ndjson"),
        &[
            r#"{"type":"header","schema_version":1,"watch_dir":"/tmp/test"}"#,
            &format!(r#"{{"type":"event","seq":1,"elapsed_ms":10,"op":"create","path":"a.txt","content_hash":"{}","size":5}}"#, h1),
            &format!(r#"{{"type":"event","seq":2,"elapsed_ms":20,"op":"create","path":"b.txt","content_hash":"{}","size":6}}"#, h2),
        ],
    );

    let status = std::process::Command::new(env!("CARGO_BIN_EXE_replayfs"))
        .args([
            "replay",
            "-d", data_dir.to_str().unwrap(),
            "-o", output.to_str().unwrap(),
            "--until-seq", "1",
        ])
        .status()
        .unwrap();
    assert!(status.success());

    assert!(output.join("a.txt").exists());
    assert!(!output.join("b.txt").exists()); // seq 2 was not applied
}

#[test]
fn replay_rejects_future_schema_version() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let output = dir.path().join("output");

    fs::create_dir_all(&data_dir).unwrap();

    write_log(
        &data_dir.join("log.ndjson"),
        &[r#"{"type":"header","schema_version":99,"watch_dir":"/tmp/test"}"#],
    );

    let output_cmd = std::process::Command::new(env!("CARGO_BIN_EXE_replayfs"))
        .args(["replay", "-d", data_dir.to_str().unwrap(), "-o", output.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(!output_cmd.status.success());
    let stderr = String::from_utf8_lossy(&output_cmd.stderr);
    assert!(stderr.contains("schema version") || stderr.contains("unsupported"));
}
