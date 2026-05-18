//! Read-only validation of user [`PassthroughConfig`] choices against the
//! detected [`SystemProfile`] and the engine's [`CompatibilityReport`].
//!
//! Validation never mutates host state. It produces issues classified as
//! [`ValidationSeverity::Error`] (the choice is impossible or unsafe and must
//! be changed before any plan can be generated) or
//! [`ValidationSeverity::Warning`] (the choice is allowed but the user should
//! confirm a real risk).
//!
//! Validation preserves user intent: ignored GPUs stay ignored, single-GPU
//! mode is allowed even when other GPUs exist, and the user may still choose
//! Looking Glass auto-build provided the Looking Glass milestone has not yet
//! produced an installer.

use crate::detect::gpu::GpuInfo;
use crate::detect::SystemProfile;
use crate::engine::{CompatibilityReport, FindingSeverity};
use crate::vm::passthrough::{
    AudioChoice, DiskChoice, GpuPassthroughMode, GpuRole, LookingGlassChoice, MonitorPlan,
    NetworkChoice, PassthroughConfig, SingleMonitorStrategy,
};
use serde::{Deserialize, Serialize};

/// Severity of a single validation issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValidationSeverity {
    /// The user's choice is impossible or unsafe. The plan must not run.
    Error,
    /// The user's choice is allowed, but the user should confirm a real risk.
    Warning,
}

impl std::fmt::Display for ValidationSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationSeverity::Error => write!(f, "ERROR"),
            ValidationSeverity::Warning => write!(f, "WARN"),
        }
    }
}

/// Stable identifiers for validation issues. Keeping these as a closed enum
/// makes test assertions and TUI mapping straightforward.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValidationIssueId {
    CompatibilityBlocked,
    GpuRolesEmpty,
    GpuRoleSlotUnknown,
    GpuRoleDuplicate,
    GpuRoleMissing,
    GpuModeMismatch,
    PassthroughGpuNotIsolated,
    HostGpuPassthroughBound,
    SingleGpuRiskAcknowledged,
    MultiGpuNotImplemented,
    MonitorConnectorUnknown,
    MonitorConnectorsCollide,
    MonitorPlanNeedsTwoMonitors,
    LookingGlassRequiresPassthrough,
    LookingGlassResolutionInvalid,
    LookingGlassAutoBuildNotImplemented,
    HookHandoffRequiresSingleGpu,
    IsoMissing,
    DiskPathExistsForCreate,
    DiskPathMissingForExisting,
    DiskTooSmall,
    StorageInsufficient,
    RamTooSmall,
    RamExceedsHost,
    VcpuTooLow,
    VcpuExceedsHost,
    AudioBackendMissing,
    AudioBackendNotPipeable,
    NetworkInterfaceMissing,
    EvdevPathUnknown,
    EvdevPathDuplicated,
}

impl std::fmt::Display for ValidationIssueId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ValidationIssueId::CompatibilityBlocked => "compatibility-blocked",
            ValidationIssueId::GpuRolesEmpty => "gpu-roles-empty",
            ValidationIssueId::GpuRoleSlotUnknown => "gpu-role-slot-unknown",
            ValidationIssueId::GpuRoleDuplicate => "gpu-role-duplicate",
            ValidationIssueId::GpuRoleMissing => "gpu-role-missing",
            ValidationIssueId::GpuModeMismatch => "gpu-mode-mismatch",
            ValidationIssueId::PassthroughGpuNotIsolated => "passthrough-gpu-not-isolated",
            ValidationIssueId::HostGpuPassthroughBound => "host-gpu-passthrough-bound",
            ValidationIssueId::SingleGpuRiskAcknowledged => "single-gpu-risk-acknowledged",
            ValidationIssueId::MultiGpuNotImplemented => "multi-gpu-not-implemented",
            ValidationIssueId::MonitorConnectorUnknown => "monitor-connector-unknown",
            ValidationIssueId::MonitorConnectorsCollide => "monitor-connectors-collide",
            ValidationIssueId::MonitorPlanNeedsTwoMonitors => "monitor-plan-needs-two-monitors",
            ValidationIssueId::LookingGlassRequiresPassthrough => {
                "looking-glass-requires-passthrough"
            }
            ValidationIssueId::LookingGlassResolutionInvalid => "looking-glass-resolution-invalid",
            ValidationIssueId::LookingGlassAutoBuildNotImplemented => {
                "looking-glass-auto-build-not-implemented"
            }
            ValidationIssueId::HookHandoffRequiresSingleGpu => "hook-handoff-requires-single-gpu",
            ValidationIssueId::IsoMissing => "iso-missing",
            ValidationIssueId::DiskPathExistsForCreate => "disk-path-exists-for-create",
            ValidationIssueId::DiskPathMissingForExisting => "disk-path-missing-for-existing",
            ValidationIssueId::DiskTooSmall => "disk-too-small",
            ValidationIssueId::StorageInsufficient => "storage-insufficient",
            ValidationIssueId::RamTooSmall => "ram-too-small",
            ValidationIssueId::RamExceedsHost => "ram-exceeds-host",
            ValidationIssueId::VcpuTooLow => "vcpu-too-low",
            ValidationIssueId::VcpuExceedsHost => "vcpu-exceeds-host",
            ValidationIssueId::AudioBackendMissing => "audio-backend-missing",
            ValidationIssueId::AudioBackendNotPipeable => "audio-backend-not-pipeable",
            ValidationIssueId::NetworkInterfaceMissing => "network-interface-missing",
            ValidationIssueId::EvdevPathUnknown => "evdev-path-unknown",
            ValidationIssueId::EvdevPathDuplicated => "evdev-path-duplicated",
        };
        write!(f, "{s}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationIssue {
    pub id: ValidationIssueId,
    pub severity: ValidationSeverity,
    pub message: String,
}

