pub mod audio;
pub mod bootloader;
pub mod cpu;
pub mod display_manager;
pub mod display_server;
pub mod distro;
pub mod gpu;
pub mod initramfs;
pub mod iommu;
pub mod memory;
pub mod monitors;
pub mod readiness;
pub mod storage;
pub mod usb;
pub mod virtualization;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::info;

pub use audio::AudioSystem;
pub use bootloader::BootloaderInfo;
pub use cpu::CpuInfo;
pub use display_manager::DisplayManager;
pub use display_server::DisplayServer;
pub use distro::DistroInfo;
pub use gpu::GpuInfo;
pub use initramfs::InitramfsSystem;
pub use iommu::IommuGroup;
pub use memory::MemInfo;
pub use monitors::MonitorInfo;
pub use readiness::ReadinessInfo;
pub use storage::StorageInfo;
pub use usb::UsbDevice;
pub use virtualization::VirtInfo;

/// Immutable picture of the host system captured at scan time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemProfile {
    pub cpu: CpuInfo,
    pub gpus: Vec<GpuInfo>,
    pub iommu_groups: Vec<IommuGroup>,
    pub ram: MemInfo,
    pub distro: DistroInfo,
    pub bootloader: BootloaderInfo,
    pub initramfs_system: InitramfsSystem,
    pub display_manager: DisplayManager,
    pub display_server: DisplayServer,
    pub audio: AudioSystem,
    pub monitors: Vec<MonitorInfo>,
    pub usb_devices: Vec<UsbDevice>,
    pub storage: StorageInfo,
    pub virtualization: VirtInfo,
    pub readiness: ReadinessInfo,
    pub secure_boot: bool,
    pub kernel_cmdline: String,
    pub scan_timestamp: chrono::DateTime<chrono::Utc>,
}

impl SystemProfile {
    pub fn iommu_active(&self) -> bool {
        !self.iommu_groups.is_empty()
    }
}

/// Runs every detection module concurrently and assembles the SystemProfile.
pub async fn scan_system() -> Result<SystemProfile> {
    info!("Starting full system scan");

    let (
        cpu_res,
        gpus_res,
        iommu_res,
        ram_res,
        distro_res,
        bootloader_res,
        initramfs_res,
        dm_res,
        ds_res,
        audio_res,
        monitors_res,
        usb_res,
        storage_res,
        virt_res,
        readiness_res,
    ) = tokio::join!(
        cpu::detect(),
        gpu::detect_all(),
        iommu::detect_groups(),
        memory::detect(),
        distro::detect(),
        bootloader::detect(),
        initramfs::detect(),
        display_manager::detect(),
        display_server::detect(),
        audio::detect(),
        monitors::detect(),
        usb::detect_input_devices(),
        storage::detect(),
        virtualization::detect(),
        readiness::detect(),
    );

    let mut gpus = gpus_res?;
    let iommu_groups = iommu_res?;
    let readiness = readiness_res?;

    for gpu in &mut gpus {
        gpu.iommu_isolated = iommu::is_gpu_isolated(&iommu_groups, &gpu.pci_slot);
        gpu.iommu_group_id = iommu::group_for_pci_slot(&iommu_groups, &gpu.pci_slot);
        gpu.vfio_compatible =
            gpu.iommu_isolated && gpu.current_driver.as_deref() != Some("vfio-pci");
    }

    let profile = SystemProfile {
        cpu: cpu_res?,
        gpus,
        iommu_groups,
        ram: ram_res?,
        distro: distro_res?,
        bootloader: bootloader_res?,
        initramfs_system: initramfs_res?,
        display_manager: dm_res?,
        display_server: ds_res?,
        audio: audio_res?,
        monitors: monitors_res?,
        usb_devices: usb_res?,
        storage: storage_res?,
        virtualization: virt_res?,
        secure_boot: readiness.secure_boot,
        kernel_cmdline: readiness.kernel_cmdline.clone(),
        readiness,
        scan_timestamp: chrono::Utc::now(),
    };

    info!("System scan complete");
    Ok(profile)
}

/// Print a human-readable system scan summary to stdout.
pub fn print_report(profile: &SystemProfile) {
    println!("\n=== VIRTU SYSTEM SCAN ===");
    println!(
        "CPU:         {} ({})",
        profile.cpu.model_name, profile.cpu.vendor
    );
    println!(
        "VT-d/AMD-Vi: {}",
        if profile.cpu.iommu_capable {
            "supported"
        } else {
            "not detected - enable virtualization/IOMMU in firmware"
        }
    );
    println!(
        "IOMMU:       {}",
        if profile.iommu_active() {
            format!("active ({} groups)", profile.iommu_groups.len())
        } else {
            "not active - kernel parameters or firmware settings are needed".to_string()
        }
    );
    println!("Distro:      {}", profile.distro.pretty_name);
    println!("Kernel:      {}", profile.readiness.kernel_version);
    println!("Bootloader:  {}", profile.bootloader.kind);
    println!("Initramfs:   {}", profile.initramfs_system.name());
    println!(
        "RAM:         {} GB total, {} GB hugepage-eligible",
        profile.ram.total_gb(),
        profile.ram.hugepage_eligible_gb()
    );
    println!("Monitors:    {} detected", profile.monitors.len());
    println!(
        "Secure Boot: {}",
        if profile.secure_boot {
            "enabled (may require module signing)"
        } else {
            "disabled"
        }
    );
    println!(
        "OVMF:        {}",
        if profile.readiness.ovmf.available() {
            "available"
        } else {
            "not found"
        }
    );
    println!(
        "Groups:      libvirt={}, kvm={}",
        if profile.readiness.user_access.in_libvirt_group {
            "yes"
        } else {
            "no"
        },
        if profile.readiness.user_access.in_kvm_group {
            "yes"
        } else {
            "no"
        }
    );
    println!(
        "VM domains:  {} existing",
        profile.readiness.libvirt_domains.len()
    );

    println!("\nDetected GPUs:");
    for gpu in &profile.gpus {
        let isolated = if gpu.iommu_isolated {
            "isolated"
        } else {
            "not isolated"
        };
        let kind = match gpu.gpu_type {
            gpu::GpuType::Integrated => "iGPU",
            gpu::GpuType::Discrete => "dGPU",
            gpu::GpuType::Unknown => "GPU?",
        };
        println!(
            "  [{kind}] {} - {} | IOMMU group {}: {}",
            gpu.model_name,
            gpu.pci_slot,
            gpu.iommu_group_id
                .map(|i| i.to_string())
                .unwrap_or_else(|| "?".to_string()),
            isolated
        );
    }
    println!();
}

/// Print current VFIO binding status.
pub async fn print_vfio_status() -> Result<()> {
    let groups = iommu::detect_groups().await?;
    let gpus = gpu::detect_all().await?;

    println!("\n=== VIRTU STATUS ===\n");
    let cmdline = std::fs::read_to_string("/proc/cmdline").unwrap_or_default();
    let iommu_on = cmdline.contains("intel_iommu=on") || cmdline.contains("amd_iommu=on");
    println!(
        "IOMMU in kernel cmdline: {}",
        if iommu_on { "yes" } else { "no" }
    );
    println!("IOMMU groups detected:   {}", groups.len());

    for gpu in &gpus {
        let driver = gpu.current_driver.as_deref().unwrap_or("none");
        let bound = if driver == "vfio-pci" {
            "VFIO bound".to_string()
        } else {
            format!("driver: {driver}")
        };
        println!("  {} {} - {}", gpu.model_name, gpu.pci_slot, bound);
    }
    println!();
    Ok(())
}
