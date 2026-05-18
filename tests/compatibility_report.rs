use std::path::PathBuf;

use virtu::detect::bootloader::{self, BootloaderKind};
use virtu::detect::display_manager::DisplayManager;
use virtu::detect::display_server::DisplayServer;
use virtu::detect::initramfs::InitramfsSystem;
use virtu::detect::readiness;
use virtu::detect::virtualization::VirtInfo;
use virtu::detect::{
    audio, cpu, display_manager, distro, gpu, initramfs, iommu, memory, monitors, storage, usb,
    SystemProfile,
};
use virtu::engine::{
    build_compatibility_report, CompatibilityStatus, FindingSeverity, FixAutomation,
};

fn fixture(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(path)
}

async fn fixture_profile() -> SystemProfile {
    let sysfs = fixture("sysfs");
    let iommu_groups = iommu::detect_groups_from_sysfs_root(&sysfs).await.unwrap();
    let mut gpus = gpu::detect_all_from_sysfs_root(&sysfs).await.unwrap();

    for gpu in &mut gpus {
        gpu.iommu_isolated = iommu::is_gpu_isolated(&iommu_groups, &gpu.pci_slot);
        gpu.iommu_group_id = iommu::group_for_pci_slot(&iommu_groups, &gpu.pci_slot);
        gpu.vfio_compatible =
            gpu.iommu_isolated && gpu.current_driver.as_deref() != Some("vfio-pci");
    }

    let cpuinfo = std::fs::read_to_string(fixture("proc/cpuinfo-intel-2socket-ht")).unwrap();
    let meminfo = std::fs::read_to_string(fixture("proc/meminfo-basic")).unwrap();
    let os_release = std::fs::read_to_string(fixture("etc/os-release-arch")).unwrap();
    let readiness = readiness::detect_from_root(fixture("readiness"))
        .await
        .unwrap();

    SystemProfile {
        cpu: cpu::parse_cpuinfo(&cpuinfo, true, Vec::new()),
        gpus,
        iommu_groups,
        ram: memory::parse_meminfo(&meminfo),
        distro: distro::parse_distro_info(&os_release),
        bootloader: bootloader::detect_from_root(fixture("bootloaders/grub"), true)
            .await
            .unwrap(),
        initramfs_system: initramfs::detect_from_root(fixture("initramfs/arch"), false)
            .await
            .unwrap(),
        display_manager: display_manager::detect_from_root(fixture("display/sddm"))
            .await
            .unwrap(),
        display_server: DisplayServer::Wayland,
        audio: audio::detect_from_root(fixture("audio/pipewire"))
            .await
            .unwrap(),
        monitors: monitors::detect_from_drm_root(fixture("sysfs/class/drm"))
            .await
            .unwrap(),
        usb_devices: usb::detect_input_devices_from_root(fixture("usb"))
            .await
            .unwrap(),
        storage: storage::detect_from_root(fixture("storage"), false)
            .await
            .unwrap(),
        virtualization: VirtInfo {
            qemu_version: Some("QEMU emulator version 8.2.0".to_string()),
            libvirt_version: Some("10.0.0".to_string()),
            virsh_available: true,
            virt_manager_available: false,
            libvirtd_running: true,
        },
        secure_boot: readiness.secure_boot,
        kernel_cmdline: readiness.kernel_cmdline.clone(),
        readiness,
        scan_timestamp: chrono::Utc::now(),
    }
}

#[tokio::test]
async fn fixture_profile_is_compatible_with_secure_boot_warning() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);

    assert_eq!(report.status, CompatibilityStatus::Warnings);
    assert_eq!(report.count(FindingSeverity::Fail), 0);
    assert_eq!(
        report.finding("gpu-isolation").unwrap().severity,
        FindingSeverity::Pass
    );
    assert_eq!(
        report.finding("ovmf-available").unwrap().severity,
        FindingSeverity::Pass
    );
    assert_eq!(
        report.finding("secure-boot").unwrap().severity,
        FindingSeverity::Warn
    );
}

#[tokio::test]
async fn blocked_profile_reports_exact_host_blockers() {
    let mut profile = fixture_profile().await;

    profile.iommu_groups.clear();
    for gpu in &mut profile.gpus {
        gpu.iommu_isolated = false;
        gpu.iommu_group_id = None;
        gpu.vfio_compatible = false;
    }
    profile.bootloader.kind = BootloaderKind::Unknown;
    profile.bootloader.config_path = None;
    profile.bootloader.entry_paths.clear();
    profile.bootloader.update_command = None;
    profile.initramfs_system = InitramfsSystem::Unknown;
    profile.virtualization = VirtInfo {
        qemu_version: None,
        libvirt_version: None,
        virsh_available: false,
        virt_manager_available: false,
        libvirtd_running: false,
    };
    profile.readiness.ovmf.code_paths.clear();
    profile.readiness.ovmf.vars_paths.clear();
    profile.readiness.user_access =
        readiness::parse_user_access(Some("alice".to_string()), "alice wheel input");
    profile.secure_boot = false;

    let report = build_compatibility_report(&profile);

    assert_eq!(report.status, CompatibilityStatus::Blocked);
    for id in [
        "iommu-active",
        "bootloader-detected",
        "initramfs-detected",
        "gpu-isolation",
        "qemu-available",
        "libvirt-available",
        "ovmf-available",
        "user-access",
    ] {
        assert_eq!(
            report.finding(id).unwrap().severity,
            FindingSeverity::Fail,
            "{id} should be a blocker"
        );
    }

    let iommu_fix = &report.finding("iommu-active").unwrap().fix_options[0];
    assert_eq!(iommu_fix.automation, FixAutomation::Manual);
}

#[tokio::test]
async fn single_gpu_profile_warns_instead_of_claiming_safe_automation() {
    let mut profile = fixture_profile().await;
    let mut only_gpu = profile.gpus.remove(0);
    only_gpu.iommu_isolated = true;
    only_gpu.iommu_group_id = Some(99);
    only_gpu.vfio_compatible = true;
    profile.gpus = vec![only_gpu];
    profile.display_manager = DisplayManager::Sddm;

    let report = build_compatibility_report(&profile);
    let layout = report.finding("gpu-layout").unwrap();

    assert_eq!(layout.severity, FindingSeverity::Warn);
    assert!(layout.explanation.contains("Single-GPU passthrough"));
    assert_eq!(
        layout.fix_options[0].automation,
        FixAutomation::VirtuCanApply
    );
    assert!(layout.fix_options[0].requires_confirmation);
}
