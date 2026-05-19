//! Dry-run plan construction.
//!
//! The planner consumes a [`SystemProfile`], a [`CompatibilityReport`], and a
//! validated [`PassthroughConfig`] and produces an ordered [`Plan`] of
//! [`PlannedStep`]s. The planner is intentionally read-only: it never touches
//! the host. It refuses to plan when the user-choice [`ValidationReport`]
//! contains errors, and it skips host-mutating phases that are flagged by
//! `CompatibilityStatus::Blocked`.
//!
//! Steps are described in declarative terms (touches, commands, verification,
//! rollback) so an executor can be added later without changing the plan
//! shape.

use crate::detect::bootloader::BootloaderKind;
use crate::detect::SystemProfile;
use crate::engine::compatibility::{CompatibilityReport, CompatibilityStatus};
use crate::vm::{
    validate, GpuPassthroughMode, GpuRole, LookingGlassChoice, MonitorPlan, PassthroughConfig,
    SingleMonitorStrategy, ValidationIssue, ValidationReport,
};

use super::step::{PlannedStep, PrivilegeNeed, StepKind, StepRisk, StepState};

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Why the planner refused to produce a plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanError {
    /// The user-choice validation has errors. The user must adjust choices
    /// before any plan can be generated.
    ValidationFailed(ValidationReport),
    /// The compatibility report has hard blockers. The host must be fixed
    /// before any plan can be generated.
    CompatibilityBlocked(Vec<String>),
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanError::ValidationFailed(report) => {
                let count = report.errors().count();
                write!(
                    f,
                    "Cannot plan: user-choice validation has {count} error(s)."
                )
            }
            PlanError::CompatibilityBlocked(ids) => {
                write!(
                    f,
                    "Cannot plan: compatibility report has hard blockers: {}",
                    ids.join(", ")
                )
            }
        }
    }
}

impl std::error::Error for PlanError {}

/// An ordered, dry-run plan. Building a [`Plan`] does not mutate the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub steps: Vec<PlannedStep>,
    /// Validation warnings are kept alongside the plan so the executor and
    /// the TUI can surface them without re-running validation.
    pub warnings: Vec<ValidationIssue>,
    pub summary: PlanSummary,
}

/// Aggregated metadata about a [`Plan`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSummary {
    pub total_steps: usize,
    pub pending_steps: usize,
    pub already_satisfied_steps: usize,
    pub max_risk: StepRisk,
    pub requires_reboot: bool,
    pub requires_confirmation: bool,
}

impl Plan {
    pub fn pending(&self) -> impl Iterator<Item = &PlannedStep> {
        self.steps
            .iter()
            .filter(|step| step.state == StepState::Pending)
    }

