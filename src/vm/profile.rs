//! Derived VM XML input view.
//!
//! [`PassthroughConfig`](crate::vm::passthrough::PassthroughConfig) is the
//! persisted user-choice model. The XML renderer should not own a second,
//! overlapping configuration struct, so this module exposes a lightweight
//! borrowed [`VmView`] derived from a validated system profile + config pair.

use crate::detect::{GpuInfo, MonitorInfo, SystemProfile};
use crate::vm::passthrough::{
    AudioChoice, DiskChoice, DiskFormat, GpuPassthroughMode, GuestOs, NetworkChoice,
    PassthroughConfig,
};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum VmViewError {
    #[error("expected exactly one passthrough GPU for VM XML generation, found {count}")]
    ExpectedOnePassthroughGpu { count: usize },
    #[error("expected one host GPU for {mode} VM XML generation")]
    ExpectedHostGpu { mode: GpuPassthroughMode },
    #[error("cannot derive VM XML view without a passthrough mode")]
    MissingPassthroughMode,
    #[error("multi-GPU passthrough XML generation is not implemented yet")]
    MultiGpuPassthroughUnsupported,
}

#[derive(Debug, Clone, Copy)]
pub struct VmDiskView<'a> {
    pub path: &'a Path,
    pub size_gb: Option<u64>,
    pub format: DiskFormat,
    pub create: bool,
}

