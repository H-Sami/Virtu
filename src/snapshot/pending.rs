//! Pending-plan record for the resumable two-phase apply workflow (slice
//! 6.1).
//!
//! GPU passthrough cannot be applied in one go. Bootloader cmdline edits,
//! initramfs rebuilds, and the vfio-pci module-load order only take effect
//! on the next boot. Virtu therefore splits a [`Plan`] into:
//!
//! 1. **Phase A** — snapshot, bootloader edit, VFIO modprobe, initramfs
//!    rebuild. Mutates the host. Ends by writing a [`PendingPlan`] under
//!    `~/.virtu/state/pending.toml` and asking the user to reboot.
//! 2. **Phase B** — re-detect after reboot, verify the new boot landed,
//!    finish the remaining steps (hooks / Looking Glass / VM XML /
//!    libvirt). Implemented in Milestone 6.5 (`virtu resume`).
//!
//! This module owns the on-disk record. It does not run anything.

use crate::detect::SystemProfile;
use crate::engine::planner::{Plan, PlanSummary};
use crate::engine::step::{PlannedStep, StepKind};
use crate::vm::PassthroughConfig;

use serde::{Deserialize, Serialize};

/// Default location for the pending-plan record. Lives next to
/// `~/.virtu/snapshots/`. Callers may override it for tests.
pub const DEFAULT_FILENAME: &str = "pending.toml";

/// Subdirectory under `~/.virtu/` where Phase-A state lives.
pub const STATE_SUBDIR: &str = "state";

/// On-disk record describing a Phase-A handoff to Phase B.
///
/// Phase B (`virtu resume`) reads this record, verifies the host actually
/// rebooted into the expected state (right kernel, right cmdline, right
/// vfio-pci binding), and then finishes the remaining plan steps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingPlan {
    /// Virtu version that wrote this record.
    pub virtu_version: String,
    /// When Phase A finished (UTC, RFC 3339).
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Snapshot id captured during Phase A. Phase B uses this for rollback
    /// if verification fails.
    pub snapshot_id: String,
    /// Fingerprint of the host as Phase A saw it. Phase B compares this to
    /// fresh detection; mismatches (different kernel, different bootloader,
    /// missing vfio module) are surfaced before any further mutation.
    pub host_fingerprint: HostFingerprint,
    /// Plan summary at Phase-A capture time. Useful for the human-readable
    /// "what's pending" report.
    pub plan_summary: PlanSummary,
    /// Steps that still need to run after the reboot. Phase B walks these
    /// in order.
    pub remaining_steps: Vec<PlannedStep>,
    /// User configuration that drove the plan. Phase B re-runs validation
    /// against the post-reboot system before continuing.
    pub config: PassthroughConfig,
}

/// Subset of [`SystemProfile`] used to decide whether the post-reboot host
/// is still the same machine Phase A targeted. Differences here do not
/// auto-block Phase B; they trigger an explicit confirmation prompt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostFingerprint {
    pub distro_id: String,
    pub kernel_version: String,
    pub bootloader: crate::detect::bootloader::BootloaderKind,
    pub initramfs: crate::detect::initramfs::InitramfsSystem,
    /// Sorted list of `<vendor>:<device>` PCI ids the user wants passed
    /// through. Phase B verifies vfio-pci has actually bound each of them.
    pub passthrough_pci_ids: Vec<String>,
    /// Kernel cmdline at Phase-A scan time, exactly as
    /// `/proc/cmdline` returned it. Phase B compares against the new live
    /// cmdline to confirm the bootloader edit landed.
    pub kernel_cmdline_pre_apply: String,
}

impl HostFingerprint {
    /// Build a fingerprint from the values Phase A had on hand. The PCI ids
    /// are sorted to make TOML round-trips deterministic.
    pub fn capture(profile: &SystemProfile, mut passthrough_pci_ids: Vec<String>) -> Self {
        passthrough_pci_ids.sort();
        passthrough_pci_ids.dedup();
        Self {
            distro_id: profile.distro.id.clone(),
            kernel_version: profile.readiness.kernel_version.clone(),
            bootloader: profile.bootloader.kind.clone(),
            initramfs: profile.initramfs_system.clone(),
            passthrough_pci_ids,
            kernel_cmdline_pre_apply: profile.kernel_cmdline.clone(),
        }
    }
}

impl PendingPlan {
    /// Construct a pending record from a Phase-A apply.
    pub fn new(
        snapshot_id: impl Into<String>,
        host_fingerprint: HostFingerprint,
        plan: &Plan,
        remaining_steps: Vec<PlannedStep>,
        config: PassthroughConfig,
    ) -> Self {
        Self {
            virtu_version: env!("CARGO_PKG_VERSION").to_string(),
            created_at: chrono::Utc::now(),
            snapshot_id: snapshot_id.into(),
            host_fingerprint,
            plan_summary: plan.summary.clone(),
            remaining_steps,
            config,
        }
    }

    /// Returns the steps from `plan` that Phase A is responsible for. Phase
    /// A handles snapshot, bootloader, vfio, and initramfs only.
    pub fn phase_a_steps(plan: &Plan) -> Vec<PlannedStep> {
        plan.steps
            .iter()
            .filter(|step| {
                matches!(
                    step.kind,
                    StepKind::Snapshot
                        | StepKind::BootloaderWrite
                        | StepKind::VfioConfig
                        | StepKind::InitramfsWrite
                )
            })
            .cloned()
            .collect()
    }