impl ValidationIssue {
    fn error(id: ValidationIssueId, message: impl Into<String>) -> Self {
        Self {
            id,
            severity: ValidationSeverity::Error,
            message: message.into(),
        }
    }

    fn warn(id: ValidationIssueId, message: impl Into<String>) -> Self {
        Self {
            id,
            severity: ValidationSeverity::Warning,
            message: message.into(),
        }
    }
}

/// Aggregated outcome of validating a [`PassthroughConfig`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ValidationReport {
    pub issues: Vec<ValidationIssue>,
}

impl ValidationReport {
    pub fn is_ok(&self) -> bool {
        !self.has_errors()
    }

    pub fn has_errors(&self) -> bool {
        self.issues
            .iter()
            .any(|issue| issue.severity == ValidationSeverity::Error)
    }

    pub fn has_warnings(&self) -> bool {
        self.issues
            .iter()
            .any(|issue| issue.severity == ValidationSeverity::Warning)
    }

    pub fn errors(&self) -> impl Iterator<Item = &ValidationIssue> {
        self.issues
            .iter()
            .filter(|issue| issue.severity == ValidationSeverity::Error)
    }

    pub fn warnings(&self) -> impl Iterator<Item = &ValidationIssue> {
        self.issues
            .iter()
            .filter(|issue| issue.severity == ValidationSeverity::Warning)
    }

    pub fn has_issue(&self, id: ValidationIssueId) -> bool {
        self.issues.iter().any(|issue| issue.id == id)
    }

    pub fn issue(&self, id: ValidationIssueId) -> Option<&ValidationIssue> {
        self.issues.iter().find(|issue| issue.id == id)
    }
}

/// Validate a [`PassthroughConfig`] without mutating any host state.
pub fn validate(
    profile: &SystemProfile,
    report: &CompatibilityReport,
    config: &PassthroughConfig,
) -> ValidationReport {
    let mut issues = Vec::new();

    propagate_blockers(report, &mut issues);
    let mode = check_gpu_roles(profile, config, &mut issues);
    check_monitors(profile, config, &mut issues);
    check_looking_glass(config, mode, &mut issues);
    check_resources(profile, config, &mut issues);
    check_audio(profile, config, &mut issues);
    check_network(profile, config, &mut issues);
    check_input(config, &mut issues);
    check_iso(config, &mut issues);

    ValidationReport { issues }
}

fn propagate_blockers(report: &CompatibilityReport, issues: &mut Vec<ValidationIssue>) {
    let blockers: Vec<&str> = report
        .findings
        .iter()
        .filter(|finding| finding.severity == FindingSeverity::Fail)
        .map(|finding| finding.id.as_str())
        .collect();

    if blockers.is_empty() {
        return;
    }

    issues.push(ValidationIssue::error(
        ValidationIssueId::CompatibilityBlocked,
        format!(
            "Compatibility report has {} blocker(s) that must be resolved before any plan: {}",
            blockers.len(),
            blockers.join(", ")
        ),
    ));
}

