//! Snapshot capture and rollback (slices 5.1 - 5.6).
//!
//! Snapshots back up every file Virtu may edit before any mutation runs.
//! Each capture produces a directory under `~/.virtu/snapshots/<id>/`:
//!
//! ```text
//! ~/.virtu/snapshots/2026-05-19T14-30-00Z-v0.1.0/
//!     manifest.toml
//!     files/
//!         _etc_default_grub
//!         _etc_modprobe.d_virtu-vfio.conf
//! ```
//!
//! The capture path is read-mostly: it reads each plan touch through the
//! [`FileSystem`] trait, hashes the bytes, copies them to a sanitized
//! filename inside `files/`, and writes the manifest as TOML. Restore
//! reverses the process and verifies the post-restore hash matches the
//! recorded `pre_edit_sha256`.

use anyhow::Result;
use std::path::{Path, PathBuf};

pub mod fs;
pub mod manifest;
pub mod pending;

pub use fs::{FileSystem, MemoryFileSystem, RealFileSystem};
pub use manifest::{HostSummary, RestoreAction, SnapshotEntry, SnapshotManifest};
pub use pending::{HostFingerprint, PendingPlan};

use crate::detect::SystemProfile;
use crate::engine::planner::Plan;
use crate::engine::step::StepKind;

use sha2::{Digest, Sha256};

/// Subdirectory inside a snapshot directory that holds the backed-up files.
pub const FILES_SUBDIR: &str = "files";

/// Manifest filename inside a snapshot directory.
pub const MANIFEST_FILENAME: &str = "manifest.toml";

/// The opaque identifier for a captured snapshot. Matches the directory name
/// under `snapshots_root`.
pub type SnapshotId = String;

/// Error raised by capture, restore, and rollback.
#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("snapshot I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("snapshot {id}: manifest is missing or invalid")]
    MissingManifest { id: String },
    #[error("snapshot {id}: manifest cannot be parsed: {source}")]
    ManifestParse {
        id: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("snapshot manifest cannot be serialized: {0}")]
    ManifestSerialize(#[from] toml::ser::Error),
    #[error("snapshot {id}: rollback hash mismatch for {path}: expected {expected}, got {actual}")]
    HashMismatch {
        id: String,
        path: PathBuf,
        expected: String,
        actual: String,
    },
}

