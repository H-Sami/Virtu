use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// What kind of work a step performs. The exact verb is captured in the
/// step's title; this enum is for grouping and ordering only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepKind {
    /// Capture rollback data before any mutation.
    Snapshot,
    /// Edit the bootloader configuration to add IOMMU/VFIO kernel parameters.
    BootloaderWrite,
    /// Write a `/etc/modprobe.d/` snippet for `vfio-pci` IDs and module
    /// ordering.
    VfioConfig,
    /// Edit initramfs configuration so the VFIO modules load before host GPU
    /// drivers and rebuild the initramfs image.
    InitramfsWrite,
    /// Install or update display-manager-aware libvirt hooks for single-GPU
    /// passthrough.
    HookInstall,
    /// Generate the libvirt domain XML.
    VmXmlGenerate,
    /// Register the libvirt domain with `virsh define`.
    VmRegister,
    /// Install or compile Looking Glass.
    LookingGlassInstall,
    /// Final verification step that re-reads system state and confirms the
    /// plan landed.
    Verify,
}

impl StepKind {
    pub fn label(&self) -> &'static str {
        match self {
            StepKind::Snapshot => "snapshot",
            StepKind::BootloaderWrite => "bootloader",
            StepKind::VfioConfig => "vfio",
            StepKind::InitramfsWrite => "initramfs",
            StepKind::HookInstall => "hooks",
            StepKind::VmXmlGenerate => "vm-xml",
            StepKind::VmRegister => "vm-register",
            StepKind::LookingGlassInstall => "looking-glass",
            StepKind::Verify => "verify",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepRisk {
    /// Reads only; cannot leave the host in a worse state.
    ReadOnly,
    /// Writes user-scoped files; reversible without root.
    Low,
    /// Writes system files; reversible only via snapshot manifest.
    Medium,
    /// Touches code paths that can leave the host without a usable display
    /// (single-GPU hooks, Secure Boot module signing, ACS override, etc.).
    High,
}

impl std::fmt::Display for StepRisk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StepRisk::ReadOnly => write!(f, "read-only"),
            StepRisk::Low => write!(f, "low"),
            StepRisk::Medium => write!(f, "medium"),
            StepRisk::High => write!(f, "high"),
        }
    }
}

/// What privilege level a step needs to actually run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrivilegeNeed {
    /// Runs as the invoking user.
    User,
    /// Needs targeted privilege escalation for one or more commands.
    RootViaSudo,
}

impl std::fmt::Display for PrivilegeNeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrivilegeNeed::User => write!(f, "user"),
            PrivilegeNeed::RootViaSudo => write!(f, "root via sudo"),
        }
    }
}

/// Tracks whether a planned step still needs to run, or whether the host is
/// already in the target state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepState {
    /// Mutation is required.
    Pending,
    /// Already in the target state on this host. The step is kept in the plan
    /// for transparency but the executor can skip it.
    AlreadySatisfied,
}

impl std::fmt::Display for StepState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StepState::Pending => write!(f, "pending"),
            StepState::AlreadySatisfied => write!(f, "already satisfied"),
        }
    }
}

/// One ordered step in a Virtu plan. Every mutating step must declare its
/// touched files, commands, privilege need, verification description, and
/// rollback behavior up front. Read-only steps still declare the same fields
/// for consistency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedStep {
    pub kind: StepKind,
    pub title: String,
    pub summary: String,
    pub risk: StepRisk,
    pub privilege: PrivilegeNeed,
    pub state: StepState,
    /// File paths the step will read or write. Pure-read steps may list paths
    /// they consult; mutating steps must list every path they touch.
    pub touches: Vec<PathBuf>,
    /// Shell commands the step will run, in order.
    pub commands: Vec<String>,
    /// Plain-language description of how the step will be verified after it
    /// runs.
    pub verification: String,
    /// Plain-language description of what rollback will do if this step is
    /// the last one applied before failure. "Snapshot restore" is the
    /// default; richer descriptions help the diagnostics layer.
    pub rollback: String,
    pub requires_reboot: bool,
    pub requires_confirmation: bool,
}

impl PlannedStep {
    /// Returns `true` if the executor should ask the user for confirmation
    /// before running this step.
    pub fn must_confirm(&self) -> bool {
        self.requires_confirmation || matches!(self.risk, StepRisk::High)
    }
}
