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
                run_vfio_step(step, host, config, filesystem, &mut manifest)?;
                completed.push(StepKind::VfioConfig);
            }
            StepKind::InitramfsWrite => {
                run_initramfs_step(step, host, filesystem, &mut manifest)?;
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
    declare_created_entry(manifest, target, &backup_relative, StepKind::VfioConfig);

    snapshot_then_write(manifest, filesystem, target, new_content.as_bytes())?;
    Ok(())
}

fn run_initramfs_step(
    step: &PlannedStep,
    host: &SystemProfile,
    filesystem: &impl FileSystem,
    manifest: &mut SnapshotManifest,
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
    #[error("step {step:?}: VM XML generation failed: {source}")]
    XmlGenerate {
        step: StepKind,
        #[source]
        source: crate::vm::xml::XmlError,
    },
    #[error("step {step:?}: virt-xml-validate rejected the generated XML: {source}")]
    XmlValidate {
        step: StepKind,
        #[source]
        source: crate::config::writers::commands::CommandError,
    },
    #[error("step {step:?}: snapshot-aware write failed: {source}")]
    Persist {
        step: StepKind,
        #[source]
        source: SnapshotError,
    },
    #[error(
        "step {step:?}: qemu-img create failed for {path}; rollback already cleaned up the partial image: {source}"
    )]
    DiskCreate {
        step: StepKind,
        path: PathBuf,
        #[source]
        source: crate::config::writers::commands::CommandError,
    },
    #[error(
        "step {step:?}: virsh define failed; the libvirt domain was NOT registered. The disk image at {disk:?} has been left on disk for inspection: {source}"
    )]
    VirshDefine {
        step: StepKind,
        disk: Option<PathBuf>,
        #[source]
        source: crate::config::writers::commands::CommandError,
    },
    #[error("step {step:?}: hook script generation failed: {source}")]
    HookGenerate {
        step: StepKind,
        #[source]
        source: crate::config::writers::hooks::HookScriptError,
    },
    #[error(
        "step {step:?}: bash -n rejected the generated `{script}` hook script for `{vm_name}`. \
         Refusing to install: a syntactically broken hook can lock the user out of the host display manager: {source}"
    )]
    HookValidate {
        step: StepKind,
        vm_name: String,
        script: String,
        #[source]
        source: Box<crate::config::writers::commands::CommandError>,
    },
    #[error("step {step:?}: failed to make hook script `{path}` executable: {source}")]
    HookChmod {
        step: StepKind,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Whether [`execute_phase_b`] should invoke the host commands its steps
/// declare (`virt-xml-validate`, `qemu-img create`, `virsh define`).
///
/// Tests use [`HostCommandMode::Skip`] so the in-memory filesystem stays
/// hermetic; the CLI uses [`HostCommandMode::Run`] for real execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostCommandMode {
    /// Generate XML and run all read-only logic, but do not invoke
    /// `virt-xml-validate`, `qemu-img create`, or `virsh define`. Phase B
    /// returns success after staging the XML and recording the disk-image
    /// entry. Used in tests so the in-memory filesystem stays hermetic.
    Skip,
    /// Invoke every host command. Failures abort Phase B and surface a
    /// structured error.
    Run,
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
/// As of slice 7.6, the executor runs `VmXmlGenerate` and `VmRegister`
/// itself. `HookInstall` (Milestone 9) and `LookingGlassInstall`
/// (permanently cut from v1.0) are still recorded as `deferred_steps`.
/// The read-only `Verify` step prints a final-state summary.
///
/// `snapshots_root` is required so the executor can update the manifest
/// (the XML file Phase B writes is recorded there for rollback). The
/// manifest is re-read after the `VmXmlGenerate` step writes so the
/// matching `SnapshotEntry` carries an up-to-date post-edit hash.
///
/// `host_command_mode` controls whether `virt-xml-validate`, `qemu-img
/// create`, and `virsh define` are actually invoked. Tests use
/// [`HostCommandMode::Skip`].
///
/// Once every step has been processed (executed, deferred, or skipped),
/// Phase B clears `pending.toml`. The host is then back to a "no
/// in-progress plan" state and the user is free to run `virtu apply`
/// again later.
pub fn execute_phase_b(
    pending: &PendingPlan,
    profile_after_reboot: &SystemProfile,
    filesystem: &impl FileSystem,
    snapshots_root: &Path,
    state_root: &Path,
    host_command_mode: HostCommandMode,
) -> Result<PhaseBOutcome, PhaseBError> {
    let mut outcome = PhaseBOutcome {
        snapshot_id: pending.snapshot_id.clone(),
        completed_steps: Vec::new(),
        deferred_steps: Vec::new(),
        pending_cleared: false,
    };

    // Load the existing manifest so VmXmlGenerate / VmRegister can use
    // snapshot_then_write through it.
    let mut manifest = read_manifest(filesystem, snapshots_root, &pending.snapshot_id)?;

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

            StepKind::VmXmlGenerate => {
                run_vm_xml_step(
                    step,
                    pending,
                    profile_after_reboot,
                    filesystem,
                    snapshots_root,
                    &mut manifest,
                    host_command_mode,
                )?;
                outcome.completed_steps.push(StepKind::VmXmlGenerate);
            }

            StepKind::VmRegister => {
                run_vm_register_step(
                    step,
                    pending,
                    filesystem,
                    snapshots_root,
                    &mut manifest,
                    host_command_mode,
                )?;
                outcome.completed_steps.push(StepKind::VmRegister);
            }

            StepKind::HookInstall => {
                run_hook_install_step(
                    step,
                    pending,
                    profile_after_reboot,
                    filesystem,
                    snapshots_root,
                    &mut manifest,
                    host_command_mode,
                )?;
                outcome.completed_steps.push(StepKind::HookInstall);
            }

            // Future / cut milestones own these; for now Phase B records
            // them as deferred so the CLI can surface a clear "still on the
            // roadmap" message. LookingGlassInstall is permanently
            // deferred per the v1.0 cut.
            StepKind::LookingGlassInstall => {
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

fn read_manifest(
    filesystem: &impl FileSystem,
    snapshots_root: &Path,
    snapshot_id: &str,
) -> Result<SnapshotManifest, PhaseBError> {
    let manifest_path = snapshots_root
        .join(snapshot_id)
        .join(crate::snapshot::MANIFEST_FILENAME);
    let bytes = filesystem
        .read(&manifest_path)
        .map_err(|source| PhaseBError::io(&manifest_path, source))?;
    let text = String::from_utf8(bytes).map_err(|err| {
        PhaseBError::io(
            manifest_path.clone(),
            std::io::Error::new(std::io::ErrorKind::InvalidData, err),
        )
    })?;
    toml::from_str::<SnapshotManifest>(&text).map_err(|source| PhaseBError::Step {
        step: StepKind::Snapshot,
        detail: format!(
            "manifest at {} is not parseable: {source}",
            manifest_path.display()
        ),
    })
}

fn write_manifest(
    filesystem: &impl FileSystem,
    snapshots_root: &Path,
    snapshot_id: &str,
    manifest: &SnapshotManifest,
) -> Result<(), PhaseBError> {
    let manifest_path = snapshots_root
        .join(snapshot_id)
        .join(crate::snapshot::MANIFEST_FILENAME);
    let serialized = toml::to_string(manifest).map_err(|source| PhaseBError::Step {
        step: StepKind::Snapshot,
        detail: format!("serializing manifest: {source}"),
    })?;
    filesystem
        .write_atomic(&manifest_path, serialized.as_bytes())
        .map_err(|source| PhaseBError::io(&manifest_path, source))?;
    Ok(())
}

/// Generate the libvirt domain XML and persist it under
/// `~/.virtu/<vm_name>.xml` through `snapshot_then_write`.
///
/// The XML path is taken from the planner's declared `touches`; the planner
/// keys the path on `vm_name`. We do not invent the path here so a planner
/// change cannot silently desync from the executor.
fn run_vm_xml_step(
    step: &PlannedStep,
    pending: &PendingPlan,
    profile: &SystemProfile,
    filesystem: &impl FileSystem,
    snapshots_root: &Path,
    manifest: &mut SnapshotManifest,
    mode: HostCommandMode,
) -> Result<(), PhaseBError> {
    let xml_path = step
        .touches
        .first()
        .cloned()
        .ok_or_else(|| PhaseBError::Step {
            step: StepKind::VmXmlGenerate,
            detail: "vm-xml step has no declared touch path".to_string(),
        })?;

    // 1. Render the XML.
    let xml_content =
        crate::engine::generate_vm_xml(profile, &pending.config).map_err(|source| {
            PhaseBError::XmlGenerate {
                step: StepKind::VmXmlGenerate,
                source,
            }
        })?;

    // 2. Validate via virt-xml-validate when host commands are enabled.
    if mode == HostCommandMode::Run {
        crate::config::writers::commands::validate_xml(&xml_content).map_err(|source| {
            PhaseBError::XmlValidate {
                step: StepKind::VmXmlGenerate,
                source,
            }
        })?;
    }

    // 3. Make sure the parent dir exists. ~/.virtu is created by Phase A
    // (snapshots + pending live there) but the regular case does not
    // include the XML file.
    if let Some(parent) = xml_path.parent() {
        filesystem
            .create_dir_all(parent)
            .map_err(|source| PhaseBError::io(parent, source))?;
    }

    // 4. Declare a manifest entry for the (likely-new) file before mutating.
    // declare_created_entry is a no-op when the entry already exists.
    let backup_relative = std::path::PathBuf::from(crate::snapshot::FILES_SUBDIR)
        .join(crate::snapshot::sanitize_path(&xml_path));
    crate::config::atomic_write::declare_created_entry(
        manifest,
        &xml_path,
        &backup_relative,
        StepKind::VmXmlGenerate,
    );

    // 5. Atomic write that records the post-edit hash on the manifest.
    crate::config::atomic_write::snapshot_then_write(
        manifest,
        filesystem,
        &xml_path,
        xml_content.as_bytes(),
    )
    .map_err(|source| PhaseBError::Persist {
        step: StepKind::VmXmlGenerate,
        source,
    })?;

    // 6. Persist the updated manifest before moving on. If `virsh define`
    // later fails, the rollback path needs to know the XML file exists.
    write_manifest(filesystem, snapshots_root, &pending.snapshot_id, manifest)?;

    Ok(())
}

/// Register the libvirt domain. Runs `qemu-img create` first if the user
/// asked for a fresh image; otherwise refuses on a missing existing image.
fn run_vm_register_step(
    _step: &PlannedStep,
    pending: &PendingPlan,
    filesystem: &impl FileSystem,
    snapshots_root: &Path,
    manifest: &mut SnapshotManifest,
    mode: HostCommandMode,
) -> Result<(), PhaseBError> {
    use crate::vm::DiskChoice;

    // 1. Refuse early when the user pointed at a non-existent existing
    // image. `virsh define` itself only checks the XML schema, not the
    // referenced disks; we want a clean error before libvirt commits.
    if let DiskChoice::Existing { path } = &pending.config.resources.disk {
        if !filesystem.exists(path) {
            return Err(PhaseBError::Step {
                step: StepKind::VmRegister,
                detail: format!(
                    "configured existing disk image {} does not exist; refusing to register the domain",
                    path.display()
                ),
            });
        }
    }

    // 2. Locate the XML the previous step wrote. We take it from the
    // manifest entry produced by VmXmlGenerate so the executor never
    // hand-rebuilds the path.
    let xml_path = manifest
        .entries
        .iter()
        .find(|entry| entry.produced_by == StepKind::VmXmlGenerate)
        .map(|entry| entry.original_path.clone())
        .ok_or_else(|| PhaseBError::Step {
            step: StepKind::VmRegister,
            detail: "manifest has no VmXmlGenerate entry; the prior step must have failed before persisting"
                .to_string(),
        })?;

    let mut created_disk_path: Option<std::path::PathBuf> = None;

    // 3. Create the disk image when requested. Only when host commands
    // are enabled — Skip mode keeps the in-memory filesystem hermetic.
    if let DiskChoice::Create {
        path,
        size_gb,
        format,
    } = &pending.config.resources.disk
    {
        let qemu_format = match format {
            crate::vm::DiskFormat::Qcow2 => {
                crate::config::writers::commands::DiskImageFormat::Qcow2
            }
            crate::vm::DiskFormat::Raw => crate::config::writers::commands::DiskImageFormat::Raw,
        };

        // Declare the disk image in the manifest before mutating, so a
        // later rollback knows to delete it.
        let backup_relative = std::path::PathBuf::from(crate::snapshot::FILES_SUBDIR)
            .join(crate::snapshot::sanitize_path(path));
        crate::config::atomic_write::declare_created_entry(
            manifest,
            path,
            &backup_relative,
            StepKind::VmRegister,
        );
        write_manifest(filesystem, snapshots_root, &pending.snapshot_id, manifest)?;

        if mode == HostCommandMode::Run {
            // Make sure the parent directory exists. `qemu-img create`
            // refuses to create one for us. We use the live filesystem
            // here so that even with `Run` we honor the FileSystem
            // abstraction for the directory creation step.
            if let Some(parent) = path.parent() {
                filesystem
                    .create_dir_all(parent)
                    .map_err(|source| PhaseBError::io(parent, source))?;
            }

            crate::config::writers::commands::run_qemu_img_create(path, *size_gb, qemu_format)
                .map_err(|source| PhaseBError::DiskCreate {
                    step: StepKind::VmRegister,
                    path: path.clone(),
                    source,
                })?;
            created_disk_path = Some(path.clone());
        }
    }

    // 4. Define the domain.
    if mode == HostCommandMode::Run {
        if let Err(source) = crate::config::writers::commands::run_virsh_define(&xml_path) {
            // Compensating action: if we just created the disk image,
            // remove it so a retry starts from a clean slate.
            if let Some(disk) = created_disk_path.clone() {
                if let Err(rm_err) = filesystem.remove_file(&disk) {
                    tracing::warn!(
                        ?rm_err,
                        path = %disk.display(),
                        "failed to clean up partial disk image after virsh define failure"
                    );
                }
            }
            return Err(PhaseBError::VirshDefine {
                step: StepKind::VmRegister,
                disk: created_disk_path,
                source,
            });
        }

        // 5. virsh define succeeded. Push the rollback action and persist
        // the manifest so a subsequent `virtu rollback --to <id>` knows to
        // run `virsh undefine <vm_name>`.
        manifest.restore_actions.push(
            crate::snapshot::manifest::RestoreAction::UndefineLibvirtDomain {
                name: pending.config.vm_name.clone(),
            },
        );
        write_manifest(filesystem, snapshots_root, &pending.snapshot_id, manifest)?;
    }

    Ok(())
}

fn run_hook_install_step(
    _step: &PlannedStep,
    pending: &PendingPlan,
    profile: &SystemProfile,
    filesystem: &impl FileSystem,
    snapshots_root: &Path,
    manifest: &mut SnapshotManifest,
    mode: HostCommandMode,
) -> Result<(), PhaseBError> {
    use crate::config::writers::commands::validate_bash_script;
    use crate::config::writers::hooks::{
        dispatcher_script, reattach_script, release_script, HookContext,
    };

    // 1. Build the HookContext from the post-reboot profile and the
    //    pending plan. The passthrough GPU and its companion audio
    //    drive the bind list; the host display manager drives the
    //    `systemctl stop`/`start` calls inside the helper scripts.
    let passthrough_gpu = pending
        .config
        .primary_passthrough_gpu(profile)
        .ok_or_else(|| PhaseBError::Step {
            step: StepKind::HookInstall,
            detail: "single-GPU hook install requires exactly one passthrough GPU; \
                     the user-choice config does not point at one"
                .to_string(),
        })?;

    let mut vfio_pci_ids: Vec<String> = Vec::new();
    vfio_pci_ids.push(format!(
        "{}:{}",
        passthrough_gpu.vendor_id, passthrough_gpu.device_id
    ));
    if let Some(audio) = &passthrough_gpu.companion_audio {
        vfio_pci_ids.push(format!("{}:{}", audio.vendor_id, audio.device_id));
    }

    let hook_ctx = HookContext {
        vm_name: pending.config.vm_name.clone(),
        display_manager: profile.display_manager.clone(),
        gpu_vendor: passthrough_gpu.vendor.clone(),
        vfio_pci_ids,
    };

    let dispatcher =
        dispatcher_script(&hook_ctx.vm_name).map_err(|source| PhaseBError::HookGenerate {
            step: StepKind::HookInstall,
            source,
        })?;
    let release = release_script(&hook_ctx).map_err(|source| PhaseBError::HookGenerate {
        step: StepKind::HookInstall,
        source,
    })?;
    let reattach = reattach_script(&hook_ctx).map_err(|source| PhaseBError::HookGenerate {
        step: StepKind::HookInstall,
        source,
    })?;

    // 2. Always validate. Even in Skip mode we want to ensure no
    //    syntactically broken script can ever land. The validator runs
    //    `bash -n`, which is hermetic and side-effect-free.
    if mode == HostCommandMode::Run || which::which("bash").is_ok() {
        for (label, content) in [
            ("dispatcher", &dispatcher),
            ("release", &release),
            ("reattach", &reattach),
        ] {
            validate_bash_script(content).map_err(|source| PhaseBError::HookValidate {
                step: StepKind::HookInstall,
                vm_name: hook_ctx.vm_name.clone(),
                script: label.to_string(),
                source: Box::new(source),
            })?;
        }
    }

    // 3. Hook directory layout:
    //
    //     /etc/libvirt/hooks/qemu.d/<vm_name>            (dispatcher)
    //     /etc/libvirt/hooks/qemu.d/<vm_name>.d/release  (helper)
    //     /etc/libvirt/hooks/qemu.d/<vm_name>.d/reattach (helper)
    //
    // libvirt invokes the dispatcher; the dispatcher execs the helper
    // matching the operation/sub-operation pair.
    let qemu_d = std::path::PathBuf::from("/etc/libvirt/hooks/qemu.d");
    let dispatcher_path = qemu_d.join(&hook_ctx.vm_name);
    let helper_dir = qemu_d.join(format!("{}.d", hook_ctx.vm_name));
    let release_path = helper_dir.join("release");
    let reattach_path = helper_dir.join("reattach");

    filesystem
        .create_dir_all(&qemu_d)
        .map_err(|source| PhaseBError::io(&qemu_d, source))?;
    filesystem
        .create_dir_all(&helper_dir)
        .map_err(|source| PhaseBError::io(&helper_dir, source))?;

    // 4. Declare every script the manifest must know about, *then*
    //    write them. declare_created_entry is a no-op when the entry
    //    already exists, so re-running this step is safe.
    for path in [&dispatcher_path, &release_path, &reattach_path] {
        let backup_relative = std::path::PathBuf::from(crate::snapshot::FILES_SUBDIR)
            .join(crate::snapshot::sanitize_path(path));
        crate::config::atomic_write::declare_created_entry(
            manifest,
            path,
            &backup_relative,
            StepKind::HookInstall,
        );
    }

    // 5. Atomic-write each script through the snapshot machinery so
    //    the manifest captures every byte.
    for (path, content) in [
        (&dispatcher_path, &dispatcher),
        (&release_path, &release),
        (&reattach_path, &reattach),
    ] {
        crate::config::atomic_write::snapshot_then_write(
            manifest,
            filesystem,
            path,
            content.as_bytes(),
        )
        .map_err(|source| PhaseBError::Persist {
            step: StepKind::HookInstall,
            source,
        })?;
    }

    // 6. Mark every script executable. libvirt only invokes the
    //    dispatcher, but the helpers must be executable too because
    //    the dispatcher `exec`s them directly (a non-exec helper would
    //    surface as a confusing libvirt hook failure at VM start).
    for path in [&dispatcher_path, &release_path, &reattach_path] {
        filesystem
            .set_executable(path)
            .map_err(|source| PhaseBError::HookChmod {
                step: StepKind::HookInstall,
                path: path.clone(),
                source,
            })?;
    }

    // 7. Persist the manifest now that every script is on disk and
    //    executable. Then push the rollback action.
    write_manifest(filesystem, snapshots_root, &pending.snapshot_id, manifest)?;
    manifest.restore_actions.push(
        crate::snapshot::manifest::RestoreAction::RemoveHookScripts {
            vm_name: hook_ctx.vm_name.clone(),
        },
    );
    write_manifest(filesystem, snapshots_root, &pending.snapshot_id, manifest)?;

    Ok(())
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
                guest_os: crate::vm::GuestOs::Windows11,
                gpu_mode: crate::vm::GpuPassthroughMode::DualGpu,
                gpu_roles: vec![
                    crate::vm::GpuRoleAssignment {
                        pci_slot: "0000:01:00.0".to_string(),
                        role: crate::vm::GpuRole::Passthrough,
                    },
                    crate::vm::GpuRoleAssignment {
                        pci_slot: "0000:02:00.0".to_string(),
                        role: crate::vm::GpuRole::Host,
                    },
                ],
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
        let snapshots_root = PathBuf::from("/var/lib/virtu/snapshots");
        let state_root = PathBuf::from("/var/lib/virtu/state");
        let pending = dummy_pending(vec![dummy_step(StepKind::Verify)]);
        seed_test_manifest(&fs, &snapshots_root, &pending);
        fs.create_dir_all(&state_root).unwrap();
        let pending_path = state_root.join(crate::snapshot::pending::DEFAULT_FILENAME);
        fs.write_atomic(&pending_path, b"placeholder").unwrap();

        let outcome = execute_phase_b(
            &pending,
            &dummy_profile(),
            &fs,
            &snapshots_root,
            &state_root,
            HostCommandMode::Skip,
        )
        .expect("phase B should succeed on a verify-only plan");

        assert!(outcome.pending_cleared);
        assert!(outcome.completed_steps.contains(&StepKind::Verify));
        assert!(!fs.exists(&pending_path));
    }

    #[test]
    fn phase_b_records_looking_glass_install_as_deferred() {
        // After slice 7.6, VmXmlGenerate and VmRegister are no longer
        // deferred — Phase B runs them. After slice 9.3, HookInstall is
        // no longer deferred either. Only LookingGlassInstall (cut from
        // v1.0) remains deferred. HookInstall is exercised separately in
        // the dedicated single-GPU test below; this case keeps it out of
        // the plan because the dummy profile uses
        // `DisplayManager::Unknown`, which `run_hook_install_step` would
        // (correctly) refuse.
        let fs = MemoryFileSystem::new();
        let snapshots_root = PathBuf::from("/var/lib/virtu/snapshots");
        let state_root = PathBuf::from("/var/lib/virtu/state");
        let mut pending = dummy_pending(vec![
            dummy_step(StepKind::LookingGlassInstall),
            vm_xml_step_with_touch(),
            dummy_step(StepKind::VmRegister),
            dummy_step(StepKind::Verify),
        ]);
        pending.config.vm_name = "virtu-test".to_string();
        let existing_disk = match &pending.config.resources.disk {
            crate::vm::DiskChoice::Existing { path } => path.clone(),
            _ => panic!("dummy_pending must use DiskChoice::Existing"),
        };
        if let Some(parent) = existing_disk.parent() {
            fs.create_dir_all(parent).unwrap();
        }
        fs.write_atomic(&existing_disk, b"sentinel").unwrap();
        seed_test_manifest(&fs, &snapshots_root, &pending);
        fs.create_dir_all(&state_root).unwrap();
        let pending_path = state_root.join(crate::snapshot::pending::DEFAULT_FILENAME);
        fs.write_atomic(&pending_path, b"placeholder").unwrap();

        let outcome = execute_phase_b(
            &pending,
            &dummy_profile_with_gpus(),
            &fs,
            &snapshots_root,
            &state_root,
            HostCommandMode::Skip,
        )
        .unwrap();

        assert!(outcome
            .deferred_steps
            .contains(&StepKind::LookingGlassInstall));
        assert!(!outcome.deferred_steps.contains(&StepKind::HookInstall));

        assert!(outcome.completed_steps.contains(&StepKind::VmXmlGenerate));
        assert!(outcome.completed_steps.contains(&StepKind::VmRegister));
        assert!(outcome.completed_steps.contains(&StepKind::Verify));

        assert!(outcome.pending_cleared);
    }

    #[test]
    fn phase_b_hook_install_writes_dispatcher_and_helpers_executable() {
        // Slice 9.3 happy path: a single-GPU plan with a known display
        // manager produces three executable scripts at the canonical
        // libvirt hook paths, plus a `RemoveHookScripts` rollback
        // action.
        use crate::detect::display_manager::DisplayManager;

        let fs = MemoryFileSystem::new();
        let snapshots_root = PathBuf::from("/var/lib/virtu/snapshots");
        let state_root = PathBuf::from("/var/lib/virtu/state");
        let mut pending = dummy_pending(vec![dummy_step(StepKind::HookInstall)]);
        pending.config.vm_name = "virtu-singlegpu".to_string();
        seed_test_manifest(&fs, &snapshots_root, &pending);
        fs.create_dir_all(&state_root).unwrap();

        let mut profile = dummy_profile_with_gpus();
        profile.display_manager = DisplayManager::Sddm;

        let outcome = execute_phase_b(
            &pending,
            &profile,
            &fs,
            &snapshots_root,
            &state_root,
            HostCommandMode::Skip,
        )
        .expect("hook install must succeed against the in-memory FS");

        assert!(outcome.completed_steps.contains(&StepKind::HookInstall));

        let dispatcher = PathBuf::from("/etc/libvirt/hooks/qemu.d/virtu-singlegpu");
        let release = PathBuf::from("/etc/libvirt/hooks/qemu.d/virtu-singlegpu.d/release");
        let reattach = PathBuf::from("/etc/libvirt/hooks/qemu.d/virtu-singlegpu.d/reattach");

        for path in [&dispatcher, &release, &reattach] {
            assert!(fs.exists(path), "hook script {} must exist", path.display());
            assert!(
                fs.is_executable(path),
                "hook script {} must be marked executable",
                path.display()
            );
        }

        let dispatcher_text = String::from_utf8(fs.read(&dispatcher).unwrap()).unwrap();
        assert!(
            dispatcher_text.contains("HOOK_DIR=\"/etc/libvirt/hooks/qemu.d/virtu-singlegpu.d\"")
        );
        assert!(dispatcher_text.contains("if [ \"$vm\" != 'virtu-singlegpu' ]; then"));

        let release_text = String::from_utf8(fs.read(&release).unwrap()).unwrap();
        assert!(release_text.contains("systemctl stop sddm"));
        assert!(release_text.contains("modprobe vfio-pci"));

        let reattach_text = String::from_utf8(fs.read(&reattach).unwrap()).unwrap();
        assert!(reattach_text.contains("systemctl start sddm"));

        // Manifest carries a RemoveHookScripts action keyed on the
        // actual vm_name.
        let manifest_path = snapshots_root
            .join(&pending.snapshot_id)
            .join(crate::snapshot::MANIFEST_FILENAME);
        let manifest_text = String::from_utf8(fs.read(&manifest_path).unwrap()).unwrap();
        let manifest: SnapshotManifest = toml::from_str(&manifest_text).unwrap();
        assert!(manifest.restore_actions.iter().any(|a| matches!(
            a,
            crate::snapshot::RestoreAction::RemoveHookScripts { vm_name } if vm_name == "virtu-singlegpu"
        )));
    }

    #[test]
    fn phase_b_hook_install_refuses_unknown_display_manager() {
        // Defense-in-depth: slice 9.1 already refuses Unknown DMs at the
        // template level. Phase B must surface that as a structured
        // `HookGenerate` error instead of crashing or installing
        // garbage. The dummy profile already uses
        // `DisplayManager::Unknown`, so this is the natural test.
        let fs = MemoryFileSystem::new();
        let snapshots_root = PathBuf::from("/var/lib/virtu/snapshots");
        let state_root = PathBuf::from("/var/lib/virtu/state");
        let mut pending = dummy_pending(vec![dummy_step(StepKind::HookInstall)]);
        pending.config.vm_name = "virtu-singlegpu".to_string();
        seed_test_manifest(&fs, &snapshots_root, &pending);
        fs.create_dir_all(&state_root).unwrap();

        let err = execute_phase_b(
            &pending,
            &dummy_profile_with_gpus(),
            &fs,
            &snapshots_root,
            &state_root,
            HostCommandMode::Skip,
        )
        .unwrap_err();

        match err {
            PhaseBError::HookGenerate { step, source } => {
                assert_eq!(step, StepKind::HookInstall);
                assert!(matches!(
                    source,
                    crate::config::writers::hooks::HookScriptError::UnknownDisplayManager
                ));
            }
            other => panic!("expected HookGenerate(UnknownDisplayManager), got {other:?}"),
        }
    }

    #[test]
    fn phase_b_refuses_phase_a_step_in_pending_list() {
        let fs = MemoryFileSystem::new();
        let snapshots_root = PathBuf::from("/var/lib/virtu/snapshots");
        let state_root = PathBuf::from("/var/lib/virtu/state");
        let pending = dummy_pending(vec![dummy_step(StepKind::BootloaderWrite)]);
        seed_test_manifest(&fs, &snapshots_root, &pending);
        fs.create_dir_all(&state_root).unwrap();

        let err = execute_phase_b(
            &pending,
            &dummy_profile(),
            &fs,
            &snapshots_root,
            &state_root,
            HostCommandMode::Skip,
        )
        .unwrap_err();
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
        let snapshots_root = PathBuf::from("/var/lib/virtu/snapshots");
        let state_root = PathBuf::from("/var/lib/virtu/state");
        let pending = dummy_pending(vec![dummy_step(StepKind::Verify)]);
        seed_test_manifest(&fs, &snapshots_root, &pending);
        fs.create_dir_all(&state_root).unwrap();

        let outcome = execute_phase_b(
            &pending,
            &dummy_profile(),
            &fs,
            &snapshots_root,
            &state_root,
            HostCommandMode::Skip,
        )
        .unwrap();
        assert!(!outcome.pending_cleared);
        assert!(outcome.completed_steps.contains(&StepKind::Verify));
    }

    #[test]
    fn phase_b_writes_xml_under_dot_virtu_and_records_manifest_entry() {
        // Slice 7.6 happy path against MemoryFileSystem in Skip mode:
        // the XML lives at the planner-declared touch path, the manifest
        // gains a VmXmlGenerate entry with a post-edit hash, and the
        // domain XML actually generates from the user's PassthroughConfig.
        let fs = MemoryFileSystem::new();
        let snapshots_root = PathBuf::from("/var/lib/virtu/snapshots");
        let state_root = PathBuf::from("/var/lib/virtu/state");
        let mut pending = dummy_pending(vec![vm_xml_step_with_touch()]);
        pending.config.vm_name = "virtu-test".to_string();
        seed_test_manifest(&fs, &snapshots_root, &pending);
        fs.create_dir_all(&state_root).unwrap();

        let outcome = execute_phase_b(
            &pending,
            &dummy_profile_with_gpus(),
            &fs,
            &snapshots_root,
            &state_root,
            HostCommandMode::Skip,
        )
        .expect("vm-xml step should succeed in Skip mode");

        assert!(outcome.completed_steps.contains(&StepKind::VmXmlGenerate));
        let xml_path = PathBuf::from("/tmp/virtu-test.xml");
        assert!(fs.exists(&xml_path), "Phase B must write the XML file");
        let content = fs.read(&xml_path).unwrap();
        let xml = String::from_utf8(content).unwrap();
        assert!(xml.contains("<domain type='kvm'"));
        assert!(xml.contains("<name>virtu-test</name>"));

        let manifest_path = snapshots_root
            .join(&pending.snapshot_id)
            .join(crate::snapshot::MANIFEST_FILENAME);
        let manifest_text = String::from_utf8(fs.read(&manifest_path).unwrap()).unwrap();
        let manifest: SnapshotManifest = toml::from_str(&manifest_text).unwrap();
        let entry = manifest
            .entries
            .iter()
            .find(|e| e.original_path == xml_path)
            .expect("manifest must record the new XML file");
        assert!(!entry.original_existed);
        assert!(entry.post_edit_sha256.is_some());
        assert_eq!(entry.produced_by, StepKind::VmXmlGenerate);
    }

    #[test]
    fn phase_b_refuses_register_when_existing_disk_image_is_missing() {
        // VmRegister with DiskChoice::Existing pointing at a path that
        // does not exist must error out before any host command runs.
        let fs = MemoryFileSystem::new();
        let snapshots_root = PathBuf::from("/var/lib/virtu/snapshots");
        let state_root = PathBuf::from("/var/lib/virtu/state");
        let mut pending = dummy_pending(vec![
            vm_xml_step_with_touch(),
            dummy_step(StepKind::VmRegister),
        ]);
        pending.config.vm_name = "virtu-test".to_string();
        pending.config.resources.disk = crate::vm::DiskChoice::Existing {
            path: PathBuf::from("/var/lib/libvirt/images/missing.qcow2"),
        };
        seed_test_manifest(&fs, &snapshots_root, &pending);
        fs.create_dir_all(&state_root).unwrap();

        let err = execute_phase_b(
            &pending,
            &dummy_profile_with_gpus(),
            &fs,
            &snapshots_root,
            &state_root,
            HostCommandMode::Skip,
        )
        .unwrap_err();
        match err {
            PhaseBError::Step { step, detail } => {
                assert_eq!(step, StepKind::VmRegister);
                assert!(detail.contains("missing.qcow2"));
            }
            other => panic!("expected Step error, got {other:?}"),
        }
    }

    #[test]
    fn phase_b_register_declares_disk_image_in_manifest_for_create_choice() {
        // VmRegister with DiskChoice::Create declares the future disk
        // image in the manifest *before* invoking qemu-img, so a partial
        // run can be rolled back. In Skip mode, qemu-img is skipped but
        // the manifest entry still lands and persists.
        let fs = MemoryFileSystem::new();
        let snapshots_root = PathBuf::from("/var/lib/virtu/snapshots");
        let state_root = PathBuf::from("/var/lib/virtu/state");
        let disk_path = PathBuf::from("/var/lib/libvirt/images/virtu-test.qcow2");
        let mut pending = dummy_pending(vec![
            vm_xml_step_with_touch(),
            dummy_step(StepKind::VmRegister),
        ]);
        pending.config.vm_name = "virtu-test".to_string();
        pending.config.resources.disk = crate::vm::DiskChoice::Create {
            path: disk_path.clone(),
            size_gb: 100,
            format: crate::vm::DiskFormat::Qcow2,
        };
        seed_test_manifest(&fs, &snapshots_root, &pending);
        fs.create_dir_all(&state_root).unwrap();

        let outcome = execute_phase_b(
            &pending,
            &dummy_profile_with_gpus(),
            &fs,
            &snapshots_root,
            &state_root,
            HostCommandMode::Skip,
        )
        .unwrap();
        assert!(outcome.completed_steps.contains(&StepKind::VmRegister));

        let manifest_path = snapshots_root
            .join(&pending.snapshot_id)
            .join(crate::snapshot::MANIFEST_FILENAME);
        let manifest_text = String::from_utf8(fs.read(&manifest_path).unwrap()).unwrap();
        let manifest: SnapshotManifest = toml::from_str(&manifest_text).unwrap();
        let disk_entry = manifest
            .entries
            .iter()
            .find(|e| e.original_path == disk_path)
            .expect("manifest must record the planned disk image");
        assert_eq!(disk_entry.produced_by, StepKind::VmRegister);
        assert!(!disk_entry.original_existed);

        // In Skip mode no virsh define ran, so no UndefineLibvirtDomain
        // restore action should have been pushed.
        assert!(!manifest.restore_actions.iter().any(|a| matches!(
            a,
            crate::snapshot::RestoreAction::UndefineLibvirtDomain { .. }
        )));
    }

    /// Build a minimal in-memory snapshot manifest for the snapshot id
    /// the dummy pending plan uses, and persist it to the test
    /// MemoryFileSystem under `<snapshots_root>/<id>/manifest.toml`.
    fn seed_test_manifest(fs: &MemoryFileSystem, snapshots_root: &Path, pending: &PendingPlan) {
        let snapshot_dir = snapshots_root.join(&pending.snapshot_id);
        fs.create_dir_all(&snapshot_dir.join(crate::snapshot::FILES_SUBDIR))
            .unwrap();
        let manifest = SnapshotManifest::new(
            &pending.snapshot_id,
            crate::snapshot::manifest::HostSummary {
                distro_id: "arch".to_string(),
                distro_pretty_name: "Arch".to_string(),
                kernel_version: "6.10.0".to_string(),
                bootloader: crate::detect::bootloader::BootloaderKind::Grub2,
                initramfs: crate::detect::initramfs::InitramfsSystem::Mkinitcpio,
            },
            pending.plan_summary.clone(),
            Vec::new(),
        );
        let serialized = toml::to_string(&manifest).unwrap();
        let manifest_path = snapshot_dir.join(crate::snapshot::MANIFEST_FILENAME);
        fs.write_atomic(&manifest_path, serialized.as_bytes())
            .unwrap();
    }

    /// A `VmXmlGenerate` step with a writable touch path the
    /// MemoryFileSystem can host.
    fn vm_xml_step_with_touch() -> PlannedStep {
        let mut step = dummy_step(StepKind::VmXmlGenerate);
        step.touches = vec![PathBuf::from("/tmp/virtu-test.xml")];
        step
    }

    /// Profile with two GPUs so `vm_view` succeeds for the dummy config
    /// (DualGpu mode + the role assignments below).
    fn dummy_profile_with_gpus() -> SystemProfile {
        use crate::detect::gpu::{GpuInfo, GpuType, GpuVendor};
        let mut profile = dummy_profile();
        profile.gpus = vec![
            GpuInfo {
                pci_slot: "0000:01:00.0".to_string(),
                vendor: GpuVendor::Amd,
                gpu_type: GpuType::Discrete,
                model_name: "Test AMD".to_string(),
                vendor_id: "1002".to_string(),
                device_id: "7590".to_string(),
                subsystem_vendor_id: "0000".to_string(),
                subsystem_device_id: "0000".to_string(),
                current_driver: None,
                iommu_group_id: Some(1),
                iommu_isolated: true,
                rom_accessible: false,
                companion_audio: None,
                is_boot_vga: false,
                vfio_compatible: true,
                quirks: Vec::new(),
            },
            GpuInfo {
                pci_slot: "0000:02:00.0".to_string(),
                vendor: GpuVendor::Nvidia,
                gpu_type: GpuType::Discrete,
                model_name: "Test NVIDIA".to_string(),
                vendor_id: "10de".to_string(),
                device_id: "1f08".to_string(),
                subsystem_vendor_id: "0000".to_string(),
                subsystem_device_id: "0000".to_string(),
                current_driver: None,
                iommu_group_id: Some(2),
                iommu_isolated: true,
                rom_accessible: false,
                companion_audio: None,
                is_boot_vga: false,
                vfio_compatible: true,
                quirks: Vec::new(),
            },
        ];
        profile
    }
}