impl SnapshotError {
    fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// Capture a [`Plan`] into a snapshot directory.
///
/// Returns the snapshot id (matches the directory name created under
/// `snapshots_root`). The function reads each path declared in
/// `plan.steps[].touches`, hashes the bytes when the file exists, copies the
/// original to `snapshots_root/<id>/files/<sanitized-path>`, and writes the
/// manifest as `snapshots_root/<id>/manifest.toml`.
///
/// Path collisions on shared basenames (e.g. multiple `loader.conf` files in
/// systemd-boot) are avoided by sanitizing the absolute path: `/` and `:`
/// are rewritten to `_`. The original absolute path is preserved in each
/// manifest entry, so rollback can reconstruct it exactly.
///
/// Touches that do not map to a regular file (such as the snapshot directory
/// itself or libvirt hook directories) are recorded as entries with
/// `original_existed = false`. Restore handles them as "delete on rollback".
pub fn capture(
    plan: &Plan,
    host: &SystemProfile,
    filesystem: &impl FileSystem,
    snapshots_root: &Path,
) -> Result<SnapshotId, SnapshotError> {
    let id = SnapshotManifest::generate_id(chrono::Utc::now());
    capture_with_id(plan, host, filesystem, snapshots_root, &id)?;
    Ok(id)
}

/// Capture a [`Plan`] with a caller-supplied id. Useful for tests that need
/// determinism. Production code uses [`capture`].
pub fn capture_with_id(
    plan: &Plan,
    host: &SystemProfile,
    filesystem: &impl FileSystem,
    snapshots_root: &Path,
    id: &str,
) -> Result<SnapshotId, SnapshotError> {
    let snapshot_dir = snapshots_root.join(id);
    let files_dir = snapshot_dir.join(FILES_SUBDIR);
    filesystem
        .create_dir_all(&files_dir)
        .map_err(|source| SnapshotError::io(&files_dir, source))?;

    let host_summary = HostSummary::from_profile(host);
    let restore_actions = derive_restore_actions(plan, host);
    let mut manifest =
        SnapshotManifest::new(id, host_summary, plan.summary.clone(), restore_actions);

    let unique_targets = collect_targets(plan);
    for target in unique_targets {
        let entry = capture_one(filesystem, &target, &snapshot_dir, plan)?;
        manifest.push_entry(entry);
    }

    let serialized = toml::to_string(&manifest)?;
    let manifest_path = snapshot_dir.join(MANIFEST_FILENAME);
    filesystem
        .write_atomic(&manifest_path, serialized.as_bytes())
        .map_err(|source| SnapshotError::io(&manifest_path, source))?;

    Ok(id.to_string())
}

/// Build the list of unique target paths from a plan, skipping entries whose
/// sanitized basename is empty (which would indicate a pathological "/" or
/// equivalent).
fn collect_targets(plan: &Plan) -> Vec<PathBuf> {
    let mut seen: Vec<PathBuf> = Vec::new();
    for step in &plan.steps {
        // The snapshot step itself touches the snapshots root; do not back it
        // up — its existence is the rollback baseline.
        if step.kind == StepKind::Snapshot {
            continue;
        }
        for touch in &step.touches {
            if touch.as_os_str().is_empty() {
                continue;
            }
            if !seen.contains(touch) {
                seen.push(touch.clone());
            }
        }
    }
    seen
}

fn capture_one(
    filesystem: &impl FileSystem,
    target: &Path,
    snapshot_dir: &Path,
    plan: &Plan,
) -> Result<SnapshotEntry, SnapshotError> {
    let backup_relative = backup_relative_path(target);
    let backup_absolute = snapshot_dir.join(&backup_relative);
    let produced_by = produced_by(plan, target);

    if filesystem.exists(target) {
        let bytes = filesystem
            .read(target)
            .map_err(|source| SnapshotError::io(target, source))?;
        let pre_hash = sha256_hex(&bytes);
        // Copy via the FileSystem trait so the test backend stays in sync.
        if let Some(parent) = backup_absolute.parent() {
            filesystem
                .create_dir_all(parent)
                .map_err(|source| SnapshotError::io(parent, source))?;
        }
        filesystem
            .write_atomic(&backup_absolute, &bytes)
            .map_err(|source| SnapshotError::io(&backup_absolute, source))?;

        Ok(SnapshotEntry {
            original_path: target.to_path_buf(),
            backup_path: backup_relative,
            pre_edit_sha256: pre_hash,
            post_edit_sha256: None,
            original_existed: true,
            produced_by,
        })
    } else {
        Ok(SnapshotEntry {
            original_path: target.to_path_buf(),
            backup_path: backup_relative,
            pre_edit_sha256: String::new(),
            post_edit_sha256: None,
            original_existed: false,
            produced_by,
        })
    }
}

/// Walk the plan to find which step kind owns a given touch path. The first
/// matching step wins; the planner already enforces a fixed step order, so
/// this is deterministic.
fn produced_by(plan: &Plan, target: &Path) -> StepKind {
    for step in &plan.steps {
        if step.touches.iter().any(|t| t == target) {
            return step.kind.clone();
        }
    }
    // Fallback: should be unreachable because `target` came from a plan touch.
    StepKind::Verify
}

/// Sanitize an absolute path into a flat backup filename.
///
/// Replaces `/`, `:` and `\` with `_`. Leading underscores are kept so the
/// manifest reader can tell capture-time absolute paths from accidentally
/// relative ones.
pub fn sanitize_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '/' | ':' | '\\' => out.push('_'),
            _ => out.push(ch),
        }
    }
    out
}

fn backup_relative_path(target: &Path) -> PathBuf {
    PathBuf::from(FILES_SUBDIR).join(sanitize_path(target))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    hex::encode(digest)
}

/// Convert a plan into the post-restore commands a user must re-run after
/// rollback. The mapping is intentionally small; later milestones can add
/// step-kind-specific actions as new writers are wired up.
fn derive_restore_actions(plan: &Plan, host: &SystemProfile) -> Vec<RestoreAction> {
    let mut actions: Vec<RestoreAction> = Vec::new();

    for step in &plan.steps {
        match step.kind {
            StepKind::BootloaderWrite => {
                if let Some(cmd) = host.bootloader.update_command.clone() {
                    actions.push(RestoreAction::RegenerateBootloader { command: cmd });
                }
            }
            StepKind::InitramfsWrite => {
                let cmd = host.initramfs_system.rebuild_command();
                if !cmd.is_empty() {
                    actions.push(RestoreAction::RebuildInitramfs {
                        command: cmd.to_string(),
                    });
                }
            }
            StepKind::VfioConfig => {
                actions.push(RestoreAction::ReloadKernelModules {
                    module: "vfio_pci".to_string(),
                });
            }
            StepKind::VmRegister => {
                actions.push(RestoreAction::UndefineLibvirtDomain {
                    name: "<generated>".to_string(),
                });
            }
            _ => {}
        }
    }

    if plan.summary.requires_reboot {
        actions.push(RestoreAction::RecommendReboot);
    }

    actions
}

