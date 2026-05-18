//! Snapshot-aware atomic write primitive (slice 5.4).
//!
//! Every Phase-6 writer mutates host configuration through this single entry
//! point. The function:
//!
//! 1. Confirms the target's pre-edit state matches what was recorded at
//!    capture time (defends against concurrent edits between snapshot and
//!    write).
//! 2. Writes the new content via [`FileSystem::write_atomic`].
//! 3. Records the post-edit hash on the matching [`SnapshotEntry`] so
//!    rollback knows what bytes are on disk after the write.
//!
//! The legacy `plan_atomic_write` / `write_with_backup` primitive lived in
//! the same file but used a flat backup directory keyed by basename, which
//! collided on shared filenames (multiple `loader.conf`s, for example).
//! Capture (slice 5.3) now records each backup with the original absolute
//! path, and writers consume that record here.

use crate::snapshot::fs::FileSystem;
use crate::snapshot::manifest::{SnapshotEntry, SnapshotManifest};
use crate::snapshot::SnapshotError;

use sha2::{Digest, Sha256};
use std::path::Path;

/// Mutate `target` through a snapshot-aware atomic write.
///
/// `manifest` must already contain a [`SnapshotEntry`] for `target` (created
/// during capture). `snapshot_then_write` records the post-edit hash on that
/// entry; if the entry is missing, the call is rejected so callers cannot
/// silently bypass capture.
pub fn snapshot_then_write(
    manifest: &mut SnapshotManifest,
    filesystem: &impl FileSystem,
    target: &Path,
    new_content: &[u8],
) -> Result<(), SnapshotError> {
    // Verify the pre-edit hash still matches before mutating. If the file
    // has changed since capture (someone edited it externally), refuse to
    // write and surface the mismatch so the operator can investigate.
    if let Some(entry) = manifest.entry_for(target) {
        if entry.original_existed {
            let current = filesystem
                .read(target)
                .map_err(|source| SnapshotError::Io {
                    path: target.to_path_buf(),
                    source,
                })?;
            let actual = sha256_hex(&current);
            if actual != entry.pre_edit_sha256 {
                return Err(SnapshotError::HashMismatch {
                    id: manifest.id.clone(),
                    path: target.to_path_buf(),
                    expected: entry.pre_edit_sha256.clone(),
                    actual,
                });
            }
        }
    } else {
        return Err(SnapshotError::Io {
            path: target.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "snapshot manifest has no entry for {} - capture must run before snapshot_then_write",
                    target.display()
                ),
            ),
        });
    }

    filesystem
        .write_atomic(target, new_content)
        .map_err(|source| SnapshotError::Io {
            path: target.to_path_buf(),
            source,
        })?;

    let post_hash = sha256_hex(new_content);
    if let Some(entry) = manifest.entry_for_mut(target) {
        entry.record_post_edit_hash(post_hash);
    }

    Ok(())
}

/// Insert a fresh [`SnapshotEntry`] into the manifest for a path that did
/// not exist at capture time. Writers occasionally create new files (the
/// VFIO modprobe snippet, for example); they must declare that path here so
/// rollback can later remove it.
pub fn declare_created_entry(
    manifest: &mut SnapshotManifest,
    target: &Path,
    backup_path_in_manifest_dir: &Path,
    produced_by: crate::engine::step::StepKind,
) {
    if manifest.entry_for(target).is_some() {
        return;
    }
    manifest.push_entry(SnapshotEntry {
        original_path: target.to_path_buf(),
        backup_path: backup_path_in_manifest_dir.to_path_buf(),
        pre_edit_sha256: String::new(),
        post_edit_sha256: None,
        original_existed: false,
        produced_by,
    });
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::planner::PlanSummary;
    use crate::engine::step::{StepKind, StepRisk};
    use crate::snapshot::fs::MemoryFileSystem;
    use crate::snapshot::manifest::{HostSummary, SnapshotEntry};
    use std::path::PathBuf;

    fn empty_summary() -> PlanSummary {
        PlanSummary {
            total_steps: 0,
            pending_steps: 0,
            already_satisfied_steps: 0,
            max_risk: StepRisk::ReadOnly,
            requires_reboot: false,
            requires_confirmation: false,
        }
    }

    fn host_summary() -> HostSummary {
        HostSummary {
            distro_id: "arch".to_string(),
            distro_pretty_name: "Arch Linux".to_string(),
            kernel_version: "6.10.0".to_string(),
            bootloader: crate::detect::bootloader::BootloaderKind::Grub2,
            initramfs: crate::detect::initramfs::InitramfsSystem::Mkinitcpio,
        }
    }

    #[test]
    fn snapshot_then_write_records_post_hash() {
        let fs = MemoryFileSystem::new();
        fs.create_dir_all(Path::new("/etc")).unwrap();
        fs.write_atomic(Path::new("/etc/foo"), b"original").unwrap();

        let pre_hash = sha256_hex(b"original");
        let mut manifest =
            SnapshotManifest::new("id-1", host_summary(), empty_summary(), Vec::new());
        manifest.push_entry(SnapshotEntry {
            original_path: PathBuf::from("/etc/foo"),
            backup_path: PathBuf::from("files/_etc_foo"),
            pre_edit_sha256: pre_hash,
            post_edit_sha256: None,
            original_existed: true,
            produced_by: StepKind::VfioConfig,
        });

        snapshot_then_write(&mut manifest, &fs, Path::new("/etc/foo"), b"updated").unwrap();
        let entry = manifest.entry_for(Path::new("/etc/foo")).unwrap();
        assert_eq!(
            entry.post_edit_sha256.as_deref(),
            Some(sha256_hex(b"updated").as_str())
        );
        assert_eq!(fs.read(Path::new("/etc/foo")).unwrap(), b"updated");
    }

    #[test]
    fn snapshot_then_write_rejects_concurrent_external_edit() {
        let fs = MemoryFileSystem::new();
        fs.create_dir_all(Path::new("/etc")).unwrap();
        fs.write_atomic(Path::new("/etc/foo"), b"original").unwrap();

        let captured_hash = sha256_hex(b"original");
        let mut manifest =
            SnapshotManifest::new("id-2", host_summary(), empty_summary(), Vec::new());
        manifest.push_entry(SnapshotEntry {
            original_path: PathBuf::from("/etc/foo"),
            backup_path: PathBuf::from("files/_etc_foo"),
            pre_edit_sha256: captured_hash,
            post_edit_sha256: None,
            original_existed: true,
            produced_by: StepKind::VfioConfig,
        });

        // Simulate an external edit between capture and write.
        fs.write_atomic(Path::new("/etc/foo"), b"externally-changed")
            .unwrap();

        let err = snapshot_then_write(&mut manifest, &fs, Path::new("/etc/foo"), b"updated")
            .expect_err("must reject mismatched pre-hash");
        match err {
            SnapshotError::HashMismatch { .. } => {}
            other => panic!("expected HashMismatch, got {other:?}"),
        }
    }

    #[test]
    fn snapshot_then_write_rejects_missing_manifest_entry() {
        let fs = MemoryFileSystem::new();
        fs.create_dir_all(Path::new("/etc")).unwrap();
        let mut manifest =
            SnapshotManifest::new("id-3", host_summary(), empty_summary(), Vec::new());

        let err = snapshot_then_write(&mut manifest, &fs, Path::new("/etc/foo"), b"x")
            .expect_err("missing entry must error");
        match err {
            SnapshotError::Io { .. } => {}
            other => panic!("expected Io error, got {other:?}"),
        }
    }
}