impl<'a> VmDiskView<'a> {
    fn from_choice(choice: &'a DiskChoice) -> Self {
        match choice {
            DiskChoice::Existing { path } => Self {
                path,
                size_gb: None,
                format: disk_format_from_path(path),
                create: false,
            },
            DiskChoice::Create {
                path,
                size_gb,
                format,
            } => Self {
                path,
                size_gb: Some(*size_gb),
                format: *format,
                create: true,
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct VmInputView<'a> {
    pub keyboard_evdev: Option<&'a Path>,
    pub mouse_evdev: Option<&'a Path>,
    pub additional_evdev: Vec<&'a Path>,
}

impl<'a> VmInputView<'a> {
    fn from_config(config: &'a PassthroughConfig) -> Self {
        Self {
            keyboard_evdev: config.input.keyboard_evdev.as_deref(),
            mouse_evdev: config.input.mouse_evdev.as_deref(),
            additional_evdev: config
                .input
                .additional_evdev
                .iter()
                .map(|path| path.as_path())
                .collect(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct VmView<'a> {
    pub vm_name: &'a str,
    pub guest_os: GuestOs,
    pub ram_mb: u64,
    pub vcpu_count: u32,
    pub disk: VmDiskView<'a>,
    pub passthrough_gpu: &'a GpuInfo,
    pub host_gpu: Option<&'a GpuInfo>,
    pub passthrough_mode: GpuPassthroughMode,
    pub iso_path: Option<&'a Path>,
    pub vm_monitor: Option<&'a MonitorInfo>,
    pub host_monitor: Option<&'a MonitorInfo>,
    pub input: VmInputView<'a>,
    pub use_hugepages: bool,
    pub use_cpu_pinning: bool,
    pub use_iothreads: bool,
    pub enable_tpm: bool,
    pub enable_hyperv: bool,
    pub enable_secure_boot: bool,
    pub audio: AudioChoice,
    pub network: &'a NetworkChoice,
}

/// Derive the VM XML input view from the immutable host profile and the
/// user-choice model.
pub fn vm_view<'a>(
    profile: &'a SystemProfile,
    config: &'a PassthroughConfig,
) -> Result<VmView<'a>, VmViewError> {
    let passthrough_mode = config
        .derived_mode(profile)
        .ok_or(VmViewError::MissingPassthroughMode)?;
    if passthrough_mode == GpuPassthroughMode::MultiGpu {
        return Err(VmViewError::MultiGpuPassthroughUnsupported);
    }

    let passthrough_gpus = config.passthrough_gpus(profile);
    let passthrough_gpu =
        passthrough_gpus
            .first()
            .copied()
            .ok_or(VmViewError::ExpectedOnePassthroughGpu {
                count: passthrough_gpus.len(),
            })?;
    if passthrough_gpus.len() != 1 {
        return Err(VmViewError::ExpectedOnePassthroughGpu {
            count: passthrough_gpus.len(),
        });
    }

    let host_gpu = config.host_gpu(profile);
    if passthrough_mode != GpuPassthroughMode::SingleGpu && host_gpu.is_none() {
        return Err(VmViewError::ExpectedHostGpu {
            mode: passthrough_mode,
        });
    }

    let (host_monitor, vm_monitor) = match &config.monitor_plan {
        crate::vm::MonitorPlan::TwoMonitors {
            host_connector,
            vm_connector,
        } => (
            monitor_by_connector(profile, host_connector),
            monitor_by_connector(profile, vm_connector),
        ),
        crate::vm::MonitorPlan::OneMonitor { .. } => (None, None),
    };

    Ok(VmView {
        vm_name: config.vm_name.as_str(),
        guest_os: config.guest_os,
        ram_mb: config.resources.ram_mb,
        vcpu_count: config.resources.vcpu_count,
        disk: VmDiskView::from_choice(&config.resources.disk),
        passthrough_gpu,
        host_gpu,
        passthrough_mode,
        iso_path: config.iso_path.as_deref(),
        vm_monitor,
        host_monitor,
        input: VmInputView::from_config(config),
        use_hugepages: false,
        use_cpu_pinning: true,
        use_iothreads: true,
        enable_tpm: config.guest_os.requires_tpm(),
        enable_hyperv: config.guest_os.benefits_from_hyperv(),
        enable_secure_boot: config.guest_os.enables_secure_boot_by_default(),
        audio: config.audio,
        network: &config.network,
    })
}

fn monitor_by_connector<'a>(
    profile: &'a SystemProfile,
    connector: &str,
) -> Option<&'a MonitorInfo> {
    profile
        .monitors
        .iter()
        .find(|monitor| monitor.connector_name == connector)
}

fn disk_format_from_path(path: &Path) -> DiskFormat {
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.eq_ignore_ascii_case("qcow2"))
        .unwrap_or(false)
    {
        DiskFormat::Qcow2
    } else {
        DiskFormat::Raw
    }
}

#[cfg(test)]
mod tests {
    use super::{vm_view, VmViewError};
    use crate::detect::audio::AudioSystem;
    use crate::detect::bootloader::{BootloaderInfo, BootloaderKind};
    use crate::detect::cpu::CpuInfo;
    use crate::detect::display_manager::DisplayManager;
    use crate::detect::display_server::DisplayServer;
    use crate::detect::distro::{DistroFamily, DistroInfo, PackageManager};
    use crate::detect::gpu::{GpuInfo, GpuType, GpuVendor};
    use crate::detect::initramfs::InitramfsSystem;
    use crate::detect::memory::MemInfo;
    use crate::detect::readiness::{KernelHeadersInfo, OvmfInfo, ReadinessInfo, UserAccessInfo};
    use crate::detect::storage::StorageInfo;
    use crate::detect::virtualization::VirtInfo;
    use crate::detect::{MonitorInfo, SystemProfile};
    use crate::vm::{
        AudioChoice, DiskChoice, DiskFormat, GpuPassthroughMode, GpuRole, GpuRoleAssignment,
        GuestOs, InputChoice, LookingGlassChoice, MonitorPlan, NetworkChoice, PassthroughConfig,
        SingleMonitorStrategy, VmResources,
    };
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn vm_view_derives_xml_fields_from_passthrough_config() {
        let profile = dummy_profile();
        let mut config = dummy_config();
        config.guest_os = GuestOs::Windows10;
        config.iso_path = Some(PathBuf::from("/isos/windows.iso"));
        config.resources.disk = DiskChoice::Existing {
            path: PathBuf::from("/var/lib/libvirt/images/win.raw"),
        };
        config.network = NetworkChoice::Bridge {
            interface: "br0".to_string(),
        };
        config.input.keyboard_evdev = Some(PathBuf::from("/dev/input/by-id/kbd"));

        let view = match vm_view(&profile, &config) {
            Ok(view) => view,
            Err(err) => panic!("vm_view failed: {err}"),
        };

        assert_eq!(view.vm_name, "virtu-windows");
        assert_eq!(view.guest_os, GuestOs::Windows10);
        assert_eq!(view.passthrough_gpu.pci_slot, "0000:01:00.0");
        assert_eq!(
            view.host_gpu.map(|gpu| gpu.pci_slot.as_str()),
            Some("0000:02:00.0")
        );
        assert_eq!(view.passthrough_mode, GpuPassthroughMode::DualGpu);
        assert_eq!(view.ram_mb, 8192);
        assert_eq!(view.vcpu_count, 4);
        assert_eq!(view.disk.format, DiskFormat::Raw);
        assert!(!view.disk.create);
        assert_eq!(
            view.iso_path.map(|path| path.to_string_lossy().to_string()),
            Some("/isos/windows.iso".to_string())
        );
        assert!(view.enable_hyperv);
        assert!(!view.enable_tpm);
        assert!(!view.enable_secure_boot);
        assert_eq!(
            view.input
                .keyboard_evdev
                .map(|path| path.to_string_lossy().to_string()),
            Some("/dev/input/by-id/kbd".to_string())
        );
    }

