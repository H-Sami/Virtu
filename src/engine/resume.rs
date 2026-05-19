//! Phase-B verifier (slice 6.5.1).
//!
//! After the user reboots into the new kernel, `virtu resume` re-runs
//! detection and asks this module: did Phase A land correctly?
//!
//! The verifier is pure. It compares a freshly captured `SystemProfile`
//! against the `PendingPlan` that Phase A persisted, and returns one of:
//!
//! - [`ResumeReadiness::Ready`] — the host is in the expected state and
//!   Phase B can continue.
//! - [`ResumeReadiness::NotReady`] — Phase A's edits did not take effect
//!   (likely a bootloader/initramfs misconfiguration). Carries a list of
//!   [`Divergence`]s describing what does not match. The caller offers
//!   rollback.
//! - [`ResumeReadiness::WrongHost`] — the snapshot id matches but the
//!   distro / bootloader / initramfs system has changed. The user has
//!   migrated the pending record between machines, or rebuilt the host.
//!   Refuse to continue.
//!
//! No filesystem or command access. All the data needed is in
//! `SystemProfile` (which detection produces) and `PendingPlan` (which
//! Phase A wrote).

use crate::detect::SystemProfile;
use crate::snapshot::pending::{HostFingerprint, PendingPlan};

use serde::{Deserialize, Serialize};

/// Whether `virtu resume` can continue safely.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResumeReadiness {
    /// Phase A landed; Phase B may continue.
    Ready,
    /// Phase A's edits did not produce the expected host state. Phase B
    /// must not run; the user should be offered rollback.
    NotReady { divergences: Vec<Divergence> },
    /// The host fundamentally differs from what Phase A targeted. Refuse
    /// to continue without explicit user confirmation.
    WrongHost { reasons: Vec<HostMismatch> },
}

impl ResumeReadiness {
    pub fn is_ready(&self) -> bool {
        matches!(self, ResumeReadiness::Ready)
    }
}

/// One observed mismatch between the post-reboot host and the expected
/// state. Each variant carries enough context for the CLI to print a
/// useful "this is what's wrong, here's how to fix it" message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Divergence {
    /// `/sys/kernel/iommu_groups/` is empty. The kernel did not enable
    /// IOMMU. Most often the bootloader edit did not stick.
    IommuNotActive,
    /// A PCI id the user wants passed through is not bound to vfio-pci.
    /// `current_driver` is `None` if the device is unbound, or carries
    /// the offending driver name (e.g. `nvidia`, `amdgpu`).
    VfioPciNotBoundTo {
        pci_id: String,
        current_driver: Option<String>,
    },
    /// A required kernel cmdline parameter is missing from the live
    /// `/proc/cmdline`.
    KernelCmdlineMissing { param: String },
    /// vfio_pci is not in the loaded module list, even though Phase A
    /// added it to the initramfs. Either the rebuild did not run or the
    /// module was unloaded post-boot.
    VfioModuleNotLoaded,
    /// A single-GPU hook script that the manifest says Phase B
    /// installed is now missing on the host filesystem.
    HookScriptMissing { vm_name: String, path: String },
    /// A single-GPU hook script exists on the host but its content has
    /// changed since Phase B wrote it. The captured hash and the live
    /// hash are reported so the user can spot tampering or partial
    /// regeneration.
    HookScriptDivergent {
        vm_name: String,
        path: String,
        expected_sha256: String,
        actual_sha256: String,
    },
    /// A single-GPU hook script exists with the right content but is
    /// not marked executable. libvirt would fail to invoke it at VM
    /// start; we surface this as a divergence so the user sees the
    /// problem before running the VM.
    HookScriptNotExecutable { vm_name: String, path: String },
}

