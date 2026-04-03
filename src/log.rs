use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::Instant;

use serde::{Deserialize, Serialize};

pub const CURRENT_SCHEMA_VERSION: u64 = 1;

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LogRow {
    Header {
        schema_version: u64,
        watch_dir: String,
    },
    Event(LogEntry),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LogEntry {
    pub seq: u64,
    pub elapsed_ms: u64,
    pub op: Operation,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dest_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Create,
    Modify,
    Delete,
    Rename,
}

pub struct LogWriter {
    writer: BufWriter<File>,
    seq: u64,
    start: Instant,
}

impl LogWriter {
    pub fn create(log_path: &Path, watch_dir: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)?;
        let mut writer = BufWriter::new(file);

        let header = LogRow::Header {
            schema_version: CURRENT_SCHEMA_VERSION,
            watch_dir: watch_dir.to_string_lossy().into_owned(),
        };
        serde_json::to_writer(&mut writer, &header).map_err(io_err)?;
        writer.write_all(b"\n")?;
        writer.flush()?;

        Ok(LogWriter {
            writer,
            seq: 0,
            start: Instant::now(),
        })
    }

    pub fn append(&mut self, op: Operation, path: String, dest_path: Option<String>, content_hash: Option<String>, size: Option<u64>) -> std::io::Result<()> {
        self.seq += 1;
        let entry = LogEntry {
            seq: self.seq,
            elapsed_ms: self.start.elapsed().as_millis() as u64,
            op,
            path,
            dest_path,
            content_hash,
            size,
        };
        let row = LogRow::Event(entry);
        serde_json::to_writer(&mut self.writer, &row).map_err(io_err)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(())
    }
}

fn io_err(e: serde_json::Error) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    #[test]
    fn log_header_roundtrip() {
        let header = LogRow::Header {
            schema_version: 1,
            watch_dir: "/tmp/test".into(),
        };
        let json = serde_json::to_string(&header).unwrap();
        assert!(json.contains("\"type\":\"header\""));
        assert!(json.contains("\"schema_version\":1"));

        let parsed: LogRow = serde_json::from_str(&json).unwrap();
        match parsed {
            LogRow::Header { schema_version, watch_dir } => {
                assert_eq!(schema_version, 1);
                assert_eq!(watch_dir, "/tmp/test");
            }
            _ => panic!("expected header"),
        }
    }

    #[test]
    fn log_event_roundtrip() {
        let entry = LogEntry {
            seq: 42,
            elapsed_ms: 1000,
            op: Operation::Modify,
            path: "src/main.rs".into(),
            dest_path: None,
            content_hash: Some("abcdef".into()),
            size: Some(512),
        };
        let row = LogRow::Event(entry);
        let json = serde_json::to_string(&row).unwrap();
        assert!(json.contains("\"type\":\"event\""));
        assert!(json.contains("\"op\":\"modify\""));
        assert!(!json.contains("dest_path")); // skip_serializing_if None

        let parsed: LogRow = serde_json::from_str(&json).unwrap();
        match parsed {
            LogRow::Event(e) => {
                assert_eq!(e.seq, 42);
                assert_eq!(e.op, Operation::Modify);
                assert_eq!(e.content_hash.as_deref(), Some("abcdef"));
            }
            _ => panic!("expected event"),
        }
    }

    #[test]
    fn log_writer_creates_valid_ndjson() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("log.ndjson");
        let watch_dir = Path::new("/tmp/watched");

        {
            let mut writer = LogWriter::create(&log_path, watch_dir).unwrap();
            writer.append(Operation::Create, "a.txt".into(), None, Some("hash1".into()), Some(10)).unwrap();
            writer.append(Operation::Delete, "b.txt".into(), None, None, None).unwrap();
            writer.append(Operation::Rename, "c.txt".into(), Some("d.txt".into()), None, None).unwrap();
        }

        let file = std::fs::File::open(&log_path).unwrap();
        let lines: Vec<String> = BufReader::new(file).lines().map(|l| l.unwrap()).collect();

        assert_eq!(lines.len(), 4); // header + 3 events

        // Verify header
        let header: LogRow = serde_json::from_str(&lines[0]).unwrap();
        match header {
            LogRow::Header { schema_version, .. } => assert_eq!(schema_version, 1),
            _ => panic!("first line should be header"),
        }

        // Verify events have incrementing seq
        for (i, line) in lines[1..].iter().enumerate() {
            let row: LogRow = serde_json::from_str(line).unwrap();
            match row {
                LogRow::Event(e) => assert_eq!(e.seq, (i + 1) as u64),
                _ => panic!("expected event"),
            }
        }
    }
}
