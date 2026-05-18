//! Plan executor (slice 5.7 + slice 6.6).
//!
//! This file orchestrates the mutating half of Virtu. Slice 5.7 added the
//! snapshot step bridge; slice 6.6 adds Phase-A execution: bootloader,
//! VFIO modprobe, and initramfs writers, each wrapped in
//! `snapshot_then_write` and followed by host-command verification.
//!
//! Phase A stops after the initramfs rebuild and writes a `PendingPlan`
//! that Phase B (`virtu resume`, Milestone 6.5) will pick up after the
//! user reboots.

use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};

use crate::config::atomic_write::{declare_created_entry, snapshot_then_write};
use crate::config::writers::{
    grub::rewrite_grub_default,
    initramfs::{
        dracut::generate_dracut_conf, mkinitcpio::rewrite_mkinitcpio_conf,
        update_initramfs::rewrite_initramfs_modules,
    },
    systemd_boot::rewrite_systemd_boot_entry,
    vfio_modprobe::generate_vfio_modprobe_conf,
};
use crate::detect::bootloader::BootloaderKind;
use crate::detect::initramfs::InitramfsSystem;
use crate::detect::SystemProfile;
use crate::engine::planner::Plan;
use crate::engine::step::{PlannedStep, StepKind};
use crate::snapshot::{
    capture, FileSystem, HostFingerprint, PendingPlan, SnapshotError, SnapshotManifest,
};
use crate::vm::{GpuRole, PassthroughConfig};

/// Execute the snapshot step at the front of a [`Plan`].
///
/// Returns the captured snapshot id paired with a freshly read manifest. The
/// manifest is the value Phase-6 writers will thread through their
/// `snapshot_then_write` calls.
pub fn execute_snapshot_step(
    plan: &Plan,
    host: &SystemProfile,
    filesystem: &impl FileSystem,
    snapshots_root: &Path,
) -> Result<(String, SnapshotManifest), SnapshotError> {
    if !plan
        .steps
        .first()
        .map(|step| step.kind == StepKind::Snapshot)
        .unwrap_or(false)
    {
        return Err(SnapshotError::Io {
            path: snapshots_root.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "execute_snapshot_step: plan does not start with StepKind::Snapshot",
            ),
        });
    }

    let id = capture(plan, host, filesystem, snapshots_root)?;
    let snapshot_dir = snapshots_root.join(&id);
    let manifest_path = snapshot_dir.join(crate::snapshot::MANIFEST_FILENAME);
    let manifest_bytes = filesystem
        .read(&manifest_path)
        .map_err(|source| SnapshotError::Io {
            path: manifest_path.clone(),
            source,
        })?;
    let manifest_str = String::from_utf8(manifest_bytes).map_err(|err| SnapshotError::Io {
        path: manifest_path.clone(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, err),
    })?;
    let manifest: SnapshotManifest =
        toml::from_str(&manifest_str).map_err(|source| SnapshotError::ManifestParse {
            id: id.clone(),
            source,
        })?;
    Ok((id, manifest))
}

