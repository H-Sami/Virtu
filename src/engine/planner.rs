use crate::detect::SystemProfile;

use super::step::{PlannedStep, StepKind, StepRisk};

pub fn build_plan(profile: &SystemProfile) -> Vec<PlannedStep> {
    let mut steps = Vec::new();

    steps.push(PlannedStep {
        kind: StepKind::Snapshot,
        title: "Create rollback snapshot".to_string(),
        summary: "Back up every file Virtu may edit before applying changes.".to_string(),
        risk: StepRisk::ReadOnly,
        touches: vec!["~/.virtu/snapshots".to_string()],
        requires_reboot: false,
    });

    if !profile.iommu_active() {
        steps.push(PlannedStep {
            kind: StepKind::BootloaderWrite,
            title: "Enable IOMMU kernel parameters".to_string(),
            summary: "Add the Intel or AMD IOMMU parameter to the detected bootloader entry."
                .to_string(),
            risk: StepRisk::Medium,
            touches: profile
                .bootloader
                .config_path
                .as_ref()
                .map(|p| vec![p.display().to_string()])
                .unwrap_or_default(),
            requires_reboot: true,
        });
    }

    if !profile.passthrough_candidates().is_empty() {
        steps.push(PlannedStep {
            kind: StepKind::VfioConfig,
            title: "Configure VFIO binding".to_string(),
            summary: "Write vfio-pci IDs and module ordering for the selected passthrough GPU."
                .to_string(),
            risk: StepRisk::Medium,
            touches: vec!["/etc/modprobe.d".to_string()],
            requires_reboot: true,
        });
    }

    steps
}
