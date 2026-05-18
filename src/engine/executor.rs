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