    pub fn print_human(&self) {
        println!("\n=== VIRTU PLAN ===");
        println!(
            "Total: {} step(s); {} pending, {} already satisfied",
            self.summary.total_steps,
            self.summary.pending_steps,
            self.summary.already_satisfied_steps
        );
        println!("Max risk: {}", self.summary.max_risk);
        if self.summary.requires_reboot {
            println!("Reboot: required after applying.");
        }
        if self.summary.requires_confirmation {
            println!("Some step(s) require explicit user confirmation.");
        }

        for (idx, step) in self.steps.iter().enumerate() {
            println!(
                "\n[{n}] {title}  ({risk}, {privilege}, {state})",
                n = idx + 1,
                title = step.title,
                risk = step.risk,
                privilege = step.privilege,
                state = step.state
            );
            println!("    {}", step.summary);

            if !step.touches.is_empty() {
                println!(
                    "    Touches: {}",
                    step.touches
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if !step.commands.is_empty() {
                for cmd in &step.commands {
                    println!("    Run: {cmd}");
                }
            }
            println!("    Verify: {}", step.verification);
            println!("    Rollback: {}", step.rollback);
            if step.must_confirm() {
                println!("    Requires explicit user confirmation before running.");
            }
        }

        if !self.warnings.is_empty() {
            println!("\nWarnings:");
            for warning in &self.warnings {
                println!("  - {} ({})", warning.message, warning.id);
            }
        }
        println!();
    }
}

/// Build an ordered dry-run [`Plan`].
///
/// Refuses to plan when validation finds errors or the compatibility report
/// is blocked. Validation warnings are propagated into [`Plan::warnings`].
pub fn plan(
    profile: &SystemProfile,
    report: &CompatibilityReport,
    config: &PassthroughConfig,
) -> Result<Plan, PlanError> {
    if report.status == CompatibilityStatus::Blocked {
        let blockers: Vec<String> = report
            .findings
            .iter()
            .filter(|finding| matches!(finding.severity, crate::engine::FindingSeverity::Fail))
            .map(|finding| finding.id.clone())
            .collect();
        return Err(PlanError::CompatibilityBlocked(blockers));
    }

    let validation = validate(profile, report, config);
    if validation.has_errors() {
        return Err(PlanError::ValidationFailed(validation));
    }

    let warnings: Vec<ValidationIssue> = validation.warnings().cloned().collect();
    let derived_mode = config.derived_mode(profile);

    let mut steps: Vec<PlannedStep> = Vec::new();

    steps.push(snapshot_step());

    if let Some(step) = bootloader_step(profile, config) {
        steps.push(step);
    }

    if let Some(step) = vfio_step(profile, config) {
        steps.push(step);
    }

    if let Some(step) = initramfs_step(profile) {
        steps.push(step);
    }

    if matches!(derived_mode, Some(GpuPassthroughMode::SingleGpu))
        || single_gpu_hook_handoff(config)
    {
        steps.push(single_gpu_hook_step(profile, config));
    }

    if let LookingGlassChoice::Enabled { install_mode, .. } = &config.looking_glass {
        steps.push(looking_glass_step(*install_mode));
    }

    steps.push(vm_xml_step(config));
    steps.push(vm_register_step(config));
    steps.push(verify_step());

    let summary = summarize(&steps);

    Ok(Plan {
        steps,
        warnings,
        summary,
    })
}

/// Backwards-compatible thin builder used by older call sites that have only
/// a [`SystemProfile`] in hand. It produces a minimal read-only plan that
/// just records the snapshot intention and any obviously-needed bootloader
/// edit. Prefer [`plan`] in new code.
pub fn build_plan(profile: &SystemProfile) -> Vec<PlannedStep> {
    let mut steps = vec![snapshot_step()];
    if !profile.iommu_active() {
        if let Some(config_path) = profile.bootloader.config_path.clone() {
            steps.push(PlannedStep {
                kind: StepKind::BootloaderWrite,
                title: "Enable IOMMU kernel parameters".to_string(),
                summary: "Add the Intel or AMD IOMMU parameter to the detected bootloader entry."
                    .to_string(),
                risk: StepRisk::Medium,
                privilege: PrivilegeNeed::RootViaSudo,
                state: StepState::Pending,
                touches: vec![config_path.clone()],
                commands: profile
                    .bootloader
                    .update_command
                    .clone()
                    .map(|c| vec![c])
                    .unwrap_or_default(),
                verification: "Re-read bootloader config and confirm the IOMMU parameter is present after update.".to_string(),
                rollback: "Restore the bootloader config from the snapshot manifest and re-run the bootloader update command.".to_string(),
                requires_reboot: true,
                requires_confirmation: false,
            });
        }
    }
    steps
}

fn snapshot_step() -> PlannedStep {
    let snapshot_dir = virtu_snapshots_dir();
    PlannedStep {
        kind: StepKind::Snapshot,
        title: "Capture rollback snapshot".to_string(),
        summary: "Record the original contents and hashes of every file Virtu may edit, before any mutation.".to_string(),
        risk: StepRisk::ReadOnly,
        privilege: PrivilegeNeed::User,
        state: StepState::Pending,
        touches: vec![snapshot_dir.clone()],
        commands: Vec::new(),
        verification: "Verify the snapshot manifest exists and that every recorded path's pre-edit hash matches the live file.".to_string(),
        rollback: "No rollback is needed for the snapshot step itself; the snapshot is the rollback baseline.".to_string(),
        requires_reboot: false,
        requires_confirmation: false,
    }
}

fn virtu_snapshots_dir() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".virtu")
        .join("snapshots")
}