/// Errors raised by Phase-A execution.
#[derive(Debug, thiserror::Error)]
pub enum PhaseAError {
    #[error("snapshot step failed: {0}")]
    Snapshot(#[from] SnapshotError),
    #[error("writer rejected input for {step:?}: {source}")]
    Writer {
        step: StepKind,
        #[source]
        source: crate::config::writers::WriterError,
    },
    #[error("step {step:?}: {detail}")]
    Plan { step: StepKind, detail: String },
    #[error("pending-plan persistence failed: {0}")]
    PendingPersist(#[source] std::io::Error),
    #[error("regenerate command for {step:?} failed: {source}")]
    Regenerate {
        step: StepKind,
        #[source]
        source: crate::config::writers::commands::CommandError,
    },
}

/// Whether [`execute_phase_a`] should invoke the host's regenerate
/// commands (`grub-mkconfig`, `mkinitcpio -P`, …) after writing the
/// config files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegenerateMode {
    /// Do not shell out. Tests use this so the in-memory filesystem stays
    /// hermetic. Phase A still writes the files; the user must run the
    /// regenerate commands themselves before reboot.
    Skip,
    /// Shell out. Failures abort Phase A and surface a `Regenerate`
    /// error.
    Run,
}

/// Outcome of a successful Phase-A run.
#[derive(Debug, Clone)]
pub struct PhaseAOutcome {
    pub snapshot_id: String,
    pub pending_plan_path: PathBuf,
    /// Steps that ran successfully in Phase A.
    pub completed_steps: Vec<StepKind>,
    /// Plain-language reminder for the user.
    pub next_step_message: String,
}

/// Execute Phase A (snapshot, bootloader, VFIO, initramfs).
///
/// On success, persists a [`PendingPlan`] under
/// `<state_root>/<DEFAULT_FILENAME>` so `virtu resume` (Phase B) can
/// continue after the reboot. Returns a [`PhaseAOutcome`] with the
/// snapshot id and the message to surface to the user.
pub fn execute_phase_a(
    plan: &Plan,
    host: &SystemProfile,
    config: &PassthroughConfig,
    filesystem: &impl FileSystem,
    snapshots_root: &Path,
    state_root: &Path,
    regenerate_mode: RegenerateMode,
) -> Result<PhaseAOutcome, PhaseAError> {
    // 1. Capture the snapshot baseline.
    let (snapshot_id, mut manifest) =
        execute_snapshot_step(plan, host, filesystem, snapshots_root)?;
    let mut completed = vec![StepKind::Snapshot];

    // 2. Walk Phase-A steps in order and dispatch on kind.
    for step in &plan.steps {
        match step.kind {
            StepKind::Snapshot => continue, // already done
            StepKind::BootloaderWrite => {
                run_bootloader_step(step, host, config, filesystem, &mut manifest)?;
                completed.push(StepKind::BootloaderWrite);
            }
            StepKind::VfioConfig => {
                run_vfio_step(
                    step,
                    host,
                    config,
                    filesystem,
                    &mut manifest,
                    snapshots_root,
                    &snapshot_id,
                )?;
                completed.push(StepKind::VfioConfig);
            }
            StepKind::InitramfsWrite => {
                run_initramfs_step(
                    step,
                    host,
                    filesystem,
                    &mut manifest,
                    snapshots_root,
                    &snapshot_id,
                )?;
                completed.push(StepKind::InitramfsWrite);
            }
            // Phase B steps are skipped here. They are recorded in the
            // PendingPlan and run by `virtu resume`.
            _ => continue,
        }
    }

    // 2b. Run host regenerate commands so the writes take effect on the
    // next boot. Skipped in test mode.
    if regenerate_mode == RegenerateMode::Run {
        run_regenerate_commands(host, &completed)?;
    }

    // 3. Persist the updated manifest (it now carries post-edit hashes for
    // every Phase-A write).
    persist_manifest(filesystem, snapshots_root, &snapshot_id, &manifest)?;

    // 4. Persist the PendingPlan record so Phase B can pick up.
    let pending_plan = build_pending_plan(plan, host, config, &snapshot_id)?;
    let pending_path = state_root.join(crate::snapshot::pending::DEFAULT_FILENAME);
    filesystem
        .create_dir_all(state_root)
        .map_err(PhaseAError::PendingPersist)?;
    let serialized = toml::to_string(&pending_plan).map_err(|source| PhaseAError::Plan {
        step: StepKind::Snapshot,
        detail: format!("serializing PendingPlan: {source}"),
    })?;
    filesystem
        .write_atomic(&pending_path, serialized.as_bytes())
        .map_err(PhaseAError::PendingPersist)?;

    let next_step_message = format!(
        "Phase A complete. Snapshot id: {snapshot_id}\n\
         Reboot the host, then run `virtu resume` to finish setup.\n\
         If anything is wrong after the reboot, run `virtu rollback --to {snapshot_id}`."
    );

    Ok(PhaseAOutcome {
        snapshot_id,
        pending_plan_path: pending_path,
        completed_steps: completed,
        next_step_message,
    })
}

fn run_bootloader_step(
    step: &PlannedStep,
    host: &SystemProfile,
    config: &PassthroughConfig,
    filesystem: &impl FileSystem,
    manifest: &mut SnapshotManifest,
) -> Result<(), PhaseAError> {
    let target = step.touches.first().ok_or_else(|| PhaseAError::Plan {
        step: StepKind::BootloaderWrite,
        detail: "bootloader step has no touched path".to_string(),
    })?;

    let params = required_kernel_params(host, config);

    let current = filesystem.read(target).map_err(|source| {
        PhaseAError::Snapshot(SnapshotError::Io {
            path: target.clone(),
            source,
        })
    })?;
    let current_str = String::from_utf8_lossy(&current).into_owned();

    let new_str = match host.bootloader.kind {
        BootloaderKind::Grub2 => rewrite_grub_default(&current_str, &params),
        BootloaderKind::SystemdBoot => rewrite_systemd_boot_entry(&current_str, &params),
        BootloaderKind::Refind | BootloaderKind::Syslinux | BootloaderKind::Efistub => {
            return Err(PhaseAError::Plan {
                step: StepKind::BootloaderWrite,
                detail: format!(
                    "bootloader {} writer is not implemented yet (Milestone 6 Phase B+)",
                    host.bootloader.kind
                ),
            });
        }
        BootloaderKind::Unknown => {
            return Err(PhaseAError::Plan {
                step: StepKind::BootloaderWrite,
                detail: "bootloader is Unknown; planner should have refused this plan".to_string(),
            });
        }
    }
    .map_err(|source| PhaseAError::Writer {
        step: StepKind::BootloaderWrite,
        source,
    })?;

    snapshot_then_write(manifest, filesystem, target, new_str.as_bytes())?;
    Ok(())
}

fn run_vfio_step(
    step: &PlannedStep,
    host: &SystemProfile,
    config: &PassthroughConfig,
    filesystem: &impl FileSystem,
    manifest: &mut SnapshotManifest,
    snapshots_root: &Path,
    snapshot_id: &str,
) -> Result<(), PhaseAError> {
    let target = step.touches.first().ok_or_else(|| PhaseAError::Plan {
        step: StepKind::VfioConfig,
        detail: "vfio step has no touched path".to_string(),
    })?;

    let mut pci_ids = passthrough_pci_ids(host, config);
    pci_ids.sort();
    pci_ids.dedup();

    let new_content =
        generate_vfio_modprobe_conf(&pci_ids).map_err(|source| PhaseAError::Writer {
            step: StepKind::VfioConfig,
            source,
        })?;

    // The modprobe snippet is a Virtu-created file (does not exist on a
    // stock host). Capture already recorded it as `original_existed = false`,
    // but if a previous run left a partial entry we want a stable backup
    // path under <snapshot>/files/ regardless. declare_created_entry is a
    // no-op when the entry already exists, so this is safe to call.
    let backup_relative =
        PathBuf::from(crate::snapshot::FILES_SUBDIR).join(crate::snapshot::sanitize_path(target));
    let _ = snapshots_root;
    let _ = snapshot_id;
    declare_created_entry(manifest, target, &backup_relative, StepKind::VfioConfig);

    snapshot_then_write(manifest, filesystem, target, new_content.as_bytes())?;
    Ok(())
}

fn run_initramfs_step(
    step: &PlannedStep,
    host: &SystemProfile,
    filesystem: &impl FileSystem,
    manifest: &mut SnapshotManifest,
    snapshots_root: &Path,
    snapshot_id: &str,
) -> Result<(), PhaseAError> {
    let target = step.touches.first().ok_or_else(|| PhaseAError::Plan {
        step: StepKind::InitramfsWrite,
        detail: "initramfs step has no touched path".to_string(),
    })?;

    let new_content = match host.initramfs_system {
        InitramfsSystem::Mkinitcpio => {
            let current = filesystem.read(target).map_err(|source| {
                PhaseAError::Snapshot(SnapshotError::Io {
                    path: target.clone(),
                    source,
                })
            })?;
            let current_str = String::from_utf8_lossy(&current).into_owned();
            rewrite_mkinitcpio_conf(&current_str)
        }
        InitramfsSystem::Dracut => generate_dracut_conf(),
        InitramfsSystem::UpdateInitramfs => {
            let current = filesystem.read(target).unwrap_or_default();
            let current_str = String::from_utf8_lossy(&current).into_owned();
            rewrite_initramfs_modules(&current_str)
        }
        InitramfsSystem::Unknown => {
            return Err(PhaseAError::Plan {
                step: StepKind::InitramfsWrite,
                detail: "initramfs system Unknown; planner should have refused this plan"
                    .to_string(),
            });
        }
    }
    .map_err(|source| PhaseAError::Writer {
        step: StepKind::InitramfsWrite,
        source,
    })?;

    // Dracut writes a brand-new file. Make sure the manifest carries an
    // entry before we mutate.
    if matches!(host.initramfs_system, InitramfsSystem::Dracut) {
        let backup_relative = PathBuf::from(crate::snapshot::FILES_SUBDIR)
            .join(crate::snapshot::sanitize_path(target));
        declare_created_entry(manifest, target, &backup_relative, StepKind::InitramfsWrite);
    }
    let _ = snapshots_root;
    let _ = snapshot_id;

    snapshot_then_write(manifest, filesystem, target, new_content.as_bytes())?;
    Ok(())
}

/// Run the host's regenerate commands after the file writes finished.
/// Picks the right command per detected bootloader / initramfs system.
/// Failures are propagated as `PhaseAError::Regenerate`; the snapshot
/// manifest is already on disk by the time we get here, so the user can
/// always `virtu rollback --to <id>` to undo the file edits.
fn run_regenerate_commands(
    host: &SystemProfile,
    completed: &[StepKind],
) -> Result<(), PhaseAError> {
    use crate::config::writers::commands;

    if completed.contains(&StepKind::BootloaderWrite) {
        match host.bootloader.kind {
            BootloaderKind::Grub2 => {
                commands::run_grub_mkconfig().map_err(|source| PhaseAError::Regenerate {
                    step: StepKind::BootloaderWrite,
                    source,
                })?;
            }
            BootloaderKind::SystemdBoot => {
                // bootctl update is best-effort: a missing binary or a
                // non-zero exit (e.g. on first install) shouldn't abort
                // Phase A. The new entries take effect at next boot
                // regardless. Surface failures as a warning later.
                if let Err(err) = commands::run_bootctl_update() {
                    tracing::warn!(
                        ?err,
                        "bootctl update failed; continuing because new entries already on disk"
                    );
                }
            }
            // Bootloader writers for these are still scoped for later.
            BootloaderKind::Refind | BootloaderKind::Syslinux | BootloaderKind::Efistub => {}
            BootloaderKind::Unknown => {}
        }
    }

    if completed.contains(&StepKind::InitramfsWrite) {
        match host.initramfs_system {
            InitramfsSystem::Mkinitcpio => {
                commands::run_mkinitcpio_all().map_err(|source| PhaseAError::Regenerate {
                    step: StepKind::InitramfsWrite,
                    source,
                })?;
            }
            InitramfsSystem::Dracut => {
                commands::run_dracut_force_all().map_err(|source| PhaseAError::Regenerate {
                    step: StepKind::InitramfsWrite,
                    source,
                })?;
            }
            InitramfsSystem::UpdateInitramfs => {
                commands::run_update_initramfs_all().map_err(|source| PhaseAError::Regenerate {
                    step: StepKind::InitramfsWrite,
                    source,
                })?;
            }
            InitramfsSystem::Unknown => {}
        }
    }

    Ok(())
}

fn persist_manifest(
    filesystem: &impl FileSystem,
    snapshots_root: &Path,
    snapshot_id: &str,
    manifest: &SnapshotManifest,
) -> Result<(), PhaseAError> {
    let manifest_path = snapshots_root
        .join(snapshot_id)
        .join(crate::snapshot::MANIFEST_FILENAME);
    let serialized = toml::to_string(manifest).map_err(|source| PhaseAError::Plan {
        step: StepKind::Snapshot,
        detail: format!("serializing manifest: {source}"),
    })?;
    filesystem
        .write_atomic(&manifest_path, serialized.as_bytes())
        .map_err(|source| {
            PhaseAError::Snapshot(SnapshotError::Io {
                path: manifest_path,
                source,
            })
        })?;
    Ok(())
}

fn build_pending_plan(
    plan: &Plan,
    host: &SystemProfile,
    config: &PassthroughConfig,
    snapshot_id: &str,
) -> Result<PendingPlan, PhaseAError> {
    let pci_ids = passthrough_pci_ids(host, config);
    let fingerprint = HostFingerprint::capture(host, pci_ids);
    let remaining_steps = PendingPlan::phase_b_steps(plan);
    Ok(PendingPlan::new(
        snapshot_id,
        fingerprint,
        plan,
        remaining_steps,
        config.clone(),
    ))
}

/// Compute the kernel cmdline parameters Virtu must add for this host +
/// config combination. Mirrors planner logic so writers and the planner
/// stay in sync.
fn required_kernel_params(host: &SystemProfile, config: &PassthroughConfig) -> Vec<String> {
    let cpu_param = if host.cpu.vendor.to_lowercase().contains("amd") {
        "amd_iommu=on"
    } else {
        "intel_iommu=on"
    };
    let mut params = vec![cpu_param.to_string(), "iommu=pt".to_string()];

    let mut pci_ids = passthrough_pci_ids(host, config);
    pci_ids.sort();
    pci_ids.dedup();
    if !pci_ids.is_empty() {
        params.push(format!("vfio-pci.ids={}", pci_ids.join(",")));
    }
    params
}

fn passthrough_pci_ids(host: &SystemProfile, config: &PassthroughConfig) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for assignment in &config.gpu_roles {
        if assignment.role != GpuRole::Passthrough {
            continue;
        }
        if let Some(gpu) = host.gpus.iter().find(|g| g.pci_slot == assignment.pci_slot) {
            let id = format!("{}:{}", gpu.vendor_id, gpu.device_id);
            if !ids.contains(&id) {
                ids.push(id);
            }
            if let Some(audio) = &gpu.companion_audio {
                let aid = format!("{}:{}", audio.vendor_id, audio.device_id);
                if !ids.contains(&aid) {
                    ids.push(aid);
                }
            }
        }
    }
    ids
}

