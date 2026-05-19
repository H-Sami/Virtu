//! Snapshot manifest schema (slice 5.1).
//!
//! A [`SnapshotManifest`] is the persisted record of a Virtu plan execution
//! attempt. It is captured before any mutation runs (see [`Snapshot::capture`]
//! in `src/snapshot/mod.rs`), updated as each writer finishes (via
//! [`SnapshotEntry::record_post_edit_hash`]), and consumed by the rollback
//! path to undo every recorded change.
//!
//! The manifest is persisted as TOML so it remains human-inspectable next to
//! the backed-up files. Every public field is `Serialize + Deserialize`.
//!
//! This module is intentionally pure: it does not touch the filesystem and
//! does not run commands. Filesystem and capture/restore behavior live in
//! sibling modules (slices 5.2 and 5.3).

use crate::detect::bootloader::BootloaderKind;
use crate::detect::initramfs::InitramfsSystem;
use crate::detect::SystemProfile;
use crate::engine::planner::PlanSummary;
use crate::engine::step::StepKind;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The persisted snapshot record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotManifest {
    /// Virtu version that produced this snapshot.
    pub virtu_version: String,
    /// When the snapshot was captured (UTC, RFC 3339).
    pub created_at: DateTime<Utc>,
    /// Snapshot identifier. Matches the directory name under
    /// `~/.virtu/snapshots/`. Format is `<rfc3339>-v<version>` with `:` and
    /// `+` rewritten to `-` so it is filesystem-safe on every host.
    pub id: String,
    /// Subset of [`SystemProfile`] needed to reason about a snapshot after
    /// the host has changed.
    pub host_summary: HostSummary,
    /// Plan summary. Matches the [`PlanSummary`] produced by
    /// [`crate::engine::planner::plan`].
    pub plan_summary: PlanSummary,
    /// One entry per file Virtu may edit. Populated at capture time and
    /// updated by writers as they finish.
    pub entries: Vec<SnapshotEntry>,
    /// Post-restore commands the user must re-run (initramfs rebuild,
    /// bootloader update, ...). Derived from the plan's step kinds at capture
    /// time.
    pub restore_actions: Vec<RestoreAction>,
}

/// Subset of [`SystemProfile`] embedded in a manifest. Keeping the snapshot
/// independent from the full profile means an old manifest can be restored
/// even after detection logic has evolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSummary {
    pub distro_id: String,
    pub distro_pretty_name: String,
    pub kernel_version: String,
    pub bootloader: BootloaderKind,
    pub initramfs: InitramfsSystem,
}

impl HostSummary {
    /// Derive a [`HostSummary`] from a [`SystemProfile`] without copying
    /// anything else.
    pub fn from_profile(profile: &SystemProfile) -> Self {
        Self {
            distro_id: profile.distro.id.clone(),
            distro_pretty_name: profile.distro.pretty_name.clone(),
            kernel_version: profile.readiness.kernel_version.clone(),
            bootloader: profile.bootloader.kind.clone(),
            initramfs: profile.initramfs_system.clone(),
        }
    }
}

/// One entry per file Virtu may touch during a plan execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotEntry {
    /// Absolute path of the original file on the host.
    pub original_path: PathBuf,
    /// Path of the backup copy, *relative* to the manifest directory. Storing
    /// it relative keeps the snapshot directory portable.
    pub backup_path: PathBuf,
    /// Hash of the file contents at capture time. Hex-encoded SHA-256.
    pub pre_edit_sha256: String,
    /// Hash of the file contents after the matching writer finished. `None`
    /// means the writer for this entry has not run yet (or this entry covers
    /// a file that did not exist at capture time and was never created).
    pub post_edit_sha256: Option<String>,
    /// `true` if `original_path` did not exist when the snapshot was
    /// captured. The rollback for such an entry is to delete the file rather
    /// than restore the (nonexistent) original.
    pub original_existed: bool,
    /// Which planner step kind produced this entry. Used to decide which
    /// post-restore command must run after rollback.
    pub produced_by: StepKind,
}