fn bootloader_step(profile: &SystemProfile, config: &PassthroughConfig) -> Option<PlannedStep> {
    if profile.bootloader.kind == BootloaderKind::Unknown {
        return None;
    }

    let config_path = profile.bootloader.config_path.clone()?;
    let cpu_param = if profile.cpu.vendor.to_lowercase().contains("amd") {
        "amd_iommu=on"
    } else {
        "intel_iommu=on"
    };
    let pass_ids = passthrough_pci_ids(profile, config);

    let needed_params: Vec<&'static str> = if cpu_param == "amd_iommu=on" {
        vec!["amd_iommu=on", "iommu=pt"]
    } else {
        vec!["intel_iommu=on", "iommu=pt"]
    };

    let cmdline = &profile.kernel_cmdline;
    let cmdline_already_has = needed_params
        .iter()
        .all(|param| cmdline.split_whitespace().any(|tok| tok == *param));
    let pci_ids_already_have = if pass_ids.is_empty() {
        true
    } else {
        let token = format!("vfio-pci.ids={}", pass_ids.join(","));
        cmdline.contains(&token)
    };

    let state = if cmdline_already_has && pci_ids_already_have {
        StepState::AlreadySatisfied
    } else {
        StepState::Pending
    };

    let mut params = needed_params
        .iter()
        .map(|p| (*p).to_string())
        .collect::<Vec<_>>();
    if !pass_ids.is_empty() {
        params.push(format!("vfio-pci.ids={}", pass_ids.join(",")));
    }

    let summary = format!(
        "Add IOMMU and VFIO kernel parameters ({}) to the detected {} entry.",
        params.join(" "),
        profile.bootloader.kind
    );

    let mut commands: Vec<String> = Vec::new();
    if let Some(update) = &profile.bootloader.update_command {
        commands.push(update.clone());
    }

    Some(PlannedStep {
        kind: StepKind::BootloaderWrite,
        title: format!("Configure {} kernel parameters", profile.bootloader.kind),
        summary,
        risk: StepRisk::Medium,
        privilege: PrivilegeNeed::RootViaSudo,
        state,
        touches: vec![config_path],
        commands,
        verification: "Re-read the bootloader config and the regenerated cmdline; confirm every required parameter is present.".to_string(),
        rollback: "Restore the bootloader config from the snapshot manifest and re-run the update command.".to_string(),
        requires_reboot: true,
        requires_confirmation: false,
    })
}

fn vfio_step(profile: &SystemProfile, config: &PassthroughConfig) -> Option<PlannedStep> {
    let pass_ids = passthrough_pci_ids(profile, config);
    if pass_ids.is_empty() {
        return None;
    }

    let modprobe_path = PathBuf::from("/etc/modprobe.d/virtu-vfio.conf");
    let modules_loaded = profile
        .readiness
        .loaded_modules
        .iter()
        .any(|module| module == "vfio_pci");

    let any_target_already_bound = config
        .passthrough_gpus(profile)
        .iter()
        .any(|gpu| gpu.current_driver.as_deref() == Some("vfio-pci"));

    let state = if modules_loaded && any_target_already_bound {
        StepState::AlreadySatisfied
    } else {
        StepState::Pending
    };

    Some(PlannedStep {
        kind: StepKind::VfioConfig,
        title: "Write VFIO modprobe configuration".to_string(),
        summary: format!(
            "Write `/etc/modprobe.d/virtu-vfio.conf` so vfio-pci claims devices {} and loads before host GPU drivers.",
            pass_ids.join(",")
        ),
        risk: StepRisk::Medium,
        privilege: PrivilegeNeed::RootViaSudo,
        state,
        touches: vec![modprobe_path],
        commands: Vec::new(),
        verification: "Re-read the modprobe snippet and confirm `lsmod` shows vfio-pci after reboot.".to_string(),
        rollback: "Remove the modprobe snippet and restore any prior file from the snapshot manifest.".to_string(),
        requires_reboot: true,
        requires_confirmation: false,
    })
}