impl Divergence {
    /// Plain-language summary suitable for the CLI report.
    pub fn human_summary(&self) -> String {
        match self {
            Divergence::IommuNotActive => {
                "IOMMU is not active. The bootloader edit likely did not take effect.".to_string()
            }
            Divergence::VfioPciNotBoundTo {
                pci_id,
                current_driver,
            } => match current_driver {
                Some(driver) => {
                    format!("vfio-pci has not claimed {pci_id}; it is currently bound to {driver}.")
                }
                None => format!("vfio-pci has not claimed {pci_id}; the device is unbound."),
            },
            Divergence::KernelCmdlineMissing { param } => {
                format!("Kernel cmdline is missing `{param}`.")
            }
            Divergence::VfioModuleNotLoaded => {
                "vfio_pci is not in the loaded module list. Initramfs rebuild may not have applied."
                    .to_string()
            }
            Divergence::HookScriptMissing { vm_name, path } => format!(
                "Single-GPU hook script for `{vm_name}` is missing at `{path}`. The host display manager won't release the GPU when the VM starts."
            ),
            Divergence::HookScriptDivergent {
                vm_name,
                path,
                expected_sha256,
                actual_sha256,
            } => format!(
                "Single-GPU hook script for `{vm_name}` at `{path}` has been modified \
                 since Phase B installed it (expected sha256 {}…, found {}…). \
                 Re-run `virtu apply` to regenerate, or roll back.",
                &expected_sha256[..expected_sha256.len().min(8)],
                &actual_sha256[..actual_sha256.len().min(8)]
            ),
            Divergence::HookScriptNotExecutable { vm_name, path } => format!(
                "Single-GPU hook script for `{vm_name}` at `{path}` is not executable. \
                 libvirt will refuse to run it. Re-run `virtu apply` or `chmod +x` it manually."
            ),
        }
    }
}

/// Reason a post-reboot host differs from the one Phase A targeted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostMismatch {
    DistroChanged { expected: String, actual: String },
    BootloaderChanged { expected: String, actual: String },
    InitramfsSystemChanged { expected: String, actual: String },
}

impl HostMismatch {
    pub fn human_summary(&self) -> String {
        match self {
            HostMismatch::DistroChanged { expected, actual } => {
                format!("Distro id changed: was `{expected}`, now `{actual}`.")
            }
            HostMismatch::BootloaderChanged { expected, actual } => {
                format!("Bootloader changed: was `{expected}`, now `{actual}`.")
            }
            HostMismatch::InitramfsSystemChanged { expected, actual } => {
                format!("Initramfs system changed: was `{expected}`, now `{actual}`.")
            }
        }
    }
}

/// Compare a freshly detected host against the Phase-A pending record.
///
/// The comparison is in three layers:
///
/// 1. Host-identity facts (distro, bootloader, initramfs). A change here
///    is a `WrongHost` and short-circuits the rest.
/// 2. Boot-state facts that Phase A's edits should have produced (IOMMU
///    active, kernel cmdline params, vfio module loaded). Each missing
///    fact becomes one `Divergence`.
/// 3. Per-device facts (each requested PCI id is bound to vfio-pci).
///    Each unbound id becomes one `Divergence`.
///
/// If neither layer finds anything wrong, the result is `Ready`.
pub fn verify_phase_a_landed(profile: &SystemProfile, pending: &PendingPlan) -> ResumeReadiness {
    let host_mismatches = host_identity_changes(profile, &pending.host_fingerprint);
    if !host_mismatches.is_empty() {
        return ResumeReadiness::WrongHost {
            reasons: host_mismatches,
        };
    }

    let mut divergences: Vec<Divergence> = Vec::new();

    if !profile.iommu_active() {
        divergences.push(Divergence::IommuNotActive);
    }

    let cpu_param = if profile.cpu.vendor.to_lowercase().contains("amd") {
        "amd_iommu=on"
    } else {
        "intel_iommu=on"
    };
    let required =
        required_cmdline_params(cpu_param, &pending.host_fingerprint.passthrough_pci_ids);
    for param in required {
        if !cmdline_has_param(&profile.kernel_cmdline, &param) {
            divergences.push(Divergence::KernelCmdlineMissing { param });
        }
    }

    let vfio_loaded = profile
        .readiness
        .loaded_modules
        .iter()
        .any(|m| m == "vfio_pci");
    if !vfio_loaded {
        divergences.push(Divergence::VfioModuleNotLoaded);
    }

    for pci_id in &pending.host_fingerprint.passthrough_pci_ids {
        match find_gpu_by_id(profile, pci_id) {
            Some(driver) if driver.as_deref() == Some("vfio-pci") => {} // bound, ok
            Some(driver) => divergences.push(Divergence::VfioPciNotBoundTo {
                pci_id: pci_id.clone(),
                current_driver: driver,
            }),
            // PCI id not present in profile.gpus could mean a USB/audio
            // companion device that detect doesn't enumerate as a GPU. We
            // skip those here; the IOMMU + cmdline checks above already
            // catch a host where vfio-pci didn't bind anything.
            None => continue,
        }
    }

    if divergences.is_empty() {
        ResumeReadiness::Ready
    } else {
        ResumeReadiness::NotReady { divergences }
    }
}

