use std::fs;
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

/// Snapshot a file into the content-addressed blob store.
/// Returns (hex_hash, file_size) on success.
pub fn snapshot_file(
    abs_path: &Path,
    blob_dir: &Path,
    max_size: u64,
) -> anyhow::Result<(String, u64)> {
    let meta = fs::metadata(abs_path)?;

    if meta.is_symlink() {
        anyhow::bail!("skipping symlink: {}", abs_path.display());
    }

    if !meta.is_file() {
        anyhow::bail!("not a regular file: {}", abs_path.display());
    }

    let size = meta.len();
    if size > max_size {
        anyhow::bail!(
            "file too large ({} bytes, max {}): {}",
            size,
            max_size,
            abs_path.display()
        );
    }

    let mut file = fs::File::open(abs_path)?;
    let mut buf = Vec::with_capacity(size as usize);
    file.read_to_end(&mut buf)?;

    let hash = Sha256::digest(&buf);
    let hex_hash = hex::encode(hash);

    let prefix = &hex_hash[..2];
    let blob_path = blob_dir.join(prefix).join(&hex_hash);

    if !blob_path.exists() {
        fs::create_dir_all(blob_path.parent().unwrap())?;
        let tmp = blob_path.with_extension("tmp");
        fs::write(&tmp, &buf)?;
        fs::rename(&tmp, &blob_path)?;
    }

    Ok((hex_hash, size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn snapshot_stores_by_hash() {
        let dir = tmp_dir();
        let blob_dir = dir.path().join("blobs");
        let file_path = dir.path().join("hello.txt");
        fs::write(&file_path, "hello world").unwrap();

        let (hash, size) = snapshot_file(&file_path, &blob_dir, 1024 * 1024).unwrap();

        assert_eq!(size, 11);
        assert_eq!(hash.len(), 64); // SHA-256 hex

        // Blob should exist at blobs/{prefix}/{hash}
        let blob_path = blob_dir.join(&hash[..2]).join(&hash);
        assert!(blob_path.exists());
        assert_eq!(fs::read_to_string(&blob_path).unwrap(), "hello world");
    }

    #[test]
    fn snapshot_deduplicates() {
        let dir = tmp_dir();
        let blob_dir = dir.path().join("blobs");

        let f1 = dir.path().join("a.txt");
        let f2 = dir.path().join("b.txt");
        fs::write(&f1, "same content").unwrap();
        fs::write(&f2, "same content").unwrap();

        let (h1, _) = snapshot_file(&f1, &blob_dir, 1024 * 1024).unwrap();
        let (h2, _) = snapshot_file(&f2, &blob_dir, 1024 * 1024).unwrap();

        assert_eq!(h1, h2);
    }

    #[test]
    fn snapshot_rejects_oversized() {
        let dir = tmp_dir();
        let blob_dir = dir.path().join("blobs");
        let file_path = dir.path().join("big.txt");
        fs::write(&file_path, "too big").unwrap();

        let result = snapshot_file(&file_path, &blob_dir, 3); // max 3 bytes
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("too large"));
    }

    #[test]
    fn snapshot_rejects_directory() {
        let dir = tmp_dir();
        let blob_dir = dir.path().join("blobs");
        let subdir = dir.path().join("subdir");
        fs::create_dir(&subdir).unwrap();

        let result = snapshot_file(&subdir, &blob_dir, 1024 * 1024);
        assert!(result.is_err());
    }
}
