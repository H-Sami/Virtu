//! Filesystem abstraction for snapshot capture and restore (slice 5.2).
//!
//! The snapshot subsystem must stay testable without touching the real host
//! filesystem (Hard Rule: "no `cargo test` may touch the real host
//! filesystem"). Every capture, write, and restore goes through the
//! [`FileSystem`] trait. Production code uses [`RealFileSystem`]; tests use
//! [`MemoryFileSystem`].
//!
//! The trait is intentionally minimal. Higher-level concepts like atomic
//! writes are built *on top* of [`FileSystem::write_atomic`] and exposed in
//! `src/config/atomic_write.rs` (slice 5.4).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Storage primitives the snapshot subsystem relies on.
pub trait FileSystem {
    /// Read the entire contents of `path`.
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;

    /// Write `content` to `path` atomically. Atomicity for the production
    /// implementation is "tempfile + fsync + rename" within `path`'s parent
    /// directory. The parent directory must already exist.
    fn write_atomic(&self, path: &Path, content: &[u8]) -> io::Result<()>;

    /// Copy a single regular file from `from` to `to`. The destination's
    /// parent directory must already exist. Implementations must guarantee
    /// that the destination is left in a consistent state (either fully
    /// copied or absent).
    fn copy(&self, from: &Path, to: &Path) -> io::Result<()>;

    /// Returns `true` if `path` exists and is reachable.
    fn exists(&self, path: &Path) -> bool;

    /// Create `path` and any of its missing parent directories.
    fn create_dir_all(&self, path: &Path) -> io::Result<()>;

    /// Remove a regular file. Returns [`io::ErrorKind::NotFound`] if the
    /// file was already absent.
    fn remove_file(&self, path: &Path) -> io::Result<()>;

    /// Mark `path` as executable for owner / group / other. Used by the
    /// single-GPU hook installer (slice 9.3) so libvirt can actually run
    /// the generated dispatcher and helper scripts. The file at `path`
    /// must exist; missing files return [`io::ErrorKind::NotFound`].
    ///
    /// On Unix this is `chmod 0o755`. On non-Unix targets this is a
    /// no-op (the trait still returns `Ok(())` so callers don't have to
    /// branch on platform); production use is Linux-only.
    fn set_executable(&self, path: &Path) -> io::Result<()>;
}

/// Production [`FileSystem`] backed by `std::fs` and [`tempfile`].
///
/// `write_atomic` writes through a sibling temp file, calls `flush` to ensure
/// the new bytes are queued for the kernel, and uses
/// [`tempfile::NamedTempFile::persist`] to perform the rename. Where
/// available, `fsync` is invoked on the temp file before the rename.
#[derive(Debug, Default)]
pub struct RealFileSystem;

impl RealFileSystem {
    pub fn new() -> Self {
        Self
    }
}

impl FileSystem for RealFileSystem {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }

    fn write_atomic(&self, path: &Path, content: &[u8]) -> io::Result<()> {
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("target {} has no parent directory", path.display()),
            )
        })?;
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }

        let temp_dir = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        let mut temp = tempfile::NamedTempFile::new_in(temp_dir)?;
        use std::io::Write;
        temp.write_all(content)?;
        temp.flush()?;
        if let Err(error) = temp.as_file().sync_all() {
            // sync_all can fail on filesystems that do not support it (e.g.
            // certain network mounts). The atomic-rename below is still our
            // primary durability guarantee, so a sync failure is logged at
            // debug level and not propagated.
            tracing::debug!("sync_all on snapshot temp file failed: {error}");
        }
        temp.persist(path)
            .map_err(|persist_error| persist_error.error)?;
        Ok(())
    }

    fn copy(&self, from: &Path, to: &Path) -> io::Result<()> {
        if let Some(parent) = to.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::copy(from, to).map(|_| ())
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        std::fs::create_dir_all(path)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_file(path)
    }

    fn set_executable(&self, path: &Path) -> io::Result<()> {
        if !path.exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("set_executable: {} does not exist", path.display()),
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(path)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(path, perms)?;
        }
        #[cfg(not(unix))]
        {
            // No-op on non-Unix; production is Linux-only.
            let _ = path;
        }
        Ok(())
    }
}

/// In-memory [`FileSystem`] for tests. Stores file contents in a sorted map
/// keyed by absolute path, and tracks created directories so `exists` can
/// answer for them. Operations are interior-mutable so a single
/// `MemoryFileSystem` value can be shared across helpers in a test.
#[derive(Debug, Default)]
pub struct MemoryFileSystem {
    state: Mutex<MemoryFsState>,
}

#[derive(Debug, Default)]
struct MemoryFsState {
    files: std::collections::BTreeMap<PathBuf, Vec<u8>>,
    dirs: std::collections::BTreeSet<PathBuf>,
    executable: std::collections::BTreeSet<PathBuf>,
}

impl MemoryFileSystem {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed a file. Convenience for test setup.
    pub fn seed_file(&self, path: impl Into<PathBuf>, content: impl Into<Vec<u8>>) {
        let path = path.into();
        let content = content.into();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Self::ensure_parent_dirs(&mut state, &path);
        state.files.insert(path, content);
    }