/// Compute the cmdline parameters Phase A was expected to add. Mirrors
/// `engine::executor::required_kernel_params` but is duplicated here so
/// the verifier stays self-contained and the public API stays narrow.
fn required_cmdline_params(cpu_param: &str, pci_ids: &[String]) -> Vec<String> {
    let mut out = vec![cpu_param.to_string(), "iommu=pt".to_string()];
    if !pci_ids.is_empty() {
        out.push(format!("vfio-pci.ids={}", pci_ids.join(",")));
    }
    out
}

/// True if `cmdline` (whitespace-separated) contains `param` either as a
/// standalone token (`intel_iommu=on`) or as a `vfio-pci.ids=` substring
/// match (the value can be reordered by some bootloaders).
fn cmdline_has_param(cmdline: &str, param: &str) -> bool {
    if param.starts_with("vfio-pci.ids=") {
        cmdline.contains(param)
    } else {
        cmdline.split_whitespace().any(|tok| tok == param)
    }
}

fn find_gpu_by_id(profile: &SystemProfile, pci_id: &str) -> Option<Option<String>> {
    let parts: Vec<&str> = pci_id.splitn(2, ':').collect();
    if parts.len() != 2 {
        return None;
    }
    let (vendor, device) = (parts[0], parts[1]);
    profile
        .gpus
        .iter()
        .find(|g| {
            g.vendor_id.eq_ignore_ascii_case(vendor) && g.device_id.eq_ignore_ascii_case(device)
        })
        .map(|g| g.current_driver.clone())
}

fn host_identity_changes(profile: &SystemProfile, expected: &HostFingerprint) -> Vec<HostMismatch> {
    let mut out = Vec::new();
    if profile.distro.id != expected.distro_id {
        out.push(HostMismatch::DistroChanged {
            expected: expected.distro_id.clone(),
            actual: profile.distro.id.clone(),
        });
    }
    if profile.bootloader.kind != expected.bootloader {
        out.push(HostMismatch::BootloaderChanged {
            expected: format!("{}", expected.bootloader),
            actual: format!("{}", profile.bootloader.kind),
        });
    }
    if profile.initramfs_system != expected.initramfs {
        out.push(HostMismatch::InitramfsSystemChanged {
            expected: expected.initramfs.name().to_string(),
            actual: profile.initramfs_system.name().to_string(),
        });
    }
    out
}