fn initramfs_step(profile: &SystemProfile) -> Option<PlannedStep> {
    let kind = profile.initramfs_system.clone();
    if matches!(kind, crate::detect::initramfs::InitramfsSystem::Unknown) {
        return None;
    }

    let modules_loaded = profile
        .readiness
        .loaded_modules
        .iter()
        .any(|module| module == "vfio_pci");

    let state = if modules_loaded {
        StepState::AlreadySatisfied
    } else {
        StepState::Pending
    };

    let touches = match kind {
        crate::detect::initramfs::InitramfsSystem::Mkinitcpio => {
            vec![PathBuf::from("/etc/mkinitcpio.conf")]
        }
        crate::detect::initramfs::InitramfsSystem::Dracut => {
            vec![PathBuf::from("/etc/dracut.conf.d/virtu-vfio.conf")]
        }
        crate::detect::initramfs::InitramfsSystem::UpdateInitramfs => {
            vec![PathBuf::from("/etc/initramfs-tools/modules")]
        }
        crate::detect::initramfs::InitramfsSystem::Unknown => Vec::new(),
    };

    Some(PlannedStep {
        kind: StepKind::InitramfsWrite,
        title: format!("Add VFIO modules to {}", kind.name()),
        summary: format!(
            "Ensure vfio, vfio_iommu_type1, vfio_pci, and vfio_virqfd load before host GPU drivers, then run `{}`.",
            kind.rebuild_command()
        ),
        risk: StepRisk::Medium,
        privilege: PrivilegeNeed::RootViaSudo,
        state,
        touches,
        commands: vec![kind.rebuild_command().to_string()],
        verification: "Re-read the initramfs config and confirm the rebuild command exited 0.".to_string(),
        rollback: "Restore the initramfs config from the snapshot manifest and re-run the rebuild command.".to_string(),
        requires_reboot: true,
        requires_confirmation: false,
    })
}

fn single_gpu_hook_step(profile: &SystemProfile, config: &PassthroughConfig) -> PlannedStep {
    let qemu_d = PathBuf::from("/etc/libvirt/hooks/qemu.d");
    let dispatcher = qemu_d.join(&config.vm_name);
    let helper_dir = qemu_d.join(format!("{}.d", config.vm_name));
    let release_helper = helper_dir.join("release");
    let reattach_helper = helper_dir.join("reattach");

    PlannedStep {
        kind: StepKind::HookInstall,
        title: format!("Install single-GPU passthrough hooks for `{}`", config.vm_name),
        summary: format!(
            "Generate display-manager-aware libvirt hooks for {}/{} under /etc/libvirt/hooks/qemu.d/{}/. The host display server will be torn down when the VM starts and restored when it stops.",
            profile.display_manager,
            profile.display_server,
            config.vm_name,
        ),
        risk: StepRisk::High,
        privilege: PrivilegeNeed::RootViaSudo,
        state: StepState::Pending,
        touches: vec![dispatcher, release_helper, reattach_helper],
        commands: Vec::new(),
        verification: "Run each generated hook through `bash -n`; confirm the host display manager starts again after a synthetic stop event.".to_string(),
        rollback: "Remove generated hooks and restore the previous /etc/libvirt/hooks layout from the snapshot manifest. The user must reboot if the host display did not return.".to_string(),
        requires_reboot: false,
        requires_confirmation: true,
    }
}