    /// Read a file's contents directly. Returns `None` if the file is
    /// missing; callers in tests can `unwrap` if they know what they wrote.
    pub fn read_file(&self, path: impl AsRef<Path>) -> Option<Vec<u8>> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.files.get(path.as_ref()).cloned()
    }

    /// Test helper: list every absolute file path currently held.
    pub fn list_files(&self) -> Vec<PathBuf> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.files.keys().cloned().collect()
    }

    fn ensure_parent_dirs(state: &mut MemoryFsState, path: &Path) {
        let mut current = path.parent();
        while let Some(parent) = current {
            if parent.as_os_str().is_empty() {
                break;
            }
            state.dirs.insert(parent.to_path_buf());
            current = parent.parent();
        }
    }
}

impl FileSystem for MemoryFileSystem {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("memory fs mutex poisoned"))?;
        match state.files.get(path) {
            Some(content) => Ok(content.clone()),
            None => Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("memory fs: no file at {}", path.display()),
            )),
        }
    }

    fn write_atomic(&self, path: &Path, content: &[u8]) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("memory fs mutex poisoned"))?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() && !state.dirs.contains(parent) {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("memory fs: parent dir missing for {}", path.display()),
                ));
            }
        }
        state.files.insert(path.to_path_buf(), content.to_vec());
        Ok(())
    }

    fn copy(&self, from: &Path, to: &Path) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("memory fs mutex poisoned"))?;
        let content = state.files.get(from).cloned().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("memory fs: source missing for copy: {}", from.display()),
            )
        })?;
        if let Some(parent) = to.parent() {
            if !parent.as_os_str().is_empty() && !state.dirs.contains(parent) {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("memory fs: parent dir missing for {}", to.display()),
                ));
            }
        }
        state.files.insert(to.to_path_buf(), content);
        Ok(())
    }

    fn exists(&self, path: &Path) -> bool {
        let state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => return false,
        };
        state.files.contains_key(path) || state.dirs.contains(path)
    }

    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("memory fs mutex poisoned"))?;
        let mut current: Option<&Path> = Some(path);
        while let Some(p) = current {
            if p.as_os_str().is_empty() {
                break;
            }
            state.dirs.insert(p.to_path_buf());
            current = p.parent();
        }
        Ok(())
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("memory fs mutex poisoned"))?;
        if state.files.remove(path).is_none() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("memory fs: no file at {}", path.display()),
            ));
        }
        state.executable.remove(path);
        Ok(())
    }

    fn set_executable(&self, path: &Path) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("memory fs mutex poisoned"))?;
        if !state.files.contains_key(path) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "memory fs: cannot mark missing file executable: {}",
                    path.display()
                ),
            ));
        }
        state.executable.insert(path.to_path_buf());
        Ok(())
    }
}

impl MemoryFileSystem {
    /// Test helper: returns `true` if `path` was marked executable via
    /// `set_executable`.
    pub fn is_executable(&self, path: impl AsRef<Path>) -> bool {
        let state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => return false,
        };
        state.executable.contains(path.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_fs_round_trips_a_file() {
        let fs = MemoryFileSystem::new();
        fs.create_dir_all(Path::new("/etc/foo")).unwrap();
        fs.write_atomic(Path::new("/etc/foo/bar.conf"), b"hello")
            .unwrap();
        assert!(fs.exists(Path::new("/etc/foo/bar.conf")));
        assert_eq!(fs.read(Path::new("/etc/foo/bar.conf")).unwrap(), b"hello");
    }

    #[test]
    fn memory_fs_seed_creates_parent_dirs() {
        let fs = MemoryFileSystem::new();
        fs.seed_file("/etc/foo/bar.conf", b"original".to_vec());
        assert!(fs.exists(Path::new("/etc/foo")));
        assert!(fs.exists(Path::new("/etc/foo/bar.conf")));
    }

    #[test]
    fn memory_fs_copy_preserves_contents_and_keeps_source() {
        let fs = MemoryFileSystem::new();
        fs.seed_file("/etc/foo/bar.conf", b"original".to_vec());
        fs.create_dir_all(Path::new("/snapshot/files")).unwrap();
        fs.copy(
            Path::new("/etc/foo/bar.conf"),
            Path::new("/snapshot/files/_etc_foo_bar.conf"),
        )
        .unwrap();
        assert_eq!(
            fs.read(Path::new("/etc/foo/bar.conf")).unwrap(),
            b"original"
        );
        assert_eq!(
            fs.read(Path::new("/snapshot/files/_etc_foo_bar.conf"))
                .unwrap(),
            b"original"
        );
    }

    #[test]
    fn memory_fs_read_missing_file_returns_not_found() {
        let fs = MemoryFileSystem::new();
        let err = fs.read(Path::new("/etc/missing")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn memory_fs_write_atomic_requires_parent_dir() {
        let fs = MemoryFileSystem::new();
        let err = fs
            .write_atomic(Path::new("/no/such/dir/file"), b"x")
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn real_fs_atomic_write_replaces_an_existing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("target.txt");
        std::fs::write(&target, b"original").unwrap();

        let fs = RealFileSystem::new();
        fs.write_atomic(&target, b"replaced").unwrap();

        let contents = std::fs::read(&target).unwrap();
        assert_eq!(contents, b"replaced");
    }
}