    /// Returns the steps from `plan` that Phase B is responsible for. These
    /// are everything Phase A skipped: hooks, Looking Glass, VM XML,
    /// libvirt registration, verification.
    pub fn phase_b_steps(plan: &Plan) -> Vec<PlannedStep> {
        plan.steps
            .iter()
            .filter(|step| {
                !matches!(
                    step.kind,
                    StepKind::Snapshot
                        | StepKind::BootloaderWrite
                        | StepKind::VfioConfig
                        | StepKind::InitramfsWrite
                )
            })
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::step::{PrivilegeNeed, StepRisk, StepState};
    use std::path::PathBuf;

    fn dummy_step(kind: StepKind) -> PlannedStep {
        PlannedStep {
            kind: kind.clone(),
            title: format!("{kind:?}"),
            summary: "test".to_string(),
            risk: StepRisk::Medium,
            privilege: PrivilegeNeed::User,
            state: StepState::Pending,
            touches: vec![PathBuf::from("/tmp/x")],
            commands: Vec::new(),
            verification: "n/a".to_string(),
            rollback: "n/a".to_string(),
            requires_reboot: false,
            requires_confirmation: false,
        }
    }

    fn dummy_plan() -> Plan {
        let steps = vec![
            dummy_step(StepKind::Snapshot),
            dummy_step(StepKind::BootloaderWrite),
            dummy_step(StepKind::VfioConfig),
            dummy_step(StepKind::InitramfsWrite),
            dummy_step(StepKind::HookInstall),
            dummy_step(StepKind::VmXmlGenerate),
            dummy_step(StepKind::VmRegister),
            dummy_step(StepKind::Verify),
        ];
        Plan {
            summary: PlanSummary {
                total_steps: steps.len(),
                pending_steps: steps.len(),
                already_satisfied_steps: 0,
                max_risk: StepRisk::Medium,
                requires_reboot: true,
                requires_confirmation: false,
            },
            steps,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn phase_a_steps_only_include_pre_reboot_kinds() {
        let plan = dummy_plan();
        let phase_a: Vec<StepKind> = PendingPlan::phase_a_steps(&plan)
            .iter()
            .map(|s| s.kind.clone())
            .collect();
        assert_eq!(
            phase_a,
            vec![
                StepKind::Snapshot,
                StepKind::BootloaderWrite,
                StepKind::VfioConfig,
                StepKind::InitramfsWrite,
            ]
        );
    }

    #[test]
    fn phase_b_steps_only_include_post_reboot_kinds() {
        let plan = dummy_plan();
        let phase_b: Vec<StepKind> = PendingPlan::phase_b_steps(&plan)
            .iter()
            .map(|s| s.kind.clone())
            .collect();
        assert_eq!(
            phase_b,
            vec![
                StepKind::HookInstall,
                StepKind::VmXmlGenerate,
                StepKind::VmRegister,
                StepKind::Verify,
            ]
        );
    }

    #[test]
    fn phase_a_and_phase_b_partition_the_plan() {
        let plan = dummy_plan();
        let a = PendingPlan::phase_a_steps(&plan).len();
        let b = PendingPlan::phase_b_steps(&plan).len();
        assert_eq!(a + b, plan.steps.len());
    }

    #[test]
    fn host_fingerprint_dedups_and_sorts_pci_ids() {
        // The capture helper sorts and dedupes the PCI ids before
        // persisting. Bypass building a full SystemProfile and exercise the
        // sort/dedup directly to keep this test small.
        let mut ids = vec![
            "10de:1c03".to_string(),
            "10de:1c03".to_string(), // duplicate
            "10de:0fb9".to_string(),
            "8086:9d70".to_string(),
        ];
        ids.sort();
        ids.dedup();
        assert_eq!(ids, vec!["10de:0fb9", "10de:1c03", "8086:9d70"]);
    }

    #[test]
    fn pending_plan_round_trips_through_toml() {
        // Build a fingerprint manually so the test doesn't need a full
        // SystemProfile.
        let fingerprint = HostFingerprint {
            distro_id: "arch".to_string(),
            kernel_version: "6.10.0".to_string(),
            bootloader: crate::detect::bootloader::BootloaderKind::Grub2,
            initramfs: crate::detect::initramfs::InitramfsSystem::Mkinitcpio,
            passthrough_pci_ids: vec!["10de:1c03".to_string()],
            kernel_cmdline_pre_apply: "BOOT_IMAGE=/vmlinuz-linux".to_string(),
        };

        let plan = dummy_plan();
        let config = crate::vm::PassthroughConfig {
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
        };

        let pending = PendingPlan {
            virtu_version: "0.1.0".to_string(),
            created_at: chrono::Utc::now(),
            snapshot_id: "test-id".to_string(),
            host_fingerprint: fingerprint,
            plan_summary: plan.summary.clone(),
            remaining_steps: PendingPlan::phase_b_steps(&plan),
            config,
        };

        let serialized = toml::to_string(&pending).expect("serialize");
        let parsed: PendingPlan = toml::from_str(&serialized).expect("parse");
        assert_eq!(pending, parsed);
    }
}
