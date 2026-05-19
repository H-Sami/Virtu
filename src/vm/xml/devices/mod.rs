pub mod cpu;
pub mod disk;
pub mod features;
pub mod firmware;
pub mod gpu_hostdev;
pub mod input;
pub mod memory;
pub mod network;
pub mod tpm;

#[cfg(test)]
pub(crate) mod fixtures {
    //! Deterministic fixtures shared by per-device golden tests.
    //!
    //! Each device renderer is byte-deterministic given a `VmView`. The
    //! shared helpers here let every device-level test build the same
    //! `VmView` (or close variants of it) so the goldens stay easy to
    //! audit. The orchestrator's identity block (UUID, etc.) is
    //! intentionally not exercised here — it is non-deterministic by
    //! design and lives in the higher-level `engine::vm_xml` test.

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
    use crate::detect::SystemProfile;
    use crate::vm::passthrough::{
        AudioChoice, DiskChoice, DiskFormat, GpuPassthroughMode, GpuRole, GpuRoleAssignment,
        GuestOs, InputChoice, LookingGlassChoice, MonitorPlan, NetworkChoice, PassthroughConfig,
        SingleMonitorStrategy, VmResources,
    };
    use std::collections::HashMap;
    use std::path::PathBuf;

    /// Stable two-GPU profile (AMD passthrough + NVIDIA host) with 8
    /// physical / 16 logical cores. Used as the default host for golden
    /// tests across the device renderers.
    pub fn amd_host_with_amd_passthrough() -> SystemProfile {
        let mut core_to_threads: HashMap<u32, Vec<u32>> = HashMap::new();
        for core in 0..8u32 {
            core_to_threads.insert(core, vec![core, core + 8]);
        }
        SystemProfile {
            cpu: CpuInfo {
                vendor: "AuthenticAMD".to_string(),
                model_name: "AMD Ryzen 7 5700X3D".to_string(),
                physical_cores: 8,
                logical_cores: 16,
                numa_nodes: Vec::new(),
                iommu_capable: true,
                iommu_enabled: true,
                has_hyperthreading: true,
                core_to_threads,
            },
            gpus: vec![gpu_amd_passthrough(), gpu_nvidia_host()],
            iommu_groups: Vec::new(),
            ram: MemInfo {
                total_kb: 32 * 1024 * 1024,
                available_kb: 24 * 1024 * 1024,
                hugepages_total: 0,
                hugepages_free: 0,
                hugepage_size_kb: 2048,
            },
            distro: DistroInfo {
                id: "arch".to_string(),
                id_like: Vec::new(),
                pretty_name: "Arch Linux".to_string(),
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

    /// Same shape but with the NVIDIA card as the passthrough target.
    /// Useful for testing NVIDIA-specific quirks (KVM hidden, fake
    /// Hyper-V vendor id) in `cpu.rs` and `features.rs` without
    /// duplicating the whole profile.
    pub fn nvidia_passthrough_profile() -> SystemProfile {
        let mut profile = amd_host_with_amd_passthrough();
        profile.gpus = vec![gpu_nvidia_passthrough(), gpu_amd_host()];
        profile
    }

    /// Stable Windows-11 dual-GPU config matching the AMD-passthrough
    /// profile above. Disk is a fresh 100 GiB qcow2; network is NAT;
    /// no ISO; no evdev; no Looking Glass.
    pub fn windows_dual_gpu_config_amd_passthrough() -> PassthroughConfig {
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
                    path: PathBuf::from("/var/lib/libvirt/images/virtu-windows.qcow2"),
                    size_gb: 100,
                    format: DiskFormat::Qcow2,
                },
            },
            network: NetworkChoice::Nat,
            audio: AudioChoice::None,
            input: InputChoice::default(),
        }
    }

    /// Same shape, but `gpu_roles` are swapped so the NVIDIA card is the
    /// passthrough target. Pairs with `nvidia_passthrough_profile`.
    pub fn windows_dual_gpu_config_nvidia_passthrough() -> PassthroughConfig {
        let mut config = windows_dual_gpu_config_amd_passthrough();
        config.gpu_roles = vec![
            GpuRoleAssignment {
                pci_slot: "0000:01:00.0".to_string(),
                role: GpuRole::Passthrough,
            },
            GpuRoleAssignment {
                pci_slot: "0000:02:00.0".to_string(),
                role: GpuRole::Host,
            },
        ];
        config
    }

    fn gpu_amd_passthrough() -> GpuInfo {
        GpuInfo {
            pci_slot: "0000:01:00.0".to_string(),
            vendor: GpuVendor::Amd,
            gpu_type: GpuType::Discrete,
            model_name: "AMD Radeon RX 9060 XT".to_string(),
            vendor_id: "1002".to_string(),
            device_id: "7590".to_string(),
            subsystem_vendor_id: "1002".to_string(),
            subsystem_device_id: "7590".to_string(),
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

    fn gpu_nvidia_host() -> GpuInfo {
        GpuInfo {
            pci_slot: "0000:02:00.0".to_string(),
            vendor: GpuVendor::Nvidia,
            gpu_type: GpuType::Discrete,
            model_name: "NVIDIA RTX 2060".to_string(),
            vendor_id: "10de".to_string(),
            device_id: "1f08".to_string(),
            subsystem_vendor_id: "10de".to_string(),
            subsystem_device_id: "1f08".to_string(),
            current_driver: None,
            iommu_group_id: Some(2),
            iommu_isolated: true,
            rom_accessible: false,
            companion_audio: None,
            is_boot_vga: false,
            vfio_compatible: true,
            quirks: Vec::new(),
        }
    }

    fn gpu_amd_host() -> GpuInfo {
        let mut gpu = gpu_amd_passthrough();
        gpu.pci_slot = "0000:02:00.0".to_string();
        gpu.iommu_group_id = Some(2);
        gpu
    }

    fn gpu_nvidia_passthrough() -> GpuInfo {
        let mut gpu = gpu_nvidia_host();
        gpu.pci_slot = "0000:01:00.0".to_string();
        gpu.iommu_group_id = Some(1);
        gpu
    }
}