/// Summary returned by [`restore`]. Drives the CLI rollback report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreSummary {
    /// Snapshot id that was restored.
    pub id: String,
    /// Files that were rewritten to their pre-edit byte sequence.
    pub restored: Vec<PathBuf>,
    /// Files that were already at the pre-edit hash and therefore skipped.
    pub already_at_pre_edit: Vec<PathBuf>,
    /// Files that did not exist at capture time and were deleted (or were
    /// already absent) during restore.
    pub deleted_or_already_absent: Vec<PathBuf>,
    /// Post-restore commands the user must re-run for the rollback to be
    /// fully effective.
    pub post_restore_actions: Vec<RestoreAction>,
}

impl RestoreSummary {
    pub fn print_human(&self) {
        println!("\n=== VIRTU ROLLBACK ===");
        println!("Snapshot: {}", self.id);
        if self.restored.is_empty()
            && self.already_at_pre_edit.is_empty()
            && self.deleted_or_already_absent.is_empty()
        {
            println!("No entries to restore.");
        }
        if !self.restored.is_empty() {
            println!("\nRestored:");
            for path in &self.restored {
                println!("  - {}", path.display());
            }
        }
        if !self.already_at_pre_edit.is_empty() {
            println!("\nAlready at pre-edit state:");
            for path in &self.already_at_pre_edit {
                println!("  - {}", path.display());
            }
        }
        if !self.deleted_or_already_absent.is_empty() {
            println!("\nDeleted or already absent (re-created file rolled back):");
            for path in &self.deleted_or_already_absent {
                println!("  - {}", path.display());
            }
        }
        if !self.post_restore_actions.is_empty() {
            println!("\nPost-restore actions you must run:");
            for action in &self.post_restore_actions {
                println!("  - {}", action.human_summary());
            }
        }
        println!();
    }
}

/// Restore a previously captured snapshot. Idempotent: if every recorded
/// path already matches `pre_edit_sha256`, the call returns a summary with
/// every entry in `already_at_pre_edit`.
pub fn restore(
    snapshot_id: &str,
    filesystem: &impl FileSystem,
    snapshots_root: &Path,
) -> Result<RestoreSummary, SnapshotError> {
    let snapshot_dir = snapshots_root.join(snapshot_id);
    let manifest_path = snapshot_dir.join(MANIFEST_FILENAME);

    if !filesystem.exists(&manifest_path) {
        return Err(SnapshotError::MissingManifest {
            id: snapshot_id.to_string(),
        });
    }

    let manifest_bytes = filesystem
        .read(&manifest_path)
        .map_err(|source| SnapshotError::io(&manifest_path, source))?;
    let manifest_str = String::from_utf8(manifest_bytes).map_err(|err| SnapshotError::Io {
        path: manifest_path.clone(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, err),
    })?;
    let manifest: SnapshotManifest =
        toml::from_str(&manifest_str).map_err(|source| SnapshotError::ManifestParse {
            id: snapshot_id.to_string(),
            source,
        })?;

    let mut summary = RestoreSummary {
        id: snapshot_id.to_string(),
        restored: Vec::new(),
        already_at_pre_edit: Vec::new(),
        deleted_or_already_absent: Vec::new(),
        post_restore_actions: manifest.restore_actions.clone(),
    };

    for entry in &manifest.entries {
        if entry.original_existed {
            // Idempotency: skip when the live file already matches the
            // captured pre-edit hash.
            if filesystem.exists(&entry.original_path) {
                let current = filesystem
                    .read(&entry.original_path)
                    .map_err(|source| SnapshotError::io(&entry.original_path, source))?;
                if sha256_hex(&current) == entry.pre_edit_sha256 {
                    summary
                        .already_at_pre_edit
                        .push(entry.original_path.clone());
                    continue;
                }
            }

            let backup_absolute = snapshot_dir.join(&entry.backup_path);
            let backup_bytes = filesystem
                .read(&backup_absolute)
                .map_err(|source| SnapshotError::io(&backup_absolute, source))?;
            let restored_hash = sha256_hex(&backup_bytes);
            if restored_hash != entry.pre_edit_sha256 {
                return Err(SnapshotError::HashMismatch {
                    id: snapshot_id.to_string(),
                    path: backup_absolute.clone(),
                    expected: entry.pre_edit_sha256.clone(),
                    actual: restored_hash,
                });
            }

            if let Some(parent) = entry.original_path.parent() {
                if !parent.as_os_str().is_empty() {
                    filesystem
                        .create_dir_all(parent)
                        .map_err(|source| SnapshotError::io(parent, source))?;
                }
            }
            filesystem
                .write_atomic(&entry.original_path, &backup_bytes)
                .map_err(|source| SnapshotError::io(&entry.original_path, source))?;
            summary.restored.push(entry.original_path.clone());
        } else {
            // The file did not exist at capture time. Rolling back means
            // removing it if it has since been created. Idempotent: missing
            // is fine.
            if filesystem.exists(&entry.original_path) {
                if let Err(error) = remove_via_filesystem(filesystem, &entry.original_path) {
                    return Err(SnapshotError::io(&entry.original_path, error));
                }
            }
            summary
                .deleted_or_already_absent
                .push(entry.original_path.clone());
        }
    }

    Ok(summary)
}