fn looking_glass_step(install_mode: crate::vm::LookingGlassInstallMode) -> PlannedStep {
    match install_mode {
        crate::vm::LookingGlassInstallMode::Manual => PlannedStep {
            kind: StepKind::LookingGlassInstall,
            title: "Defer Looking Glass setup".to_string(),
            summary: "Looking Glass is cut from Virtu v1.0. Preserve the user's manual-setup choice for future compatibility, but do not write host files, emit IVSHMEM XML, or install anything.".to_string(),
            risk: StepRisk::ReadOnly,
            privilege: PrivilegeNeed::User,
            state: StepState::Pending,
            touches: Vec::new(),
            commands: Vec::new(),
            verification: "Read-only deferred step; no v1.0 verification is required.".to_string(),
            rollback: "Read-only step; no rollback needed.".to_string(),
            requires_reboot: false,
            requires_confirmation: false,
        },
        crate::vm::LookingGlassInstallMode::AutoBuild => PlannedStep {
            kind: StepKind::LookingGlassInstall,
            title: "Defer Looking Glass auto-build request".to_string(),
            summary: "Looking Glass auto-build is cut from Virtu v1.0. Preserve the user's request for future compatibility, but do not download source, build a client, write tmpfiles, or emit IVSHMEM XML.".to_string(),
            risk: StepRisk::ReadOnly,
            privilege: PrivilegeNeed::User,
            state: StepState::Pending,
            touches: Vec::new(),
            commands: Vec::new(),
            verification: "Read-only deferred step; no v1.0 verification is required.".to_string(),
            rollback: "Read-only step; no rollback needed.".to_string(),
            requires_reboot: false,
            requires_confirmation: false,
        },
    }
}

fn vm_xml_step(config: &PassthroughConfig) -> PlannedStep {
    let xml_filename = format!("{}.xml", config.vm_name);
    let xml_path: PathBuf = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".virtu")
        .join(&xml_filename);

    let summary = match &config.resources.disk {
        crate::vm::DiskChoice::Create {
            path,
            size_gb,
            format,
        } => format!(
            "Generate libvirt domain XML for `{name}` targeting a new {size} GiB {fmt} image at {path}.",
            name = config.vm_name,
            size = size_gb,
            fmt = format.extension(),
            path = path.display()
        ),
        crate::vm::DiskChoice::Existing { path } => format!(
            "Generate libvirt domain XML for `{name}` referencing the existing disk image at {path}.",
            name = config.vm_name,
            path = path.display()
        ),
    };

    let validate_command = format!("virt-xml-validate {}", xml_path.display());

    PlannedStep {
        kind: StepKind::VmXmlGenerate,
        title: format!("Generate libvirt domain XML for {}", config.vm_name),
        summary,
        risk: StepRisk::Low,
        privilege: PrivilegeNeed::User,
        state: StepState::Pending,
        touches: vec![xml_path.clone()],
        commands: vec![validate_command],
        verification: format!(
            "Run `virt-xml-validate {}` and confirm exit code 0.",
            xml_path.display()
        ),
        rollback: format!(
            "Delete the generated XML file at {}. No host configuration is changed by this step.",
            xml_path.display()
        ),
        requires_reboot: false,
        requires_confirmation: false,
    }
}

