//! Pure VM XML generation entry point for Phase B.

use crate::detect::SystemProfile;
use crate::kb::KnowledgeBase;
use crate::vm::xml::{XmlBuilder, XmlError};
use crate::vm::{vm_view, PassthroughConfig};

/// Generate a complete libvirt domain XML string from detected host state and
/// the user's passthrough choices.
///
/// This function is read-only: it does not write files, validate with
/// `virt-xml-validate`, invoke `virsh`, or create disk images.
pub fn generate_vm_xml(
    profile: &SystemProfile,
    config: &PassthroughConfig,
) -> Result<String, XmlError> {
    let view = vm_view(profile, config)?;
    let kb = KnowledgeBase::bundled();

    XmlBuilder::new(&view, profile, &kb).build()
}

#[cfg(test)]
mod tests {
    use super::generate_vm_xml;
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
    use crate::vm::profile::VmViewError;
    use crate::vm::xml::XmlError;
    use crate::vm::{
        AudioChoice, DiskChoice, DiskFormat, GpuPassthroughMode, GpuRole, GpuRoleAssignment,
        GuestOs, InputChoice, LookingGlassChoice, MonitorPlan, NetworkChoice, PassthroughConfig,
        SingleMonitorStrategy, VmResources,
    };
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn generate_vm_xml_builds_domain_from_passthrough_config() {
        let profile = dummy_profile();
        let config = dummy_config();

        let xml = match generate_vm_xml(&profile, &config) {
            Ok(xml) => xml,
            Err(err) => panic!("XML generation failed: {err}"),
        };

        assert!(xml.contains("<domain type='kvm'"));
        assert!(xml.contains("<name>virtu-windows</name>"));
        assert!(xml.contains("<disk type='file' device='disk'>"));
        assert!(xml.contains("<hostdev mode='subsystem' type='pci' managed='yes'>"));
        assert!(xml.contains("<features>"));
        assert!(!xml.contains("<shmem name='looking-glass'>"));
        assert!(!xml.contains("ivshmem"));
    }

    #[test]
    fn generate_vm_xml_propagates_vm_view_failures() {
        let mut profile = dummy_profile();
        profile
            .gpus
            .push(dummy_gpu("0000:03:00.0", GpuVendor::Intel));

        let mut config = dummy_config();
        config.gpu_mode = GpuPassthroughMode::MultiGpu;
        config.gpu_roles.push(GpuRoleAssignment {
            pci_slot: "0000:03:00.0".to_string(),
            role: GpuRole::Passthrough,
        });

        let result = generate_vm_xml(&profile, &config);

        assert!(matches!(
            result,
            Err(XmlError::View(VmViewError::MultiGpuPassthroughUnsupported))
        ));
    }

    /// Real-host smoke for the schema-correct Secure Boot XML.
    /// Renders the default Windows-11 plan (which has
    /// `enable_secure_boot=true` because of `GuestOs::Windows11`)
    /// and feeds it to `virt-xml-validate`. Catches any future
    /// regression that re-introduces the `<smmbios>` typo or moves
    /// `<smm>` back to the wrong block.
    ///
    /// Gated by `VIRTU_RUN_VIRT_XML_VALIDATE_SMOKE=1` so the normal
    /// `cargo test` run stays hermetic. Skips on hosts where
    /// `virt-xml-validate` is not on PATH.
    #[test]
    fn generate_vm_xml_passes_virt_xml_validate_for_default_secure_boot_plan() {
        if std::env::var("VIRTU_RUN_VIRT_XML_VALIDATE_SMOKE")
            .ok()
            .as_deref()
            != Some("1")
        {
            return;
        }
        if which::which("virt-xml-validate").is_err() {
            return;
        }

        let profile = dummy_profile();
        let config = dummy_config();
        let xml = generate_vm_xml(&profile, &config).expect("XML must render");

        // Regression pins on the bytes themselves: the schema-correct
        // SMM lives in <features>, the legacy typo must not appear.
        assert!(
            xml.contains("<smm state='on'/>"),
            "default Windows-11 plan must enable SMM via <features>"
        );
        assert!(
            !xml.contains("smmbios"),
            "smmbios is not a valid libvirt element"
        );

        // And the ground-truth: libvirt's schema accepts it.
        crate::config::writers::commands::validate_xml(&xml)
            .expect("virt-xml-validate must accept the default Windows-11 XML");
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

    fn dummy_gpu(slot: &str, vendor: GpuVendor) -> GpuInfo {
        let (vendor_id, device_id, model_name, gpu_type) = match vendor {
            GpuVendor::Nvidia => ("10de", "1f08", "NVIDIA test GPU", GpuType::Discrete),
            GpuVendor::Amd => ("1002", "7590", "AMD test GPU", GpuType::Discrete),
            GpuVendor::Intel => ("8086", "46a6", "Intel test GPU", GpuType::Integrated),
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