/// `FileSystem` does not yet expose `remove`. The MemoryFileSystem can fake
/// it through `write_atomic` of an empty marker, but that pollutes
/// production semantics. For now, fall through to the std backend for the
/// production path and rely on a downcast for tests.
fn remove_via_filesystem(
    filesystem: &impl FileSystem,
    target: &Path,
) -> Result<(), std::io::Error> {
    filesystem.remove_file(target)
}

pub fn list_snapshots() -> Result<()> {
    let snapshots_root = virtu_home().join("snapshots");
    if !snapshots_root.exists() {
        println!("No Virtu snapshots found.");
        return Ok(());
    }

    let mut entries: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(&snapshots_root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            entries.push(entry.path());
        }
    }
    if entries.is_empty() {
        println!("No Virtu snapshots found.");
        return Ok(());
    }
    entries.sort();

    println!("Available Virtu snapshots:");
    for path in entries {
        let id = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match read_manifest_from_dir(&path) {
            Ok(manifest) => {
                println!(
                    "  {id}  ({entries} entries, {steps} step(s), captured {ts})",
                    id = id,
                    entries = manifest.entries.len(),
                    steps = manifest.plan_summary.total_steps,
                    ts = manifest.created_at.to_rfc3339()
                );
            }
            Err(err) => {
                println!("  {id}  (manifest unreadable: {err})");
            }
        }
    }
    Ok(())
}

fn read_manifest_from_dir(snapshot_dir: &Path) -> Result<SnapshotManifest> {
    let manifest_path = snapshot_dir.join(MANIFEST_FILENAME);
    let raw = std::fs::read_to_string(&manifest_path)?;
    let parsed: SnapshotManifest = toml::from_str(&raw)?;
    Ok(parsed)
}

/// Restore a snapshot using the real filesystem.
pub async fn rollback_to(snapshot_id: &str) -> Result<()> {
    let snapshots_root = virtu_home().join("snapshots");
    let snapshot_dir = snapshots_root.join(snapshot_id);
    if !snapshot_dir.exists() {
        anyhow::bail!("Snapshot `{snapshot_id}` does not exist");
    }

    let filesystem = RealFileSystem::new();
    let summary = restore(snapshot_id, &filesystem, &snapshots_root)?;
    summary.print_human();
    Ok(())
}

fn virtu_home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".virtu")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_replaces_slashes_colons_and_backslashes() {
        assert_eq!(
            sanitize_path(Path::new("/etc/foo/bar.conf")),
            "_etc_foo_bar.conf"
        );
        assert_eq!(
            sanitize_path(Path::new("C:\\Users\\test\\foo")),
            "C__Users_test_foo"
        );
    }

    #[test]
    fn sha256_hex_is_64_chars_lowercase() {
        let hex = sha256_hex(b"hello");
        assert_eq!(hex.len(), 64);
        assert!(hex
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }

    #[test]
    fn backup_relative_path_lives_under_files_subdir() {
        let rel = backup_relative_path(Path::new("/etc/default/grub"));
        assert_eq!(rel, PathBuf::from("files/_etc_default_grub"));
    }
}