impl SnapshotEntry {
    /// Record the post-edit hash. Called by [`crate::config::atomic_write`]
    /// after a successful atomic write.
    pub fn record_post_edit_hash(&mut self, hash: impl Into<String>) {
        self.post_edit_sha256 = Some(hash.into());
    }
}

/// A command (or family of commands) the user must re-run after rollback.
///
/// Modelled as an enum keyed off [`StepKind`] so the executor can derive the
/// list deterministically from a [`Plan`] without re-running detection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RestoreAction {
    /// Re-run the bootloader's regenerate command (e.g. `grub-mkconfig
    /// -o /boot/grub/grub.cfg`). The exact command depends on the host's
    /// bootloader, so it is captured here at snapshot time.
    RegenerateBootloader { command: String },
    /// Rebuild the initramfs image with the host's tool (`mkinitcpio -P`,
    /// `dracut --force`, `update-initramfs -u -k all`, ...).
    RebuildInitramfs { command: String },
    /// Reload kernel modules so the modprobe snippet takes effect without a
    /// reboot. Optional: skipping it just means the user must reboot.
    ReloadKernelModules { module: String },
    /// Undefine a libvirt domain that was created by the plan. The argument
    /// is the libvirt domain name (`PassthroughConfig::vm_name`). Phase B
    /// appends this restore action to the manifest only after `virsh
    /// define` succeeds.
    UndefineLibvirtDomain { name: String },
    /// Inform the user that the host must reboot for the rollback to be
    /// fully effective. Carries no command because reboots cannot be
    /// automated safely from inside Virtu.
    RecommendReboot,
}

impl RestoreAction {
    /// Plain-language summary suitable for the CLI rollback report.
    pub fn human_summary(&self) -> String {
        match self {
            RestoreAction::RegenerateBootloader { command } => {
                format!("Re-run bootloader update: {command}")
            }
            RestoreAction::RebuildInitramfs { command } => {
                format!("Rebuild initramfs: {command}")
            }
            RestoreAction::ReloadKernelModules { module } => {
                format!("Reload kernel module: modprobe -r {module} && modprobe {module}")
            }
            RestoreAction::UndefineLibvirtDomain { name } => {
                format!("Undefine libvirt domain: virsh undefine {name}")
            }
            RestoreAction::RecommendReboot => "Reboot to fully apply the rollback.".to_string(),
        }
    }
}

impl SnapshotManifest {
    /// Construct an empty manifest ready to receive entries.
    ///
    /// `id` is the snapshot identifier and matches the directory name under
    /// `~/.virtu/snapshots/`. Production callers should use
    /// [`SnapshotManifest::generate_id`] to build it.
    pub fn new(
        id: impl Into<String>,
        host_summary: HostSummary,
        plan_summary: PlanSummary,
        restore_actions: Vec<RestoreAction>,
    ) -> Self {
        Self {
            virtu_version: env!("CARGO_PKG_VERSION").to_string(),
            created_at: Utc::now(),
            id: id.into(),
            host_summary,
            plan_summary,
            entries: Vec::new(),
            restore_actions,
        }
    }

    /// Derive a filesystem-safe snapshot id from a [`DateTime`] and the
    /// current Virtu version. Sub-second precision is preserved (3 digits)
    /// so two captures within the same second still get distinct ids.
    ///
    /// ```text
    /// 2026-05-19T14-30-00-123Z-v0.1.0
    /// ```
    pub fn generate_id(now: DateTime<Utc>) -> String {
        let stamp = now.format("%Y-%m-%dT%H-%M-%S-%3fZ").to_string();
        format!("{stamp}-v{ver}", ver = env!("CARGO_PKG_VERSION"))
    }

    /// Push an entry. Used by the capture and write paths.
    pub fn push_entry(&mut self, entry: SnapshotEntry) {
        self.entries.push(entry);
    }

    /// Find the entry for a given target path, if any. Used by writers that
    /// need to update the post-edit hash after a successful atomic write.
    pub fn entry_for_mut(&mut self, target: &std::path::Path) -> Option<&mut SnapshotEntry> {
        self.entries
            .iter_mut()
            .find(|entry| entry.original_path == target)
    }