fn vm_register_step(config: &PassthroughConfig) -> PlannedStep {
    let needs_disk_create = matches!(config.resources.disk, crate::vm::DiskChoice::Create { .. });

    let mut commands = Vec::new();
    if let crate::vm::DiskChoice::Create {
        path,
        size_gb,
        format,
    } = &config.resources.disk
    {
        commands.push(format!(
            "qemu-img create -f {fmt} {path} {size}G",
            fmt = format.extension(),
            path = path.display(),
            size = size_gb
        ));
    }
    commands.push(format!("virsh define ~/.virtu/{}.xml", config.vm_name));

    let summary = if needs_disk_create {
        format!(
            "Create the VM disk image with `qemu-img create`, then register libvirt domain `{name}` with `virsh define`.",
            name = config.vm_name
        )
    } else {
        format!(
            "Register libvirt domain `{name}` with `virsh define`.",
            name = config.vm_name
        )
    };

    PlannedStep {
        kind: StepKind::VmRegister,
        title: format!("Register libvirt domain `{}`", config.vm_name),
        summary,
        risk: StepRisk::Low,
        privilege: PrivilegeNeed::User,
        state: StepState::Pending,
        touches: match &config.resources.disk {
            crate::vm::DiskChoice::Create { path, .. } => vec![path.clone()],
            crate::vm::DiskChoice::Existing { .. } => Vec::new(),
        },
        commands,
        verification: format!(
            "Run `virsh dominfo {name}` and confirm the domain exists; `virsh list --all` should include `{name}`.",
            name = config.vm_name
        ),
        rollback: format!(
            "Run `virsh undefine {name}` and remove the generated disk image if it was created in this step.",
            name = config.vm_name
        ),
        requires_reboot: false,
        requires_confirmation: false,
    }
}

fn verify_step() -> PlannedStep {
    PlannedStep {
        kind: StepKind::Verify,
        title: "Verify final state".to_string(),
        summary: "Re-scan IOMMU groups, vfio-pci binding, and libvirt domain visibility."
            .to_string(),
        risk: StepRisk::ReadOnly,
        privilege: PrivilegeNeed::User,
        state: StepState::Pending,
        touches: Vec::new(),
        commands: Vec::new(),
        verification: "All targeted facts are reported as healthy; the user is shown what reboot or service restart, if any, is still required.".to_string(),
        rollback: "Verification itself is read-only; rollback is the previous step's responsibility.".to_string(),
        requires_reboot: false,
        requires_confirmation: false,
    }
}

/// PCI ids the user wants passed through, sorted lexicographically and
/// deduplicated. Mirrors what `engine::executor::passthrough_pci_ids`
/// produces post-sort, so the plan's printed `vfio-pci.ids=...`
/// description matches the bytes the executor will eventually write
/// to `/etc/default/grub` and `/etc/modprobe.d/virtu-vfio.conf`. The
/// `pci_ids_already_have` substring check below also depends on this
/// ordering: the bootloader file is always sorted, so re-planning a
/// host that already had Phase A applied must build the same sorted
/// token to detect the `AlreadySatisfied` state.
fn passthrough_pci_ids(profile: &SystemProfile, config: &PassthroughConfig) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for assignment in &config.gpu_roles {
        if assignment.role != GpuRole::Passthrough {
            continue;
        }
        let Some(gpu) = profile
            .gpus
            .iter()
            .find(|gpu| gpu.pci_slot == assignment.pci_slot)
        else {
            continue;
        };
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
    ids.sort();
    ids.dedup();
    ids
}

fn single_gpu_hook_handoff(config: &PassthroughConfig) -> bool {
    matches!(
        config.monitor_plan,
        MonitorPlan::OneMonitor {
            strategy: SingleMonitorStrategy::HookHandoff
        }
    )
}

fn summarize(steps: &[PlannedStep]) -> PlanSummary {
    let total_steps = steps.len();
    let pending_steps = steps
        .iter()
        .filter(|step| step.state == StepState::Pending)
        .count();
    let already_satisfied_steps = total_steps - pending_steps;
    let max_risk = steps
        .iter()
        .map(|step| step.risk)
        .max_by(|a, b| risk_rank(*a).cmp(&risk_rank(*b)))
        .unwrap_or(StepRisk::ReadOnly);
    let requires_reboot = steps.iter().any(|step| step.requires_reboot);
    let requires_confirmation = steps.iter().any(|step| step.must_confirm());

    PlanSummary {
        total_steps,
        pending_steps,
        already_satisfied_steps,
        max_risk,
        requires_reboot,
        requires_confirmation,
    }
}

fn risk_rank(risk: StepRisk) -> u8 {
    match risk {
        StepRisk::ReadOnly => 0,
        StepRisk::Low => 1,
        StepRisk::Medium => 2,
        StepRisk::High => 3,
    }
}