fn check_gpu_roles(
    profile: &SystemProfile,
    config: &PassthroughConfig,
    issues: &mut Vec<ValidationIssue>,
) -> Option<GpuPassthroughMode> {
    if config.gpu_roles.is_empty() {
        issues.push(ValidationIssue::error(
            ValidationIssueId::GpuRolesEmpty,
            "No GPU role assignments were provided. Assign each detected GPU to a role.",
        ));
        return None;
    }

    let mut seen: Vec<&str> = Vec::new();
    for role in &config.gpu_roles {
        if seen.contains(&role.pci_slot.as_str()) {
            issues.push(ValidationIssue::error(
                ValidationIssueId::GpuRoleDuplicate,
                format!(
                    "GPU at PCI slot {} appears in role assignments more than once.",
                    role.pci_slot
                ),
            ));
        } else {
            seen.push(&role.pci_slot);
        }

        if !profile.gpus.iter().any(|gpu| gpu.pci_slot == role.pci_slot) {
            issues.push(ValidationIssue::error(
                ValidationIssueId::GpuRoleSlotUnknown,
                format!(
                    "PCI slot {} is not present in the detected system profile.",
                    role.pci_slot
                ),
            ));
        }
    }

    for gpu in &profile.gpus {
        if !config
            .gpu_roles
            .iter()
            .any(|role| role.pci_slot == gpu.pci_slot)
        {
            issues.push(ValidationIssue::error(
                ValidationIssueId::GpuRoleMissing,
                format!(
                    "Detected GPU at {} ({}) has no role assignment. Mark it Host, Passthrough, or Ignored.",
                    gpu.pci_slot, gpu.model_name
                ),
            ));
        }
    }

    let pass_assignments: Vec<_> = config
        .gpu_roles
        .iter()
        .filter(|role| role.role == GpuRole::Passthrough)
        .collect();
    let host_assignments: Vec<_> = config
        .gpu_roles
        .iter()
        .filter(|role| role.role == GpuRole::Host)
        .collect();

    if pass_assignments.is_empty() {
        issues.push(ValidationIssue::error(
            ValidationIssueId::GpuRoleMissing,
            "At least one GPU must be assigned the Passthrough role.",
        ));
    }

    for assignment in &pass_assignments {
        let Some(gpu) = lookup_gpu(profile, &assignment.pci_slot) else {
            continue;
        };
        if !gpu.iommu_isolated {
            issues.push(ValidationIssue::error(
                ValidationIssueId::PassthroughGpuNotIsolated,
                format!(
                    "GPU {} ({}) is not isolated in its IOMMU group and cannot be passed through safely.",
                    gpu.pci_slot, gpu.model_name
                ),
            ));
        }
    }

    for assignment in &host_assignments {
        let Some(gpu) = lookup_gpu(profile, &assignment.pci_slot) else {
            continue;
        };
        if gpu.current_driver.as_deref() == Some("vfio-pci") {
            issues.push(ValidationIssue::error(
                ValidationIssueId::HostGpuPassthroughBound,
                format!(
                    "GPU {} is currently bound to vfio-pci and cannot be the host GPU until it is rebound.",
                    gpu.pci_slot
                ),
            ));
        }
    }

    let derived = config.derived_mode(profile);
    if let Some(derived_mode) = derived {
        if derived_mode != config.gpu_mode {
            issues.push(ValidationIssue::error(
                ValidationIssueId::GpuModeMismatch,
                format!(
                    "Stated GPU mode `{}` does not match assignments which describe `{}`.",
                    config.gpu_mode, derived_mode
                ),
            ));
        }

        match derived_mode {
            GpuPassthroughMode::SingleGpu => {
                issues.push(ValidationIssue::warn(
                    ValidationIssueId::SingleGpuRiskAcknowledged,
                    "Single-GPU passthrough requires display-manager-aware hooks and is supported only after rollback is reliable.",
                ));
            }
            GpuPassthroughMode::MultiGpu => {
                issues.push(ValidationIssue::error(
                    ValidationIssueId::MultiGpuNotImplemented,
                    "Multiple Passthrough GPUs are not supported by the current automation path.",
                ));
            }
            _ => {}
        }
    }

    derived
}