    #[test]
    fn vm_view_rejects_multi_gpu_passthrough_for_now() {
        let mut profile = dummy_profile();
        profile
            .gpus
            .push(dummy_gpu("0000:03:00.0", GpuVendor::Intel));
        let mut config = dummy_config();
        config.gpu_roles.push(GpuRoleAssignment {
            pci_slot: "0000:03:00.0".to_string(),
            role: GpuRole::Passthrough,
        });

        let result = vm_view(&profile, &config);

        assert!(matches!(
            result,
            Err(VmViewError::MultiGpuPassthroughUnsupported)
        ));
    }

    fn dummy_config() -> PassthroughConfig {
        PassthroughConfig {
            vm_name: "virtu-windows".to_string(),
            guest_os: GuestOs::Windows11,
            gpu_mode: GpuPassthroughMode::DualGpu,
            gpu_roles: vec![
                GpuRoleAssignment {
                    pci_slot: "0000:01:00.0".to_string(),
                    role: GpuRole::Passthrough,
                },
                GpuRoleAssignment {
                    pci_slot: "0000:02:00.0".to_string(),
                    role: GpuRole::Host,
                },
            ],
            monitor_plan: MonitorPlan::OneMonitor {
                strategy: SingleMonitorStrategy::SwitchInputs,
            },
            looking_glass: LookingGlassChoice::Disabled,
            iso_path: None,
            resources: VmResources {
                ram_mb: 8192,
                vcpu_count: 4,
                disk: DiskChoice::Create {
                    path: PathBuf::from("/var/lib/libvirt/images/win.qcow2"),
                    size_gb: 100,
                    format: DiskFormat::Qcow2,
                },
            },
            network: NetworkChoice::Nat,
            audio: AudioChoice::None,
            input: InputChoice::default(),
        }
    }

    fn dummy_profile() -> SystemProfile {
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
            gpus: vec![
                dummy_gpu("0000:01:00.0", GpuVendor::Amd),
                dummy_gpu("0000:02:00.0", GpuVendor::Nvidia),
            ],
            iommu_groups: Vec::new(),
            ram: MemInfo {
                total_kb: 16 * 1024 * 1024,
                available_kb: 12 * 1024 * 1024,
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
            monitors: vec![MonitorInfo {
                connector_name: "DP-1".to_string(),
                connected: true,
                current_mode: Some("1920x1080".to_string()),
                card: "card0".to_string(),
                gpu_pci_slot: Some("0000:01:00.0".to_string()),
                is_internal: false,
            }],
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

    fn dummy_gpu(slot: &str, vendor: GpuVendor) -> GpuInfo {
        let (vendor_id, device_id, model_name, gpu_type) = match vendor {
            GpuVendor::Nvidia => ("10de", "1f08", "NVIDIA test GPU", GpuType::Discrete),
            GpuVendor::Amd => ("1002", "7590", "AMD test GPU", GpuType::Discrete),
            GpuVendor::Intel => ("8086", "46a6", "Intel test GPU", GpuType::Discrete),
            GpuVendor::Unknown(_) => ("ffff", "ffff", "Unknown test GPU", GpuType::Unknown),
        };

        GpuInfo {
            pci_slot: slot.to_string(),
            vendor,
            gpu_type,
            model_name: model_name.to_string(),
            vendor_id: vendor_id.to_string(),
            device_id: device_id.to_string(),
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
        }
    }
}
