use crate::detect::bootloader::BootloaderKind;
use crate::detect::gpu::GpuType;
use crate::detect::initramfs::InitramfsSystem;
use crate::detect::{IommuGroup, SystemProfile};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompatibilityStatus {
    Ready,
    Warnings,
    Blocked,
}

impl std::fmt::Display for CompatibilityStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompatibilityStatus::Ready => write!(f, "ready"),
            CompatibilityStatus::Warnings => write!(f, "ready with warnings"),
            CompatibilityStatus::Blocked => write!(f, "blocked"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FindingSeverity {
    Pass,
    Warn,
    Fail,
}

impl std::fmt::Display for FindingSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FindingSeverity::Pass => write!(f, "PASS"),
            FindingSeverity::Warn => write!(f, "WARN"),
            FindingSeverity::Fail => write!(f, "FAIL"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FixAutomation {
    VirtuCanApply,
    Manual,
    NotAutomatable,
}

impl std::fmt::Display for FixAutomation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FixAutomation::VirtuCanApply => write!(f, "Virtu can apply after approval"),
            FixAutomation::Manual => write!(f, "manual action required"),
            FixAutomation::NotAutomatable => write!(f, "not automatable"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixOption {
    pub title: String,
    pub details: String,
    pub automation: FixAutomation,
    pub requires_confirmation: bool,
}

impl FixOption {
    pub fn new(
        title: impl Into<String>,
        details: impl Into<String>,
        automation: FixAutomation,
        requires_confirmation: bool,
    ) -> Self {
        Self {
            title: title.into(),
            details: details.into(),
            automation,
            requires_confirmation,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatibilityFinding {
    pub id: String,
    pub severity: FindingSeverity,
    pub title: String,
    pub explanation: String,
    pub evidence: Vec<String>,
    pub fix_options: Vec<FixOption>,
}

impl CompatibilityFinding {
    pub fn new(
        id: impl Into<String>,
        severity: FindingSeverity,
        title: impl Into<String>,
        explanation: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            severity,
            title: title.into(),
            explanation: explanation.into(),
            evidence: Vec::new(),
            fix_options: Vec::new(),
        }
    }

    pub fn with_evidence(mut self, evidence: impl Into<String>) -> Self {
        self.evidence.push(evidence.into());
        self
    }

    pub fn with_fix(mut self, fix: FixOption) -> Self {
        self.fix_options.push(fix);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatibilityReport {
    pub status: CompatibilityStatus,
    pub findings: Vec<CompatibilityFinding>,
}

impl CompatibilityReport {
    pub fn new(findings: Vec<CompatibilityFinding>) -> Self {
        let status = if findings
            .iter()
            .any(|finding| finding.severity == FindingSeverity::Fail)
        {
            CompatibilityStatus::Blocked
        } else if findings
            .iter()
            .any(|finding| finding.severity == FindingSeverity::Warn)
        {
            CompatibilityStatus::Warnings
        } else {
            CompatibilityStatus::Ready
        };

        Self { status, findings }
    }

    pub fn is_blocked(&self) -> bool {
        self.status == CompatibilityStatus::Blocked
    }

    pub fn count(&self, severity: FindingSeverity) -> usize {
        self.findings
            .iter()
            .filter(|finding| finding.severity == severity)
            .count()
    }

    pub fn finding(&self, id: &str) -> Option<&CompatibilityFinding> {
        self.findings.iter().find(|finding| finding.id == id)
    }

    pub fn print_human(&self) {
        println!("\n=== VIRTU COMPATIBILITY FINDINGS ===");
        println!(
            "Overall: {} ({} pass, {} warn, {} fail)",
            self.status,
            self.count(FindingSeverity::Pass),
            self.count(FindingSeverity::Warn),
            self.count(FindingSeverity::Fail)
        );

        for severity in [
            FindingSeverity::Fail,
            FindingSeverity::Warn,
            FindingSeverity::Pass,
        ] {
            for finding in self
                .findings
                .iter()
                .filter(|finding| finding.severity == severity)
            {
                println!("\n[{}] {}", finding.severity, finding.title);
                println!("  {}", finding.explanation);

                for evidence in &finding.evidence {
                    println!("  Evidence: {evidence}");
                }

                for fix in &finding.fix_options {
                    println!("  Fix: {} ({})", fix.title, fix.automation);
                    println!("       {}", fix.details);
                }
            }
        }
        println!();
    }
}

pub fn build_compatibility_report(profile: &SystemProfile) -> CompatibilityReport {
    let mut findings = Vec::new();

    push_cpu_virtualization(profile, &mut findings);
    push_iommu_state(profile, &mut findings);
    push_bootloader_state(profile, &mut findings);
    push_initramfs_state(profile, &mut findings);
    push_gpu_state(profile, &mut findings);
    push_virtualization_tools(profile, &mut findings);
    push_ovmf_state(profile, &mut findings);
    push_user_access(profile, &mut findings);
    push_kernel_headers(profile, &mut findings);
    push_secure_boot(profile, &mut findings);
    push_monitor_state(profile, &mut findings);
    push_storage_state(profile, &mut findings);

    CompatibilityReport::new(findings)
}

fn push_cpu_virtualization(profile: &SystemProfile, findings: &mut Vec<CompatibilityFinding>) {
    if profile.cpu.iommu_capable {
        findings.push(
            CompatibilityFinding::new(
                "cpu-virtualization",
                FindingSeverity::Pass,
                "CPU virtualization extensions detected",
                "The CPU exposes virtualization support required for KVM-based passthrough.",
            )
            .with_evidence(format!(
                "{} {}, {} physical cores / {} logical threads",
                profile.cpu.vendor,
                profile.cpu.model_name,
                profile.cpu.physical_cores,
                profile.cpu.logical_cores
            )),
        );
    } else {
        findings.push(
            CompatibilityFinding::new(
                "cpu-virtualization",
                FindingSeverity::Fail,
                "CPU virtualization extensions were not detected",
                "Virtu cannot continue until hardware virtualization is available to the host.",
            )
            .with_evidence(format!(
                "{} {}; VMX/SVM flags not found in detected CPU data",
                profile.cpu.vendor, profile.cpu.model_name
            ))
            .with_fix(FixOption::new(
                "Enable virtualization in firmware",
                "Enable Intel VT-x/VT-d or AMD SVM/AMD-Vi in BIOS/UEFI, then boot Linux again.",
                FixAutomation::Manual,
                false,
            ))
            .with_fix(FixOption::new(
                "Use supported hardware",
                "If the firmware exposes no virtualization controls, this system cannot support GPU passthrough safely.",
                FixAutomation::NotAutomatable,
                false,
            )),
        );
    }
}

fn push_iommu_state(profile: &SystemProfile, findings: &mut Vec<CompatibilityFinding>) {
    if profile.iommu_active() {
        findings.push(
            CompatibilityFinding::new(
                "iommu-active",
                FindingSeverity::Pass,
                "IOMMU groups are active",
                "The kernel exposes IOMMU groups, so Virtu can reason about device isolation.",
            )
            .with_evidence(format!(
                "{} IOMMU group(s) detected",
                profile.iommu_groups.len()
            ))
            .with_evidence(format_cmdline(&profile.kernel_cmdline)),
        );
    } else {
        let mut finding = CompatibilityFinding::new(
            "iommu-active",
            FindingSeverity::Fail,
            "IOMMU groups are not active",
            "GPU passthrough is unsafe without active IOMMU groups because the host cannot isolate PCI devices.",
        )
        .with_evidence("0 IOMMU groups detected")
        .with_evidence(format_cmdline(&profile.kernel_cmdline))
        .with_fix(FixOption::new(
            "Enable IOMMU in firmware",
            "Enable Intel VT-d or AMD-Vi/IOMMU in BIOS/UEFI. Some boards also require Above 4G Decoding.",
            FixAutomation::Manual,
            false,
        ));

        if profile.bootloader.kind != BootloaderKind::Unknown {
            finding = finding.with_fix(FixOption::new(
                "Add IOMMU kernel parameters",
                format!(
                    "Virtu can later plan a bootloader edit that adds `{}` and preserves rollback data first.",
                    expected_iommu_param(profile)
                ),
                FixAutomation::VirtuCanApply,
                true,
            ));
        }

        findings.push(finding);
    }
}

fn push_bootloader_state(profile: &SystemProfile, findings: &mut Vec<CompatibilityFinding>) {
    match profile.bootloader.kind {
        BootloaderKind::Unknown => {
            findings.push(
                CompatibilityFinding::new(
                    "bootloader-detected",
                    FindingSeverity::Fail,
                    "Bootloader could not be identified",
                    "Virtu cannot safely plan kernel-parameter changes until it knows exactly where boot entries live.",
                )
                .with_evidence("Bootloader kind: Unknown")
                .with_fix(FixOption::new(
                    "Identify the bootloader manually",
                    "Confirm whether the system uses GRUB2, systemd-boot, rEFInd, Syslinux/Extlinux, or EFISTUB and add a detector fixture for this layout.",
                    FixAutomation::Manual,
                    false,
                )),
            );
        }
        BootloaderKind::Efistub => {
            findings.push(
                CompatibilityFinding::new(
                    "bootloader-detected",
                    FindingSeverity::Warn,
                    "EFISTUB boot path detected",
                    "EFISTUB is a target boot path, but changing EFI boot variables is high impact and must remain explicit.",
                )
                .with_evidence("Bootloader kind: EFISTUB")
                .with_fix(FixOption::new(
                    "Review EFI entry before mutation",
                    "Virtu should show the exact efibootmgr command and require explicit confirmation before changing EFI variables.",
                    FixAutomation::VirtuCanApply,
                    true,
                )),
            );
        }
        _ => {
            let config = profile
                .bootloader
                .config_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "no single config path".to_string());
            findings.push(
                CompatibilityFinding::new(
                    "bootloader-detected",
                    FindingSeverity::Pass,
                    "Supported bootloader detected",
                    "Virtu can build a bootloader-specific plan instead of editing generic files blindly.",
                )
                .with_evidence(format!("Bootloader: {}", profile.bootloader.kind))
                .with_evidence(format!("Config: {config}")),
            );
        }
    }
}

fn push_initramfs_state(profile: &SystemProfile, findings: &mut Vec<CompatibilityFinding>) {
    if profile.initramfs_system == InitramfsSystem::Unknown {
        findings.push(
            CompatibilityFinding::new(
                "initramfs-detected",
                FindingSeverity::Fail,
                "Initramfs system could not be identified",
                "Virtu cannot safely place VFIO modules early in boot without knowing how initramfs is generated.",
            )
            .with_evidence("Initramfs system: Unknown")
            .with_fix(FixOption::new(
                "Add distro-specific initramfs support",
                "Identify whether this host uses mkinitcpio, dracut, update-initramfs, or another generator before mutating module order.",
                FixAutomation::Manual,
                false,
            )),
        );
    } else {
        findings.push(
            CompatibilityFinding::new(
                "initramfs-detected",
                FindingSeverity::Pass,
                "Initramfs system detected",
                "Virtu can later plan VFIO module ordering and the correct rebuild command for this distro.",
            )
            .with_evidence(profile.initramfs_system.name())
            .with_evidence(format!(
                "Rebuild command: {}",
                profile.initramfs_system.rebuild_command()
            )),
        );
    }
}

fn push_gpu_state(profile: &SystemProfile, findings: &mut Vec<CompatibilityFinding>) {
    if profile.gpus.is_empty() {
        findings.push(
            CompatibilityFinding::new(
                "gpus-detected",
                FindingSeverity::Fail,
                "No GPUs were detected",
                "Virtu cannot plan GPU passthrough without at least one display-class PCI device.",
            )
            .with_fix(FixOption::new(
                "Check PCI visibility",
                "Confirm the GPU appears under `/sys/bus/pci/devices` and is not hidden by firmware or kernel configuration.",
                FixAutomation::Manual,
                false,
            )),
        );
        return;
    }

    findings.push(
        CompatibilityFinding::new(
            "gpus-detected",
            FindingSeverity::Pass,
            "GPU devices detected",
            "Virtu found display-class PCI devices that can be evaluated for passthrough roles.",
        )
        .with_evidence(format!("{} GPU(s) detected", profile.gpus.len()))
        .with_evidence(format_gpu_list(profile)),
    );

    let isolated_gpus: Vec<_> = profile
        .gpus
        .iter()
        .filter(|gpu| gpu.iommu_isolated)
        .collect();

    if isolated_gpus.is_empty() {
        findings.push(
            CompatibilityFinding::new(
                "gpu-isolation",
                FindingSeverity::Fail,
                "No GPU is isolated for passthrough",
                "At least one selected GPU must be in an IOMMU group that contains only GPU display/audio functions and harmless bridges.",
            )
            .with_evidence(format_iommu_groups(&profile.iommu_groups))
            .with_fix(FixOption::new(
                "Try another PCIe slot or firmware settings",
                "Move the GPU to another slot when possible and enable Above 4G Decoding or similar PCIe/IOMMU options in firmware.",
                FixAutomation::Manual,
                false,
            ))
            .with_fix(FixOption::new(
                "ACS override is risky",
                "An ACS override may split groups on some systems, but it can reduce DMA isolation and must be treated as an explicit high-risk option.",
                FixAutomation::Manual,
                true,
            )),
        );
    } else {
        findings.push(
            CompatibilityFinding::new(
                "gpu-isolation",
                FindingSeverity::Pass,
                "At least one GPU is isolated",
                "Virtu has a viable PCI isolation candidate before any user choice or VFIO binding plan.",
            )
            .with_evidence(
                isolated_gpus
                    .iter()
                    .map(|gpu| {
                        format!(
                            "{} ({}) in group {}",
                            gpu.model_name,
                            gpu.pci_slot,
                            gpu.iommu_group_id
                                .map(|id| id.to_string())
                                .unwrap_or_else(|| "?".to_string())
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
        );
    }

    push_gpu_layout(profile, findings);
}

fn push_gpu_layout(profile: &SystemProfile, findings: &mut Vec<CompatibilityFinding>) {
    if profile.gpus.len() == 1 {
        let gpu = &profile.gpus[0];
        let mut finding = CompatibilityFinding::new(
            "gpu-layout",
            FindingSeverity::Warn,
            "Single-GPU setup detected",
            "Single-GPU passthrough can work, but it requires display-manager hooks and a tested rollback path before Virtu should automate it.",
        )
        .with_evidence(format!("Only GPU: {} ({})", gpu.model_name, gpu.pci_slot))
        .with_fix(FixOption::new(
            "Use single-GPU mode only after rollback is ready",
            "Virtu should generate display-manager-aware hooks, syntax-check them, and provide recovery instructions before enabling this path.",
            FixAutomation::VirtuCanApply,
            true,
        ));

        if profile.display_manager.service_name().is_none() {
            finding = finding.with_evidence(format!(
                "Display manager: {} (hook behavior may need manual review)",
                profile.display_manager
            ));
        }

        findings.push(finding);
        return;
    }

    let igpu_count = profile
        .gpus
        .iter()
        .filter(|gpu| gpu.gpu_type == GpuType::Integrated)
        .count();
    let dgpu_count = profile
        .gpus
        .iter()
        .filter(|gpu| gpu.gpu_type == GpuType::Discrete)
        .count();

    let title = if igpu_count > 0 && dgpu_count > 0 {
        "iGPU plus dGPU layout detected"
    } else {
        "Multi-GPU layout detected"
    };

    findings.push(
        CompatibilityFinding::new(
            "gpu-layout",
            FindingSeverity::Pass,
            title,
            "The hardware layout can support safer host-GPU plus guest-GPU planning, while still allowing the user to ignore any GPU.",
        )
        .with_evidence(format!(
            "{igpu_count} integrated GPU(s), {dgpu_count} discrete GPU(s), {} total GPU(s)",
            profile.gpus.len()
        )),
    );
}

fn push_virtualization_tools(profile: &SystemProfile, findings: &mut Vec<CompatibilityFinding>) {
    if let Some(version) = &profile.virtualization.qemu_version {
        findings.push(
            CompatibilityFinding::new(
                "qemu-available",
                FindingSeverity::Pass,
                "QEMU is available",
                "QEMU is required to run the VM after host configuration succeeds.",
            )
            .with_evidence(version),
        );
    } else {
        findings.push(
            CompatibilityFinding::new(
                "qemu-available",
                FindingSeverity::Fail,
                "QEMU is not available",
                "Virtu cannot create or run the passthrough VM without QEMU.",
            )
            .with_fix(FixOption::new(
                "Install QEMU packages",
                format!(
                    "Use the distro package manager to install QEMU. Detected installer family command: `{}`.",
                    profile.distro.package_manager.install_command()
                ),
                FixAutomation::Manual,
                false,
            )),
        );
    }

    if profile.virtualization.virsh_available {
        let evidence = profile
            .virtualization
            .libvirt_version
            .as_deref()
            .unwrap_or("virsh command detected");
        findings.push(
            CompatibilityFinding::new(
                "libvirt-available",
                FindingSeverity::Pass,
                "libvirt tools are available",
                "Virtu can validate and define libvirt domains once VM generation is implemented.",
            )
            .with_evidence(evidence),
        );
    } else {
        findings.push(
            CompatibilityFinding::new(
                "libvirt-available",
                FindingSeverity::Fail,
                "libvirt tools are not available",
                "Virtu needs `virsh` and libvirt to validate XML, detect domain conflicts, and register VMs safely.",
            )
            .with_fix(FixOption::new(
                "Install libvirt tools",
                format!(
                    "Install libvirt and virsh using `{}` or the equivalent distro package set.",
                    profile.distro.package_manager.install_command()
                ),
                FixAutomation::Manual,
                false,
            )),
        );
    }

    if profile.virtualization.virsh_available && !profile.virtualization.libvirtd_running {
        findings.push(
            CompatibilityFinding::new(
                "libvirtd-running",
                FindingSeverity::Warn,
                "libvirt service is not running",
                "The VM can be planned, but libvirt must be running before Virtu validates or defines a domain.",
            )
            .with_fix(FixOption::new(
                "Start libvirt service",
                "Virtu can later run a targeted `systemctl start libvirtd` or distro-equivalent service command after approval.",
                FixAutomation::VirtuCanApply,
                true,
            )),
        );
    }
}

fn push_ovmf_state(profile: &SystemProfile, findings: &mut Vec<CompatibilityFinding>) {
    if profile.readiness.ovmf.available() {
        findings.push(
            CompatibilityFinding::new(
                "ovmf-available",
                FindingSeverity::Pass,
                "OVMF firmware is available",
                "UEFI firmware files are present for modern Windows and Linux guest VMs.",
            )
            .with_evidence(format!(
                "code: {}; vars: {}",
                profile
                    .readiness
                    .ovmf
                    .code_paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
                profile
                    .readiness
                    .ovmf
                    .vars_paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        );
    } else {
        findings.push(
            CompatibilityFinding::new(
                "ovmf-available",
                FindingSeverity::Fail,
                "OVMF firmware was not found",
                "Virtu needs OVMF code and variable templates to generate modern UEFI libvirt XML.",
            )
            .with_evidence(format!(
                "{} code path(s), {} vars path(s)",
                profile.readiness.ovmf.code_paths.len(),
                profile.readiness.ovmf.vars_paths.len()
            ))
            .with_fix(FixOption::new(
                "Install OVMF/edk2 firmware package",
                "Install the distro package named OVMF, edk2-ovmf, edk2-ovmf-code, or equivalent, then scan again.",
                FixAutomation::Manual,
                false,
            )),
        );
    }
}

fn push_user_access(profile: &SystemProfile, findings: &mut Vec<CompatibilityFinding>) {
    let access = &profile.readiness.user_access;
    let mut missing = Vec::new();
    if !access.in_libvirt_group {
        missing.push("libvirt");
    }
    if !access.in_kvm_group {
        missing.push("kvm");
    }

    if missing.is_empty() {
        findings.push(
            CompatibilityFinding::new(
                "user-access",
                FindingSeverity::Pass,
                "User has virtualization group access",
                "The current user appears to have the libvirt and KVM group memberships normally needed to run VMs without full-root execution.",
            )
            .with_evidence(format!(
                "user: {}; groups: {}",
                access.username.as_deref().unwrap_or("unknown"),
                access.groups.join(" ")
            )),
        );
    } else {
        findings.push(
            CompatibilityFinding::new(
                "user-access",
                FindingSeverity::Fail,
                "User is missing virtualization group access",
                "Virtu should not run the whole application as root, so the normal user needs libvirt/KVM access before VM creation.",
            )
            .with_evidence(format!(
                "missing group(s): {}; current groups: {}",
                missing.join(", "),
                access.groups.join(" ")
            ))
            .with_fix(FixOption::new(
                "Add the user to required groups",
                "Add the normal user to the missing groups and log out/in so group membership is refreshed.",
                FixAutomation::Manual,
                false,
            )),
        );
    }
}

fn push_kernel_headers(profile: &SystemProfile, findings: &mut Vec<CompatibilityFinding>) {
    if profile.readiness.kernel_headers.present {
        findings.push(
            CompatibilityFinding::new(
                "kernel-headers",
                FindingSeverity::Pass,
                "Kernel headers are installed",
                "Header files are available for workflows that need local module builds or deeper diagnostics.",
            )
            .with_evidence(
                profile
                    .readiness
                    .kernel_headers
                    .path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "header path detected".to_string()),
            ),
        );
    } else {
        findings.push(
            CompatibilityFinding::new(
                "kernel-headers",
                FindingSeverity::Warn,
                "Kernel headers were not found",
                "Basic VFIO may still work, but some diagnostics, DKMS drivers, and advanced fixes require headers matching the running kernel.",
            )
            .with_evidence(format!("kernel: {}", profile.readiness.kernel_version))
            .with_fix(FixOption::new(
                "Install matching kernel headers",
                "Install the header package for the currently running kernel before using workflows that compile local components.",
                FixAutomation::Manual,
                false,
            )),
        );
    }
}

fn push_secure_boot(profile: &SystemProfile, findings: &mut Vec<CompatibilityFinding>) {
    if profile.secure_boot {
        findings.push(
            CompatibilityFinding::new(
                "secure-boot",
                FindingSeverity::Warn,
                "Secure Boot is enabled",
                "Secure Boot can block unsigned kernel modules or custom boot paths, so Virtu must avoid silent module-signing assumptions.",
            )
            .with_fix(FixOption::new(
                "Use signed modules or disable Secure Boot",
                "Keep Secure Boot enabled only if required modules are signed and trusted, or disable it in firmware before applying VFIO changes.",
                FixAutomation::Manual,
                false,
            )),
        );
    } else {
        findings.push(
            CompatibilityFinding::new(
                "secure-boot",
                FindingSeverity::Pass,
                "Secure Boot is disabled",
                "Unsigned VFIO-related module workflows are less likely to be blocked by firmware policy.",
            ),
        );
    }
}

fn push_monitor_state(profile: &SystemProfile, findings: &mut Vec<CompatibilityFinding>) {
    let connected = profile
        .monitors
        .iter()
        .filter(|monitor| monitor.connected)
        .count();

    if connected == 0 {
        findings.push(
            CompatibilityFinding::new(
                "monitors-detected",
                FindingSeverity::Warn,
                "No connected monitors were detected",
                "Virtu can still plan Looking Glass later, but physical-display workflows need monitor ownership data.",
            )
            .with_evidence(format!("{} DRM connector(s) scanned", profile.monitors.len())),
        );
    } else {
        findings.push(
            CompatibilityFinding::new(
                "monitors-detected",
                FindingSeverity::Pass,
                "Connected monitor data is available",
                "Virtu can ask whether the user wants one-monitor, two-monitor, physical display, or Looking Glass workflows.",
            )
            .with_evidence(format!("{connected} connected monitor(s) detected")),
        );
    }
}

fn push_storage_state(profile: &SystemProfile, findings: &mut Vec<CompatibilityFinding>) {
    let available_gb = profile.storage.available_gb();

    if available_gb == 0 {
        findings.push(
            CompatibilityFinding::new(
                "vm-storage",
                FindingSeverity::Warn,
                "VM storage availability could not be confirmed",
                "Virtu needs enough free space for the selected VM disk before it can create storage safely.",
            )
            .with_evidence(format!(
                "default VM directory: {}",
                profile.storage.default_vm_dir.display()
            )),
        );
    } else if available_gb < 64 {
        findings.push(
            CompatibilityFinding::new(
                "vm-storage",
                FindingSeverity::Warn,
                "Default VM storage is low",
                "Windows gaming VMs usually need more than the currently detected free space.",
            )
            .with_evidence(format!(
                "{} GiB available in {}",
                available_gb,
                profile.storage.default_vm_dir.display()
            )),
        );
    } else {
        findings.push(
            CompatibilityFinding::new(
                "vm-storage",
                FindingSeverity::Pass,
                "Default VM storage has usable free space",
                "The default libvirt image directory appears large enough for an initial Windows VM disk.",
            )
            .with_evidence(format!(
                "{} GiB available in {}",
                available_gb,
                profile.storage.default_vm_dir.display()
            )),
        );
    }
}

fn expected_iommu_param(profile: &SystemProfile) -> &'static str {
    if profile.cpu.vendor.to_lowercase().contains("amd") {
        "amd_iommu=on"
    } else {
        "intel_iommu=on"
    }
}

fn format_cmdline(cmdline: &str) -> String {
    let cmdline = cmdline.trim();
    if cmdline.is_empty() {
        "kernel cmdline: <empty or unavailable>".to_string()
    } else {
        format!("kernel cmdline: {cmdline}")
    }
}

fn format_gpu_list(profile: &SystemProfile) -> String {
    profile
        .gpus
        .iter()
        .map(|gpu| {
            format!(
                "{} {} [{}:{}] driver={} isolated={}",
                gpu.pci_slot,
                gpu.model_name,
                gpu.vendor_id,
                gpu.device_id,
                gpu.current_driver.as_deref().unwrap_or("none"),
                gpu.iommu_isolated
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn format_iommu_groups(groups: &[IommuGroup]) -> String {
    if groups.is_empty() {
        return "no IOMMU groups detected".to_string();
    }

    groups
        .iter()
        .map(|group| {
            let devices = group
                .devices
                .iter()
                .map(|device| format!("{} {}", device.pci_slot, device.class))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "group {} isolated_for_gpu={} devices=[{}]",
                group.id, group.is_isolated_for_gpu, devices
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}