fn check_monitors(
    profile: &SystemProfile,
    config: &PassthroughConfig,
    issues: &mut Vec<ValidationIssue>,
) {
    let connected: Vec<_> = profile.monitors.iter().filter(|m| m.connected).collect();

    match &config.monitor_plan {
        MonitorPlan::OneMonitor { strategy } => {
            if matches!(strategy, SingleMonitorStrategy::HookHandoff) {
                let derived = config.derived_mode(profile);
                if !matches!(derived, Some(GpuPassthroughMode::SingleGpu)) {
                    issues.push(ValidationIssue::error(
                        ValidationIssueId::HookHandoffRequiresSingleGpu,
                        "Hook-based monitor hand-off only applies to single-GPU passthrough.",
                    ));
                }
            }
        }
        MonitorPlan::TwoMonitors {
            host_connector,
            vm_connector,
        } => {
            if host_connector == vm_connector {
                issues.push(ValidationIssue::error(
                    ValidationIssueId::MonitorConnectorsCollide,
                    "Two-monitor plans must use two different connectors.",
                ));
            }

            for connector in [host_connector, vm_connector] {
                let known = profile
                    .monitors
                    .iter()
                    .any(|monitor| monitor.connector_name == *connector);
                if !known {
                    issues.push(ValidationIssue::error(
                        ValidationIssueId::MonitorConnectorUnknown,
                        format!(
                            "Connector `{connector}` is not present in the detected DRM monitor list."
                        ),
                    ));
                }
            }

            if connected.len() < 2 {
                issues.push(ValidationIssue::warn(
                    ValidationIssueId::MonitorPlanNeedsTwoMonitors,
                    "Two-monitor plan was selected but only one connector is currently reported as connected.",
                ));
            }
        }
    }
}

fn check_looking_glass(
    config: &PassthroughConfig,
    mode: Option<GpuPassthroughMode>,
    issues: &mut Vec<ValidationIssue>,
) {
    let LookingGlassChoice::Enabled {
        install_mode,
        target_resolution,
    } = &config.looking_glass
    else {
        return;
    };

    if mode.is_none() {
        issues.push(ValidationIssue::error(
            ValidationIssueId::LookingGlassRequiresPassthrough,
            "Looking Glass requires at least one Passthrough GPU.",
        ));
    }

    if target_resolution.width == 0 || target_resolution.height == 0 {
        issues.push(ValidationIssue::error(
            ValidationIssueId::LookingGlassResolutionInvalid,
            "Looking Glass target resolution must have non-zero width and height.",
        ));
    }

    if matches!(
        install_mode,
        crate::vm::passthrough::LookingGlassInstallMode::AutoBuild
    ) {
        issues.push(ValidationIssue::warn(
            ValidationIssueId::LookingGlassAutoBuildNotImplemented,
            "Looking Glass auto-build will only run after explicit consent and once the installer milestone ships. Choose Manual until then.",
        ));
    }
}

