//! Atomic Parquet writes (CH-4400).
//!
//! Every Parquet snapshot in the being's `sleep()` path used the same
//! pattern: `OpenOptions::create + truncate(true) + write + close`. If the
//! process dies between `truncate` and `close` (kernel hard-freeze, power
//! loss, OOM kill), the file on disk is left at zero bytes. EX-4329 hit
//! this exact failure mode at 17:14 UTC on 2026-05-06 — `world.parquet`
//! ended up empty after a thermal freeze, and the next being load failed
//! with `Parquet error: EOF: Parquet file too small. Size is 0 but need 8`.
//!
//! The fix is a write-rename pattern. `write_parquet_atomic` writes to a
//! sibling `*.tmp` file, fsyncs it, then renames into place. Linux
//! guarantees that same-filesystem rename is atomic — readers see either
//! the prior valid file or the new valid file, never an empty one. The
//! parent directory is fsynced afterwards so the rename itself survives
//! a power loss.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::Path;

/// Write to `path` atomically.
///
/// The closure receives ownership of a freshly-created tmp file (sibling of
/// `path`, suffixed `.tmp`) and is responsible for completing the write —
/// including any wrapping writer's `close()` (e.g. `ArrowWriter::close`).
/// On success, the tmp file is fsynced and renamed onto `path`.
///
/// On any error from the closure, the tmp file is removed and `path` is
/// left untouched.
///
/// ## Why fsync the parent directory
///
/// Linux's atomic rename guarantee covers the *file content* but the
/// directory entry pointing at the new inode needs to be flushed to make
/// the rename itself durable across power loss. Best-effort: if the
/// directory open fails (rare, e.g. unusual mount permissions), we still
/// return success — the rename has already happened in memory.
pub fn write_parquet_atomic<F>(path: &Path, write: F) -> io::Result<()>
where
    F: FnOnce(File) -> io::Result<()>,
{
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path has no parent directory: {}", path.display()),
        )
    })?;
    fs::create_dir_all(parent)?;

    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path has no file name: {}", path.display()),
        )
    })?;
    let mut tmp_name = file_name.to_owned();
    tmp_name.push(".tmp");
    let tmp_path = parent.join(&tmp_name);

    // Clean any stale tmp from a prior aborted write. Ignore NotFound.
    if let Err(e) = fs::remove_file(&tmp_path)
        && e.kind() != io::ErrorKind::NotFound
    {
        return Err(e);
    }

    let tmp_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp_path)?;

    if let Err(e) = write(tmp_file) {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }

    // Reopen tmp to fsync — Parquet writers (e.g. `ArrowWriter::close`)
    // drop their `File` without an explicit fsync, so contents may live
    // only in the page cache.
    {
        let f = File::open(&tmp_path)?;
        f.sync_all()?;
    }

    // Atomic rename — same-filesystem rename is atomic on Linux.
    fs::rename(&tmp_path, path)?;

    // Best-effort fsync of the parent directory so the rename survives
    // a power loss.
    if let Ok(dir) = File::open(parent) {
        let _ = dir.sync_all();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use tempfile::TempDir;

    fn read_to_vec(path: &Path) -> io::Result<Vec<u8>> {
        let mut buf = Vec::new();
        File::open(path)?.read_to_end(&mut buf)?;
        Ok(buf)
    }

    #[test]
    fn write_creates_file_with_expected_contents() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("subdir").join("data.parquet");

        write_parquet_atomic(&path, |mut f| {
            f.write_all(b"hello world")?;
            Ok(())
        })
        .unwrap();

        assert!(path.exists());
        assert_eq!(read_to_vec(&path).unwrap(), b"hello world");
        // Tmp must have been cleaned up.
        let tmp_path = path.with_file_name(format!(
            "{}.tmp",
            path.file_name().unwrap().to_str().unwrap()
        ));
        assert!(!tmp_path.exists(), "tmp file leaked: {tmp_path:?}");
    }

    #[test]
    fn closure_error_leaves_target_untouched() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("data.parquet");

        // Pre-populate target with a known good payload.
        fs::write(&path, b"prior good payload").unwrap();
        let prior = read_to_vec(&path).unwrap();

        let result: io::Result<()> = write_parquet_atomic(&path, |mut f| {
            f.write_all(b"start")?;
            // Simulate failure midway through.
            Err(io::Error::other("simulated write failure"))
        });
        assert!(result.is_err(), "expected error to propagate");

        // Target untouched — caller never sees the partial write.
        assert_eq!(read_to_vec(&path).unwrap(), prior);
        // Tmp file cleaned up.
        let tmp_path = path.with_file_name(format!(
            "{}.tmp",
            path.file_name().unwrap().to_str().unwrap()
        ));
        assert!(!tmp_path.exists(), "tmp file leaked: {tmp_path:?}");
    }

    #[test]
    fn stale_tmp_from_prior_crash_is_overwritten() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("data.parquet");
        let tmp_path = path.with_file_name(format!(
            "{}.tmp",
            path.file_name().unwrap().to_str().unwrap()
        ));

        // Simulate a prior crashed write that left a tmp behind.
        fs::write(&tmp_path, b"stale half-written garbage").unwrap();

        write_parquet_atomic(&path, |mut f| {
            f.write_all(b"fresh")?;
            Ok(())
        })
        .unwrap();

        assert_eq!(read_to_vec(&path).unwrap(), b"fresh");
        assert!(!tmp_path.exists());
    }

    #[test]
    fn replaces_existing_target_atomically() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("data.parquet");

        write_parquet_atomic(&path, |mut f| {
            f.write_all(b"v1")?;
            Ok(())
        })
        .unwrap();
        assert_eq!(read_to_vec(&path).unwrap(), b"v1");

        write_parquet_atomic(&path, |mut f| {
            f.write_all(b"v2 longer payload")?;
            Ok(())
        })
        .unwrap();
        assert_eq!(read_to_vec(&path).unwrap(), b"v2 longer payload");
    }

    #[test]
    fn parent_directory_is_created() {
        let tmp = TempDir::new().unwrap();
        let path = tmp
            .path()
            .join("a")
            .join("b")
            .join("c")
            .join("data.parquet");

        write_parquet_atomic(&path, |mut f| {
            f.write_all(b"deep")?;
            Ok(())
        })
        .unwrap();

        assert!(path.exists());
        assert_eq!(read_to_vec(&path).unwrap(), b"deep");
    }

    #[test]
    fn rejects_path_with_no_parent() {
        // Filesystem root has no parent.
        let result = write_parquet_atomic(Path::new("/"), |_f| Ok(()));
        assert!(result.is_err());
    }
}