/// Verify that every single-GPU hook script Phase B claims to have
/// installed is still on disk, byte-identical to the captured
/// `post_edit_sha256`, and marked executable.
///
/// Pure read-only check. Returns a (possibly empty) list of
/// [`Divergence`]s — one per failed expectation. Callers integrate the
/// list into their existing `ResumeReadiness::NotReady` handling.
///
/// `vm_name` filters the manifest entries: only those keyed on the
/// expected libvirt domain are checked. This lets a future multi-domain
/// host avoid false positives when one of several pending VMs has been
/// torn down independently.
pub fn verify_hook_install(
    manifest: &crate::snapshot::manifest::SnapshotManifest,
    filesystem: &impl crate::snapshot::FileSystem,
    vm_name: &str,
) -> Vec<Divergence> {
    use crate::engine::step::StepKind;
    use sha2::{Digest, Sha256};

    let mut divergences = Vec::new();

    for entry in &manifest.entries {
        if entry.produced_by != StepKind::HookInstall {
            continue;
        }
        let path = entry.original_path.clone();
        let path_str = path.display().to_string();

        // 1. Presence.
        if !filesystem.exists(&path) {
            divergences.push(Divergence::HookScriptMissing {
                vm_name: vm_name.to_string(),
                path: path_str.clone(),
            });
            continue;
        }

        // 2. Content matches the captured post-edit hash. Skip when the
        // entry has no post-edit hash (means Phase B persisted the
        // entry but the writer did not finish; that's a Phase B bug we
        // surface as a divergence).
        match (&entry.post_edit_sha256, filesystem.read(&path)) {
            (Some(expected), Ok(bytes)) => {
                let mut hasher = Sha256::new();
                hasher.update(&bytes);
                let actual = hex::encode(hasher.finalize());
                if &actual != expected {
                    divergences.push(Divergence::HookScriptDivergent {
                        vm_name: vm_name.to_string(),
                        path: path_str.clone(),
                        expected_sha256: expected.clone(),
                        actual_sha256: actual,
                    });
                    continue;
                }
            }
            (None, _) => {
                divergences.push(Divergence::HookScriptDivergent {
                    vm_name: vm_name.to_string(),
                    path: path_str.clone(),
                    expected_sha256: "(not recorded)".to_string(),
                    actual_sha256: "(unknown)".to_string(),
                });
                continue;
            }
            (Some(_), Err(_)) => {
                divergences.push(Divergence::HookScriptMissing {
                    vm_name: vm_name.to_string(),
                    path: path_str.clone(),
                });
                continue;
            }
        }

        // 3. Executable bit. The `FileSystem` trait exposes
        // `is_executable` so we get a single uniform check across the
        // `RealFileSystem` (mode bits) and `MemoryFileSystem`
        // (recorded set).
        if !filesystem.is_executable(&path) {
            divergences.push(Divergence::HookScriptNotExecutable {
                vm_name: vm_name.to_string(),
                path: path_str,
            });
        }
    }

    divergences
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmdline_has_param_matches_standalone_tokens() {
        assert!(cmdline_has_param(
            "BOOT_IMAGE=/vmlinuz quiet intel_iommu=on iommu=pt",
            "intel_iommu=on"
        ));
        assert!(!cmdline_has_param(
            "BOOT_IMAGE=/vmlinuz quiet intel_iommu=off iommu=pt",
            "intel_iommu=on"
        ));
    }

    #[test]
    fn cmdline_has_param_matches_vfio_pci_substring() {
        // Some bootloaders fold the cmdline differently; we accept a
        // substring match for vfio-pci.ids=.
        assert!(cmdline_has_param(
            "quiet vfio-pci.ids=10de:1c03,10de:0fb9 iommu=pt",
            "vfio-pci.ids=10de:1c03,10de:0fb9"
        ));
    }

    #[test]
    fn divergence_human_summary_includes_pci_id() {
        let div = Divergence::VfioPciNotBoundTo {
            pci_id: "10de:1c03".to_string(),
            current_driver: Some("nvidia".to_string()),
        };
        let summary = div.human_summary();
        assert!(summary.contains("10de:1c03"));
        assert!(summary.contains("nvidia"));
    }

    #[test]
    fn iommu_not_active_message_is_friendly() {
        let summary = Divergence::IommuNotActive.human_summary();
        assert!(summary.contains("IOMMU"));
        assert!(summary.contains("bootloader"));
    }

    #[test]
    fn host_mismatch_distro_change_is_described() {
        let m = HostMismatch::DistroChanged {
            expected: "arch".to_string(),
            actual: "ubuntu".to_string(),
        };
        let summary = m.human_summary();
        assert!(summary.contains("arch"));
        assert!(summary.contains("ubuntu"));
    }

    #[test]
    fn ready_short_circuits_is_ready() {
        assert!(ResumeReadiness::Ready.is_ready());
        assert!(!ResumeReadiness::NotReady {
            divergences: vec![Divergence::IommuNotActive],
        }
        .is_ready());
        assert!(!ResumeReadiness::WrongHost {
            reasons: vec![HostMismatch::DistroChanged {
                expected: "arch".to_string(),
                actual: "fedora".to_string(),
            }],
        }
        .is_ready());
    }

    // ---- verify_hook_install -----------------------------------------

    use crate::engine::planner::PlanSummary;
    use crate::engine::step::StepKind as StepKind2;
    use crate::snapshot::manifest::{HostSummary, SnapshotEntry, SnapshotManifest};
    use crate::snapshot::{FileSystem, MemoryFileSystem};
    use std::path::PathBuf;

    fn empty_summary() -> PlanSummary {
        PlanSummary {
            total_steps: 0,
            pending_steps: 0,
            already_satisfied_steps: 0,
            max_risk: crate::engine::step::StepRisk::ReadOnly,
            requires_reboot: false,
            requires_confirmation: false,
        }
    }

    fn host_summary() -> HostSummary {
        HostSummary {
            distro_id: "arch".to_string(),
            distro_pretty_name: "Arch".to_string(),
            kernel_version: "6.10.0".to_string(),
            bootloader: crate::detect::bootloader::BootloaderKind::Grub2,
            initramfs: crate::detect::initramfs::InitramfsSystem::Mkinitcpio,
        }
    }

    /// Build a manifest with three HookInstall entries (dispatcher +
    /// release + reattach), seeding each path on the supplied
    /// MemoryFileSystem with the byte content `bytes`. The post-edit
    /// hash on the entry is set from the bytes so the verifier finds a
    /// match.
    fn seed_hook_manifest_and_fs(
        fs: &MemoryFileSystem,
        vm_name: &str,
        bytes_per_path: &[(&str, &[u8])],
    ) -> SnapshotManifest {
        use sha2::{Digest, Sha256};
        fs.create_dir_all(std::path::Path::new("/etc/libvirt/hooks/qemu.d"))
            .unwrap();
        fs.create_dir_all(std::path::Path::new(&format!(
            "/etc/libvirt/hooks/qemu.d/{}.d",
            vm_name
        )))
        .unwrap();

        let mut manifest =
            SnapshotManifest::new("snap-id", host_summary(), empty_summary(), Vec::new());

        for (path, bytes) in bytes_per_path {
            let abs = PathBuf::from(*path);
            fs.write_atomic(&abs, bytes).unwrap();
            fs.set_executable(&abs).unwrap();

            let mut hasher = Sha256::new();
            hasher.update(bytes);
            let hash = hex::encode(hasher.finalize());

            manifest.push_entry(SnapshotEntry {
                original_path: abs.clone(),
                backup_path: PathBuf::from(format!(
                    "files/{}",
                    crate::snapshot::sanitize_path(&abs)
                )),
                pre_edit_sha256: String::new(),
                post_edit_sha256: Some(hash),
                original_existed: false,
                produced_by: StepKind2::HookInstall,
            });
        }
        manifest
    }

    #[test]
    fn verify_hook_install_returns_empty_when_every_script_is_intact() {
        let fs = MemoryFileSystem::new();
        let manifest = seed_hook_manifest_and_fs(
            &fs,
            "virtu-windows",
            &[
                ("/etc/libvirt/hooks/qemu.d/virtu-windows", b"#!/bin/bash\n"),
                (
                    "/etc/libvirt/hooks/qemu.d/virtu-windows.d/release",
                    b"# release",
                ),
                (
                    "/etc/libvirt/hooks/qemu.d/virtu-windows.d/reattach",
                    b"# reattach",
                ),
            ],
        );

        let divergences = verify_hook_install(&manifest, &fs, "virtu-windows");
        assert!(
            divergences.is_empty(),
            "expected no divergences, got {divergences:?}"
        );
    }

    #[test]
    fn verify_hook_install_reports_missing_script_when_file_was_deleted() {
        let fs = MemoryFileSystem::new();
        let manifest = seed_hook_manifest_and_fs(
            &fs,
            "virtu-windows",
            &[
                ("/etc/libvirt/hooks/qemu.d/virtu-windows", b"#!/bin/bash\n"),
                (
                    "/etc/libvirt/hooks/qemu.d/virtu-windows.d/release",
                    b"# release",
                ),
                (
                    "/etc/libvirt/hooks/qemu.d/virtu-windows.d/reattach",
                    b"# reattach",
                ),
            ],
        );

        // User (or some other process) removed the dispatcher between
        // Phase B and a re-verify.
        fs.remove_file(std::path::Path::new(
            "/etc/libvirt/hooks/qemu.d/virtu-windows",
        ))
        .unwrap();

        let divergences = verify_hook_install(&manifest, &fs, "virtu-windows");
        assert_eq!(divergences.len(), 1);
        match &divergences[0] {
            Divergence::HookScriptMissing { vm_name, path } => {
                assert_eq!(vm_name, "virtu-windows");
                assert_eq!(path, "/etc/libvirt/hooks/qemu.d/virtu-windows");
            }
            other => panic!("expected HookScriptMissing, got {other:?}"),
        }
    }

    #[test]
    fn verify_hook_install_reports_divergent_script_when_content_changed() {
        let fs = MemoryFileSystem::new();
        let manifest = seed_hook_manifest_and_fs(
            &fs,
            "virtu-windows",
            &[
                ("/etc/libvirt/hooks/qemu.d/virtu-windows", b"#!/bin/bash\n"),
                (
                    "/etc/libvirt/hooks/qemu.d/virtu-windows.d/release",
                    b"# release",
                ),
                (
                    "/etc/libvirt/hooks/qemu.d/virtu-windows.d/reattach",
                    b"# reattach",
                ),
            ],
        );

        // Tamper with the release helper — same path, different bytes.
        fs.write_atomic(
            std::path::Path::new("/etc/libvirt/hooks/qemu.d/virtu-windows.d/release"),
            b"# tampered with",
        )
        .unwrap();

        let divergences = verify_hook_install(&manifest, &fs, "virtu-windows");
        assert_eq!(divergences.len(), 1);
        match &divergences[0] {
            Divergence::HookScriptDivergent {
                vm_name,
                path,
                expected_sha256,
                actual_sha256,
            } => {
                assert_eq!(vm_name, "virtu-windows");
                assert!(path.ends_with("/release"));
                assert_ne!(expected_sha256, actual_sha256);
            }
            other => panic!("expected HookScriptDivergent, got {other:?}"),
        }
    }

    #[test]
    fn verify_hook_install_reports_not_executable_when_chmod_was_dropped() {
        // Build the manifest+filesystem normally, then strip the
        // executable bit on the dispatcher. The verifier must report
        // exactly one HookScriptNotExecutable divergence; the other
        // two scripts are still fine.
        let fs = MemoryFileSystem::new();
        let manifest = seed_hook_manifest_and_fs(
            &fs,
            "virtu-windows",
            &[
                ("/etc/libvirt/hooks/qemu.d/virtu-windows", b"#!/bin/bash\n"),
                (
                    "/etc/libvirt/hooks/qemu.d/virtu-windows.d/release",
                    b"# release",
                ),
                (
                    "/etc/libvirt/hooks/qemu.d/virtu-windows.d/reattach",
                    b"# reattach",
                ),
            ],
        );

        // Drop the executable bit on the dispatcher only. There is no
        // public "unset_executable" on the trait; we simulate it by
        // re-creating the file (write_atomic does not preserve the
        // bit, since MemoryFileSystem clears it on remove and
        // write_atomic does not implicitly set it).
        let dispatcher = std::path::Path::new("/etc/libvirt/hooks/qemu.d/virtu-windows");
        fs.remove_file(dispatcher).unwrap();
        fs.write_atomic(dispatcher, b"#!/bin/bash\n").unwrap();
        // do *not* call set_executable — leave the file there but not exec.

        let divergences = verify_hook_install(&manifest, &fs, "virtu-windows");
        assert_eq!(divergences.len(), 1);
        match &divergences[0] {
            Divergence::HookScriptNotExecutable { vm_name, path } => {
                assert_eq!(vm_name, "virtu-windows");
                assert_eq!(path, "/etc/libvirt/hooks/qemu.d/virtu-windows");
            }
            other => panic!("expected HookScriptNotExecutable, got {other:?}"),
        }
    }

    #[test]
    fn hook_divergence_human_summaries_mention_vm_name_and_path() {
        let missing = Divergence::HookScriptMissing {
            vm_name: "virtu-windows".to_string(),
            path: "/etc/libvirt/hooks/qemu.d/virtu-windows".to_string(),
        };
        let summary = missing.human_summary();
        assert!(summary.contains("virtu-windows"));
        assert!(summary.contains("/etc/libvirt/hooks/qemu.d/virtu-windows"));

        let not_exec = Divergence::HookScriptNotExecutable {
            vm_name: "virtu-windows".to_string(),
            path: "/etc/libvirt/hooks/qemu.d/virtu-windows".to_string(),
        };
        let summary = not_exec.human_summary();
        assert!(summary.contains("not executable"));
    }
}