fn check_resources(
    profile: &SystemProfile,
    config: &PassthroughConfig,
    issues: &mut Vec<ValidationIssue>,
) {
    let host_ram_mb = profile.ram.total_kb / 1024;
    let host_threads = profile.cpu.logical_cores.max(1);

    if config.resources.ram_mb < 2048 {
        issues.push(ValidationIssue::error(
            ValidationIssueId::RamTooSmall,
            "VM RAM must be at least 2048 MiB.",
        ));
    }
    if host_ram_mb > 0 && config.resources.ram_mb >= host_ram_mb {
        issues.push(ValidationIssue::error(
            ValidationIssueId::RamExceedsHost,
            format!(
                "Requested {} MiB of VM RAM is not available; host has {} MiB total.",
                config.resources.ram_mb, host_ram_mb
            ),
        ));
    } else if host_ram_mb > 0 && config.resources.ram_mb + 2048 > host_ram_mb {
        issues.push(ValidationIssue::warn(
            ValidationIssueId::RamExceedsHost,
            format!(
                "VM RAM {} MiB leaves less than 2 GiB for the host (total {} MiB).",
                config.resources.ram_mb, host_ram_mb
            ),
        ));
    }

    if config.resources.vcpu_count < 2 {
        issues.push(ValidationIssue::error(
            ValidationIssueId::VcpuTooLow,
            "VM must have at least 2 vCPUs.",
        ));
    }
    if config.resources.vcpu_count >= host_threads {
        issues.push(ValidationIssue::error(
            ValidationIssueId::VcpuExceedsHost,
            format!(
                "Requested {} vCPUs leaves no logical cores for the host (host has {}).",
                config.resources.vcpu_count, host_threads
            ),
        ));
    }

    let storage_available_gb = profile.storage.available_gb();

    match &config.resources.disk {
        DiskChoice::Create {
            path,
            size_gb,
            format: _,
        } => {
            if *size_gb < 30 {
                issues.push(ValidationIssue::warn(
                    ValidationIssueId::DiskTooSmall,
                    format!("Disk size {size_gb} GiB is unusually small for a Windows gaming VM."),
                ));
            }
            if path.exists() {
                issues.push(ValidationIssue::error(
                    ValidationIssueId::DiskPathExistsForCreate,
                    format!(
                        "Disk path {} already exists. Choose Existing instead of Create, or pick a new path.",
                        path.display()
                    ),
                ));
            }
            if storage_available_gb > 0 && *size_gb > storage_available_gb {
                issues.push(ValidationIssue::error(
                    ValidationIssueId::StorageInsufficient,
                    format!(
                        "Requested {} GiB exceeds detected free space ({} GiB) in {}.",
                        size_gb,
                        storage_available_gb,
                        profile.storage.default_vm_dir.display()
                    ),
                ));
            }
        }
        DiskChoice::Existing { path } => {
            if !path.exists() {
                issues.push(ValidationIssue::error(
                    ValidationIssueId::DiskPathMissingForExisting,
                    format!("Existing disk path {} could not be found.", path.display()),
                ));
            }
        }
    }
}

fn check_audio(
    profile: &SystemProfile,
    config: &PassthroughConfig,
    issues: &mut Vec<ValidationIssue>,
) {
    if matches!(config.audio, AudioChoice::HostAudio) {
        match profile.audio {
            crate::detect::audio::AudioSystem::PipeWire
            | crate::detect::audio::AudioSystem::PulseAudio => {}
            crate::detect::audio::AudioSystem::Unknown => {
                issues.push(ValidationIssue::error(
                    ValidationIssueId::AudioBackendMissing,
                    "Host-audio passthrough was selected but no PipeWire/PulseAudio backend was detected.",
                ));
            }
            _ => {
                issues.push(ValidationIssue::warn(
                    ValidationIssueId::AudioBackendNotPipeable,
                    format!(
                        "Detected audio backend {} cannot pipe directly into libvirt; consider Scream or None.",
                        profile.audio
                    ),
                ));
            }
        }
    }
}

fn check_network(
    profile: &SystemProfile,
    config: &PassthroughConfig,
    issues: &mut Vec<ValidationIssue>,
) {
    if let NetworkChoice::Bridge { interface } = &config.network {
        if interface.trim().is_empty() {
            issues.push(ValidationIssue::error(
                ValidationIssueId::NetworkInterfaceMissing,
                "Bridge networking was selected without a host interface name.",
            ));
        }
    }

    let _ = profile;
}

fn check_input(config: &PassthroughConfig, issues: &mut Vec<ValidationIssue>) {
    let mut seen: Vec<String> = Vec::new();
    for path in config.input.all_evdev_paths() {
        if !path.exists() {
            issues.push(ValidationIssue::warn(
                ValidationIssueId::EvdevPathUnknown,
                format!(
                    "Evdev device path {} does not exist on this host. Confirm the device is plugged in.",
                    path.display()
                ),
            ));
        }
        let key = path.display().to_string();
        if seen.contains(&key) {
            issues.push(ValidationIssue::error(
                ValidationIssueId::EvdevPathDuplicated,
                format!("Evdev path {key} is selected more than once."),
            ));
        } else {
            seen.push(key);
        }
    }
}

fn check_iso(config: &PassthroughConfig, issues: &mut Vec<ValidationIssue>) {
    if let Some(path) = &config.iso_path {
        if !path.exists() {
            issues.push(ValidationIssue::error(
                ValidationIssueId::IsoMissing,
                format!("Selected ISO {} could not be found.", path.display()),
            ));
        }
    }
}

fn lookup_gpu<'a>(profile: &'a SystemProfile, slot: &str) -> Option<&'a GpuInfo> {
    profile.gpus.iter().find(|gpu| gpu.pci_slot == slot)
}
