//! Crash-safe, serialized writes for the small TOML/JSON files filer keeps.
//!
//! Every write goes to a hidden temp file in the destination directory, is
//! flushed, and is then renamed over the destination. A reader therefore sees
//! either the old or the new complete file, never a partial one; a `kill -9`
//! between the two steps leaves the old file and an orphaned temp file, which
//! is never read.
//!
//! Writers are serialized with an advisory lock on a hidden sidecar file
//! (`.<name>.lock`). The lock uses [`std::fs::File::try_lock`], so it needs no
//! dependency, and the kernel releases it when the process dies — there is no
//! stale-lock timeout and no lock-file cleanup on drop. The sidecar is kept on
//! purpose: unlinking it would race with a writer that is about to lock it,
//! and readers would then have to recreate it.
//!
//! The lock covers the write itself, not the read-modify-write that produces
//! the document (see `bundle_cmd`). Two concurrent seeds still win by last
//! writer, but they cannot corrupt the file; per-session state is #13's job.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long a writer waits for a concurrent writer before failing the write.
/// Every document here is small, so a legitimate write never takes this long.
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const LOCK_RETRY: Duration = Duration::from_millis(10);

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Exclusive write lock for `path`, backed by a sidecar next to it. The
/// advisory lock is released when the `File` is dropped (or the process dies).
struct Lock {
    _file: File,
}

impl Lock {
    fn acquire(path: &Path) -> io::Result<Lock> {
        let lock_path = lock_path(path);
        fs::create_dir_all(parent_of(&lock_path))?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        let start = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Lock { _file: file }),
                Err(TryLockError::WouldBlock) if start.elapsed() < LOCK_TIMEOUT => {
                    thread::sleep(LOCK_RETRY);
                }
                Err(TryLockError::WouldBlock) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        format!("timed out waiting for {}", lock_path.display()),
                    ));
                }
                Err(TryLockError::Error(error)) => return Err(error),
            }
        }
    }
}

/// Replace the contents of `path` atomically, under its write lock.
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let _lock = Lock::acquire(path)?;
    replace(path, bytes)
}

fn replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = parent_of(path);
    fs::create_dir_all(parent)?;
    let temp = temp_path(path);
    let result = write_temp(&temp, bytes).and_then(|()| fs::rename(&temp, path));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    } else {
        sync_dir(parent);
    }
    result
}

/// Remove `path` atomically, under its write lock. `Ok(false)` when there was
/// nothing to remove; the parent directory is not created just to delete.
pub fn remove(path: &Path) -> io::Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let _lock = Lock::acquire(path)?;
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn write_temp(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Flush the directory entry after the rename. Only durability across a power
/// loss needs this; `kill -9` is already covered by the rename itself.
#[cfg(unix)]
fn sync_dir(path: &Path) {
    if let Ok(dir) = File::open(path) {
        let _ = dir.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_dir(_path: &Path) {}

fn parent_of(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "filer".to_string())
}

fn lock_path(path: &Path) -> PathBuf {
    path.with_file_name(format!(".{}.lock", file_name(path)))
}

fn temp_path(path: &Path) -> PathBuf {
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    path.with_file_name(format!(
        ".{}.{}.{seq}.{nanos}.tmp",
        file_name(path),
        std::process::id()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("filer-atomic-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn temp_files(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect()
    }

    #[test]
    fn replaces_and_leaves_no_temp_file() {
        let dir = temp_dir("replace");
        let path = dir.join("nested").join("state.toml");

        write(&path, b"one").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "one");
        write(&path, b"two").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two");

        assert!(temp_files(path.parent().unwrap()).is_empty());
        // The sidecar is intentional and stays around.
        assert!(path.parent().unwrap().join(".state.toml.lock").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_is_idempotent() {
        let dir = temp_dir("remove");
        let path = dir.join("state.toml");
        write(&path, b"x").unwrap();
        assert!(remove(&path).unwrap());
        assert!(!remove(&path).unwrap());
        assert!(!path.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_writers_never_expose_a_partial_file() {
        let dir = temp_dir("concurrent");
        let path = dir.join("state.toml");
        let payloads: std::sync::Arc<Vec<String>> = std::sync::Arc::new(
            (0..6)
                .map(|round| format!("round = {round}\n{}", "x".repeat(64 * 1024)))
                .collect(),
        );
        let mut writers = Vec::new();
        for index in 0..payloads.len() {
            let path = path.clone();
            let payloads = std::sync::Arc::clone(&payloads);
            writers.push(std::thread::spawn(move || {
                for _ in 0..10 {
                    write(&path, payloads[index].as_bytes()).unwrap();
                }
            }));
        }
        let reader = {
            let path = path.clone();
            let payloads = std::sync::Arc::clone(&payloads);
            std::thread::spawn(move || {
                while !path.exists() {
                    std::thread::yield_now();
                }
                let mut seen = 0;
                for _ in 0..500 {
                    let text = fs::read_to_string(&path).unwrap();
                    assert!(payloads.contains(&text), "read a torn file");
                    seen += 1;
                }
                seen
            })
        };
        for writer in writers {
            writer.join().unwrap();
        }
        assert!(reader.join().unwrap() > 0);
        assert!(payloads.contains(&fs::read_to_string(&path).unwrap()));
        let _ = fs::remove_dir_all(&dir);
    }

    /// A child process writes the same destination in a tight loop; the parent
    /// kills it at an arbitrary point. The destination must always be either
    /// the previous contents or one complete new payload.
    #[test]
    fn kill9_during_write_keeps_old_or_new() {
        const CHILD: &str = "FILER_ATOMIC_KILL9_CHILD";
        let payload = vec![b'n'; 1 << 20];

        if let Ok(path) = std::env::var(CHILD) {
            let path = PathBuf::from(path);
            loop {
                write(&path, &payload).unwrap();
            }
        }

        let dir = temp_dir("kill9");
        let path = dir.join("state.toml");
        write(&path, b"old").unwrap();

        for round in 0..8u64 {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg("atomic::tests::kill9_during_write_keeps_old_or_new")
                .arg("--nocapture")
                .env(CHILD, &path)
                .spawn()
                .unwrap();
            std::thread::sleep(Duration::from_millis(15 + round * 5));
            let _ = child.kill();
            let _ = child.wait();

            let bytes = fs::read(&path).unwrap();
            assert!(
                bytes == b"old" || bytes == payload,
                "round {round}: destination is neither the old nor the new contents ({} bytes)",
                bytes.len()
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