/// Print the plan's pending steps. Currently a no-op for non-snapshot steps.
/// Milestone 6 replaces this with real writers.
pub async fn execute_plan(plan: &[PlannedStep]) -> Result<()> {
    let snapshot_count = plan
        .iter()
        .filter(|step| step.kind == StepKind::Snapshot)
        .count();
    if snapshot_count == 0 {
        return Err(anyhow!(
            "Plan has no snapshot step; refusing to execute any other step."
        ));
    }
    for step in plan {
        println!("pending: {}", step.title);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Phase B (Milestone 6.5)
// ---------------------------------------------------------------------------

/// Errors raised by Phase-B execution.
#[derive(Debug, thiserror::Error)]
pub enum PhaseBError {
    #[error("phase B refused to run: {detail}")]
    Refused { detail: String },
    #[error("phase B I/O at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("step {step:?}: {detail}")]
    Step { step: StepKind, detail: String },
}

impl PhaseBError {
    fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// Outcome of a Phase-B run.
#[derive(Debug, Clone)]
pub struct PhaseBOutcome {
    /// Snapshot id Phase A captured. Surfaced again here so the user has
    /// the rollback escape hatch in mind even after a successful resume.
    pub snapshot_id: String,
    /// Step kinds Phase B executed (or acknowledged). Useful for the CLI
    /// summary.
    pub completed_steps: Vec<StepKind>,
    /// Step kinds that Phase B saw in the plan but cannot run yet because
    /// their underlying milestone has not landed. The CLI tells the user
    /// which features are still scoped for later.
    pub deferred_steps: Vec<StepKind>,
    /// Whether the pending-plan record was cleared. False means the file
    /// is still on disk for any reason (failure, user dry-run, etc.).
    pub pending_cleared: bool,
}

/// Execute the post-reboot plan tail described by a [`PendingPlan`].
///
/// Phase B does *not* repeat the verifier; it expects the caller (the CLI)
/// to have already invoked [`crate::engine::verify_phase_a_landed`] and
/// confirmed `Ready`. This split keeps the executor focused on action and
/// lets the verifier stay a pure function.
///
/// Today, the only mutating step kinds Phase B can produce are still
/// scoped for later milestones (hooks → M9, Looking Glass → M8, VM XML +
/// libvirt registration → M7). For each one we record a `deferred` entry
/// and print a clear "not implemented yet" message. The read-only
/// `StepKind::Verify` step is executed: it re-runs detection one more time
/// and prints a final-state summary.
///
/// Once every step has been processed (executed, deferred, or skipped),
/// Phase B clears `pending.toml`. The host is then back to a "no
/// in-progress plan" state and the user is free to run `virtu apply`
/// again later.
pub fn execute_phase_b(
    pending: &PendingPlan,
    profile_after_reboot: &SystemProfile,
    filesystem: &impl FileSystem,
    state_root: &Path,
) -> Result<PhaseBOutcome, PhaseBError> {
    let mut outcome = PhaseBOutcome {
        snapshot_id: pending.snapshot_id.clone(),
        completed_steps: Vec::new(),
        deferred_steps: Vec::new(),
        pending_cleared: false,
    };

    for step in &pending.remaining_steps {
        match step.kind {
            // Phase A's responsibility; should never appear in
            // pending.remaining_steps. Treat as a hard failure if it does
            // because it suggests the planner or the pending-plan persisted
            // the wrong slice.
            StepKind::Snapshot
            | StepKind::BootloaderWrite
            | StepKind::VfioConfig
            | StepKind::InitramfsWrite => {
                return Err(PhaseBError::Refused {
                    detail: format!(
                        "Phase A step {:?} appeared in Phase B's pending list. The pending record is corrupt; \
                         restore from snapshot {} and re-run virtu apply.",
                        step.kind, pending.snapshot_id
                    ),
                });
            }

            // Future milestones own these; for now Phase B records them as
            // deferred so the CLI can surface a clear "still on the
            // roadmap" message.
            StepKind::HookInstall
            | StepKind::LookingGlassInstall
            | StepKind::VmXmlGenerate
            | StepKind::VmRegister => {
                outcome.deferred_steps.push(step.kind.clone());
            }

            // The verify step re-runs detection and prints the final
            // health summary. It is read-only.
            StepKind::Verify => {
                run_verify_step(profile_after_reboot, pending);
                outcome.completed_steps.push(StepKind::Verify);
            }
        }
    }

    // Clear pending.toml so a subsequent `virtu apply` is allowed.
    let pending_path = state_root.join(crate::snapshot::pending::DEFAULT_FILENAME);
    if filesystem.exists(&pending_path) {
        filesystem
            .remove_file(&pending_path)
            .map_err(|source| PhaseBError::io(&pending_path, source))?;
        outcome.pending_cleared = true;
    }

    Ok(outcome)
}

fn run_verify_step(profile: &SystemProfile, pending: &PendingPlan) {
    println!("\n=== PHASE B VERIFY ===");
    println!(
        "Snapshot id: {}  ({} entries to roll back if needed)",
        pending.snapshot_id, pending.plan_summary.total_steps
    );
    println!("Kernel:      {}", profile.readiness.kernel_version);
    println!(
        "IOMMU:       {}",
        if profile.iommu_active() {
            format!("active ({} groups)", profile.iommu_groups.len())
        } else {
            "NOT active (Phase A's bootloader edit may not have applied)".to_string()
        }
    );
    let vfio_loaded = profile
        .readiness
        .loaded_modules
        .iter()
        .any(|m| m == "vfio_pci");
    println!(
        "vfio_pci:    {}",
        if vfio_loaded { "loaded" } else { "NOT loaded" }
    );
    for pci_id in &pending.host_fingerprint.passthrough_pci_ids {
        let parts: Vec<&str> = pci_id.splitn(2, ':').collect();
        if parts.len() != 2 {
            continue;
        }
        let (vendor, device) = (parts[0], parts[1]);
        let driver = profile
            .gpus
            .iter()
            .find(|g| {
                g.vendor_id.eq_ignore_ascii_case(vendor) && g.device_id.eq_ignore_ascii_case(device)
            })
            .and_then(|g| g.current_driver.clone())
            .unwrap_or_else(|| "(not detected as GPU)".to_string());
        println!("Device {pci_id}: driver {driver}");
    }
    println!();
}

#[cfg(test)]
mod phase_b_tests {
    use super::*;
    use crate::engine::planner::PlanSummary;
    use crate::engine::step::{PrivilegeNeed, StepRisk, StepState};
    use crate::snapshot::pending::HostFingerprint;
    use crate::snapshot::MemoryFileSystem;

    fn dummy_step(kind: StepKind) -> PlannedStep {
        PlannedStep {
            kind: kind.clone(),
            title: format!("{kind:?}"),
            summary: "test".to_string(),
            risk: StepRisk::Low,
            privilege: PrivilegeNeed::User,
            state: StepState::Pending,
            touches: Vec::new(),
            commands: Vec::new(),
            verification: "n/a".to_string(),
            rollback: "n/a".to_string(),
            requires_reboot: false,
            requires_confirmation: false,
        }
    }

    fn dummy_pending(remaining: Vec<PlannedStep>) -> PendingPlan {
        PendingPlan {
            virtu_version: "0.1.0".to_string(),
            created_at: chrono::Utc::now(),
            snapshot_id: "snap-x".to_string(),
            host_fingerprint: HostFingerprint {
                distro_id: "arch".to_string(),
                kernel_version: "6.10.0".to_string(),
                bootloader: crate::detect::bootloader::BootloaderKind::Grub2,
                initramfs: crate::detect::initramfs::InitramfsSystem::Mkinitcpio,
                passthrough_pci_ids: vec!["1002:7590".to_string()],
                kernel_cmdline_pre_apply: "BOOT_IMAGE=/vmlinuz".to_string(),
            },
            plan_summary: PlanSummary {
                total_steps: remaining.len(),
                pending_steps: remaining.len(),
                already_satisfied_steps: 0,
                max_risk: StepRisk::Low,
                requires_reboot: false,
                requires_confirmation: false,
            },
            remaining_steps: remaining,
            config: crate::vm::PassthroughConfig {
                vm_name: "virtu-windows".to_string(),
                gpu_mode: crate::vm::GpuPassthroughMode::DualGpu,
                gpu_roles: Vec::new(),
                monitor_plan: crate::vm::MonitorPlan::TwoMonitors {
                    host_connector: "DP-1".to_string(),
                    vm_connector: "DP-2".to_string(),
                },
                looking_glass: crate::vm::LookingGlassChoice::Disabled,
                iso_path: None,
                resources: crate::vm::VmResources {
                    ram_mb: 8192,
                    vcpu_count: 4,
                    disk: crate::vm::DiskChoice::Existing {
                        path: PathBuf::from("/var/lib/libvirt/images/win.qcow2"),
                    },
                },
                network: crate::vm::NetworkChoice::Nat,
                audio: crate::vm::AudioChoice::None,
                input: crate::vm::InputChoice::default(),
            },
        }
    }

    fn dummy_profile() -> SystemProfile {
        // We never call detect, so build the smallest viable profile by
        // hand. Only a few fields matter for the verify-step printer; the
        // rest can be defaults.
        use crate::detect::audio::AudioSystem;
        use crate::detect::bootloader::{BootloaderInfo, BootloaderKind};
        use crate::detect::cpu::CpuInfo;
        use crate::detect::display_manager::DisplayManager;
        use crate::detect::display_server::DisplayServer;
        use crate::detect::distro::{DistroFamily, DistroInfo, PackageManager};
        use crate::detect::initramfs::InitramfsSystem;
        use crate::detect::memory::MemInfo;
        use crate::detect::readiness::{
            KernelHeadersInfo, OvmfInfo, ReadinessInfo, UserAccessInfo,
        };
        use crate::detect::storage::StorageInfo;
        use crate::detect::virtualization::VirtInfo;
        use std::collections::HashMap;
        SystemProfile {
            cpu: CpuInfo {
                vendor: "AuthenticAMD".to_string(),
                model_name: "test".to_string(),
                physical_cores: 4,
                logical_cores: 8,
                numa_nodes: Vec::new(),
                iommu_capable: true,
                iommu_enabled: true,
                has_hyperthreading: true,
                core_to_threads: HashMap::new(),
            },
            gpus: Vec::new(),
            iommu_groups: Vec::new(),
            ram: MemInfo {
                total_kb: 0,
                available_kb: 0,
                hugepages_total: 0,
                hugepages_free: 0,
                hugepage_size_kb: 2048,
            },
            distro: DistroInfo {
                id: "arch".to_string(),
                id_like: Vec::new(),
                pretty_name: "Arch".to_string(),
                version_id: String::new(),
                family: DistroFamily::Arch,
                package_manager: PackageManager::Pacman,
            },
            bootloader: BootloaderInfo {
                kind: BootloaderKind::Grub2,
                config_path: None,
                entry_paths: Vec::new(),
                active_entry: None,
                update_command: None,
                is_uefi: true,
            },
            initramfs_system: InitramfsSystem::Mkinitcpio,
            display_manager: DisplayManager::Unknown,
            display_server: DisplayServer::Unknown,
            audio: AudioSystem::Unknown,
            monitors: Vec::new(),
            usb_devices: Vec::new(),
            storage: StorageInfo {
                default_vm_dir: PathBuf::from("/var/lib/libvirt/images"),
                available_bytes: 0,
            },
            virtualization: VirtInfo {
                qemu_version: None,
                libvirt_version: None,
                virsh_available: false,
                virt_manager_available: false,
                libvirtd_running: false,
            },
            readiness: ReadinessInfo {
                kernel_version: "6.10.0".to_string(),
                kernel_cmdline: "BOOT_IMAGE=/vmlinuz".to_string(),
                kernel_cmdline_params: Vec::new(),
                loaded_modules: Vec::new(),
                kernel_headers: KernelHeadersInfo {
                    present: false,
                    path: None,
                },
                secure_boot: false,
                ovmf: OvmfInfo {
                    code_paths: Vec::new(),
                    vars_paths: Vec::new(),
                },
                user_access: UserAccessInfo {
                    username: None,
                    groups: Vec::new(),
                    in_libvirt_group: false,
                    in_kvm_group: false,
                },
                libvirt_domains: Vec::new(),
            },
            secure_boot: false,
            kernel_cmdline: "BOOT_IMAGE=/vmlinuz".to_string(),
            scan_timestamp: chrono::Utc::now(),
        }
    }

    #[test]
    fn phase_b_clears_pending_record_when_complete() {
        let fs = MemoryFileSystem::new();
        let state_root = PathBuf::from("/var/lib/virtu/state");
        fs.create_dir_all(&state_root).unwrap();
        let pending_path = state_root.join(crate::snapshot::pending::DEFAULT_FILENAME);
        fs.write_atomic(&pending_path, b"placeholder").unwrap();

        let pending = dummy_pending(vec![dummy_step(StepKind::Verify)]);
        let outcome = execute_phase_b(&pending, &dummy_profile(), &fs, &state_root)
            .expect("phase B should succeed on a verify-only plan");

        assert!(outcome.pending_cleared);
        assert!(outcome.completed_steps.contains(&StepKind::Verify));
        assert!(!fs.exists(&pending_path));
    }

    #[test]
    fn phase_b_records_unimplemented_step_kinds_as_deferred() {
        let fs = MemoryFileSystem::new();
        let state_root = PathBuf::from("/var/lib/virtu/state");
        fs.create_dir_all(&state_root).unwrap();
        let pending_path = state_root.join(crate::snapshot::pending::DEFAULT_FILENAME);
        fs.write_atomic(&pending_path, b"placeholder").unwrap();

        let pending = dummy_pending(vec![
            dummy_step(StepKind::HookInstall),
            dummy_step(StepKind::LookingGlassInstall),
            dummy_step(StepKind::VmXmlGenerate),
            dummy_step(StepKind::VmRegister),
            dummy_step(StepKind::Verify),
        ]);
        let outcome = execute_phase_b(&pending, &dummy_profile(), &fs, &state_root).unwrap();

        for kind in [
            StepKind::HookInstall,
            StepKind::LookingGlassInstall,
            StepKind::VmXmlGenerate,
            StepKind::VmRegister,
        ] {
            assert!(outcome.deferred_steps.contains(&kind));
        }
        assert!(outcome.completed_steps.contains(&StepKind::Verify));
        assert!(outcome.pending_cleared);
    }

    #[test]
    fn phase_b_refuses_phase_a_step_in_pending_list() {
        let fs = MemoryFileSystem::new();
        let state_root = PathBuf::from("/var/lib/virtu/state");
        fs.create_dir_all(&state_root).unwrap();

        let pending = dummy_pending(vec![dummy_step(StepKind::BootloaderWrite)]);
        let err = execute_phase_b(&pending, &dummy_profile(), &fs, &state_root).unwrap_err();
        match err {
            PhaseBError::Refused { detail } => {
                assert!(detail.contains("BootloaderWrite"));
                assert!(detail.contains("snap-x"));
            }
            other => panic!("expected Refused, got {other:?}"),
        }
    }

    #[test]
    fn phase_b_succeeds_when_pending_record_already_absent() {
        // If the user manually deleted pending.toml between Phase A and
        // Phase B, the executor still walks the in-memory plan and reports
        // pending_cleared=false instead of erroring on a missing file.
        let fs = MemoryFileSystem::new();
        let state_root = PathBuf::from("/var/lib/virtu/state");
        fs.create_dir_all(&state_root).unwrap();

        let pending = dummy_pending(vec![dummy_step(StepKind::Verify)]);
        let outcome = execute_phase_b(&pending, &dummy_profile(), &fs, &state_root).unwrap();
        assert!(!outcome.pending_cleared);
        assert!(outcome.completed_steps.contains(&StepKind::Verify));
    }
}