    /// Find the entry for a given target path, if any. Read-only counterpart
    /// to [`Self::entry_for_mut`].
    pub fn entry_for(&self, target: &std::path::Path) -> Option<&SnapshotEntry> {
        self.entries
            .iter()
            .find(|entry| entry.original_path == target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_plan_summary() -> PlanSummary {
        PlanSummary {
            total_steps: 0,
            pending_steps: 0,
            already_satisfied_steps: 0,
            max_risk: crate::engine::step::StepRisk::ReadOnly,
            requires_reboot: false,
            requires_confirmation: false,
        }
    }

    fn dummy_host_summary() -> HostSummary {
        HostSummary {
            distro_id: "arch".to_string(),
            distro_pretty_name: "Arch Linux".to_string(),
            kernel_version: "6.10.0".to_string(),
            bootloader: BootloaderKind::Grub2,
            initramfs: InitramfsSystem::Mkinitcpio,
        }
    }

    #[test]
    fn generate_id_is_filesystem_safe_and_pinned_to_version() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-05-19T14:30:00.123Z")
            .expect("fixed timestamp parses")
            .with_timezone(&Utc);
        let id = SnapshotManifest::generate_id(now);
        assert!(!id.contains(':'));
        assert!(!id.contains('+'));
        assert!(id.starts_with("2026-05-19T14-30-00-123Z-v"));
        assert!(id.ends_with(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn manifest_round_trips_through_toml() {
        let mut manifest = SnapshotManifest::new(
            "2026-05-19T14-30-00Z-v0.1.0",
            dummy_host_summary(),
            dummy_plan_summary(),
            vec![
                RestoreAction::RegenerateBootloader {
                    command: "grub-mkconfig -o /boot/grub/grub.cfg".to_string(),
                },
                RestoreAction::RebuildInitramfs {
                    command: "mkinitcpio -P".to_string(),
                },
                RestoreAction::RecommendReboot,
            ],
        );

        manifest.push_entry(SnapshotEntry {
            original_path: PathBuf::from("/etc/default/grub"),
            backup_path: PathBuf::from("files/_etc_default_grub"),
            pre_edit_sha256: "a".repeat(64),
            post_edit_sha256: Some("b".repeat(64)),
            original_existed: true,
            produced_by: StepKind::BootloaderWrite,
        });

        let serialized = toml::to_string(&manifest).expect("serialize");
        let parsed: SnapshotManifest = toml::from_str(&serialized).expect("parse");
        assert_eq!(parsed, manifest);
    }

    #[test]
    fn entry_lookup_finds_entry_by_original_path() {
        let mut manifest =
            SnapshotManifest::new("id", dummy_host_summary(), dummy_plan_summary(), Vec::new());

        manifest.push_entry(SnapshotEntry {
            original_path: PathBuf::from("/etc/foo/bar.conf"),
            backup_path: PathBuf::from("files/_etc_foo_bar.conf"),
            pre_edit_sha256: "0".repeat(64),
            post_edit_sha256: None,
            original_existed: true,
            produced_by: StepKind::VfioConfig,
        });

        let entry = manifest
            .entry_for(std::path::Path::new("/etc/foo/bar.conf"))
            .expect("entry exists");
        assert_eq!(entry.pre_edit_sha256, "0".repeat(64));

        manifest
            .entry_for_mut(std::path::Path::new("/etc/foo/bar.conf"))
            .expect("entry exists for mut")
            .record_post_edit_hash("c".repeat(64));

        let entry = manifest
            .entry_for(std::path::Path::new("/etc/foo/bar.conf"))
            .expect("entry still there");
        assert_eq!(
            entry.post_edit_sha256.as_deref(),
            Some("c".repeat(64).as_str())
        );
    }

    #[test]
    fn restore_action_human_summary_includes_command() {
        let action = RestoreAction::RegenerateBootloader {
            command: "grub-mkconfig -o /boot/grub/grub.cfg".to_string(),
        };
        assert!(action
            .human_summary()
            .contains("grub-mkconfig -o /boot/grub/grub.cfg"));
    }
}
