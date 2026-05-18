pub mod devices;

use crate::detect::SystemProfile;
use crate::kb::KnowledgeBase;
use crate::vm::profile::{AudioPassthroughMethod, VmProfile};
use anyhow::Result;
use std::fmt::Write as FmtWrite;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum XmlError {
    #[error("failed to write XML fragment")]
    Format(#[from] std::fmt::Error),
}

/// Generates a complete, performance-tuned libvirt domain XML string
/// from the VM profile and system profile.
pub struct XmlBuilder<'a> {
    profile: &'a VmProfile,
    system: &'a SystemProfile,
    kb: &'a KnowledgeBase,
}

impl<'a> XmlBuilder<'a> {
    pub fn new(profile: &'a VmProfile, system: &'a SystemProfile, kb: &'a KnowledgeBase) -> Self {
        Self {
            profile,
            system,
            kb,
        }
    }

    pub fn build(&self) -> Result<String> {
        let mut xml = String::with_capacity(8192);

        writeln!(
            xml,
            "<domain type='kvm' xmlns:qemu='http://libvirt.org/schemas/domain/qemu/1.0'>"
        )?;

        self.write_identity(&mut xml)?;
        xml.push_str(&devices::firmware::render(
            self.profile,
            self.system,
            self.kb,
        )?);
        xml.push_str(&devices::features::render(self.profile)?);
        xml.push_str(&devices::cpu::render(self.profile, self.system)?);
        xml.push_str(&devices::memory::render(self.profile)?);
        xml.push_str(&self.render_devices()?);
        self.write_qemu_commandline(&mut xml)?;

        writeln!(xml, "</domain>")?;

        Ok(xml)
    }

    fn write_identity(&self, xml: &mut String) -> Result<()> {
        writeln!(xml, "  <name>{}</name>", self.profile.vm_name)?;
        writeln!(xml, "  <uuid>{}</uuid>", uuid::Uuid::new_v4())?;
        writeln!(
            xml,
            "  <title>Virtu VM — {}</title>",
            self.profile.guest_os.display_name()
        )?;
        writeln!(
            xml,
            "  <description>Created by Virtu GPU Passthrough Tool</description>"
        )?;
        Ok(())
    }

    fn render_devices(&self) -> Result<String> {
        let mut xml = String::new();

        writeln!(xml, "  <devices>")?;
        writeln!(xml, "    <emulator>/usr/bin/qemu-system-x86_64</emulator>")?;
        xml.push_str(&devices::disk::render(self.profile)?);
        xml.push_str(&devices::network::render(self.profile)?);
        xml.push_str(&devices::gpu_hostdev::render(self.profile)?);
        xml.push_str(&devices::input::render(self.profile)?);
        self.write_audio(&mut xml)?;
        xml.push_str(&devices::tpm::render(self.profile)?);

        writeln!(
            xml,
            "    <serial type='pty'><target type='isa-serial' port='0'/></serial>"
        )?;

        // VNC display as fallback (no password, localhost only)
        writeln!(
            xml,
            "    <graphics type='vnc' port='-1' autoport='yes' listen='127.0.0.1'>"
        )?;
        writeln!(xml, "      <listen type='address' address='127.0.0.1'/>")?;
        writeln!(xml, "    </graphics>")?;

        writeln!(xml, "  </devices>")?;
        Ok(xml)
    }

    fn write_audio(&self, xml: &mut String) -> Result<()> {
        match &self.profile.audio_passthrough {
            AudioPassthroughMethod::HostAudio => {
                let audio_type = self.system.audio.libvirt_audio_type();
                writeln!(xml, "    <sound model='ich9'>")?;
                writeln!(xml, "      <audio id='1'/>")?;
                writeln!(xml, "    </sound>")?;

                let uid = current_uid();
                writeln!(
                    xml,
                    "    <audio id='1' type='{audio_type}' runtimeDir='/run/user/{uid}'/>"
                )?;
            }
            AudioPassthroughMethod::Scream => {
                writeln!(xml, "    <sound model='ich9'/>")?;
                writeln!(
                    xml,
                    "    <!-- Scream: install Scream in Windows guest for audio -->"
                )?;
            }
            AudioPassthroughMethod::None => {}
        }

        Ok(())
    }

    fn write_qemu_commandline(&self, xml: &mut String) -> Result<()> {
        let gpu = &self.profile.passthrough_gpu;

        // AMD reset bug mitigation.
        let has_reset_bug = self
            .kb
            .quirks_for_gpu(&gpu.vendor_id, &gpu.device_id)
            .iter()
            .any(|q| q.issue_id == "reset-bug");

        if has_reset_bug {
            writeln!(xml, "  <qemu:commandline>")?;
            writeln!(xml, "    <!-- AMD reset bug mitigation -->")?;
            writeln!(xml, "    <qemu:arg value='-global'/>")?;
            writeln!(xml, "    <qemu:arg value='PIIX4_PM.disable_s3=1'/>")?;
            writeln!(xml, "    <qemu:arg value='-global'/>")?;
            writeln!(xml, "    <qemu:arg value='PIIX4_PM.disable_s4=1'/>")?;
            writeln!(xml, "  </qemu:commandline>")?;
        }

        Ok(())
    }
}

#[cfg(unix)]
fn current_uid() -> u32 {
    unsafe { libc::getuid() }
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    1000
}

#[cfg(test)]
mod tests {
    use super::XmlBuilder;
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
    use crate::kb::KnowledgeBase;
    use crate::vm::profile::{
        AudioPassthroughMethod, GuestOs, LookingGlassConfig, NetworkType, PassthroughMode,
        VmProfile,
    };
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn builder_does_not_emit_looking_glass_shmem_for_v1() {
        let system = dummy_system_profile();
        let mut profile = dummy_vm_profile();
        profile.looking_glass = LookingGlassConfig {
            enabled: true,
            auto_compile: false,
            target_width: 1920,
            target_height: 1080,
            buffer_size_mb: 64,
        };
        let kb = KnowledgeBase::default();
        let xml = match XmlBuilder::new(&profile, &system, &kb).build() {
            Ok(xml) => xml,
            Err(err) => panic!("builder failed: {err}"),
        };

        assert!(!xml.contains("<shmem name='looking-glass'>"));
        assert!(!xml.contains("ivshmem"));
    }

    fn dummy_vm_profile() -> VmProfile {
        VmProfile {
            vm_name: "virtu-windows".to_string(),
            guest_os: GuestOs::Windows11,
            ram_mb: 8192,
            vcpu_count: 4,
            disk_path: PathBuf::from("/var/lib/libvirt/images/virtu-windows.qcow2"),
            disk_size_gb: 100,
            disk_exists: false,
            passthrough_gpu: dummy_gpu("0000:01:00.0", GpuVendor::Amd),
            host_gpu: dummy_gpu("0000:02:00.0", GpuVendor::Nvidia),
            passthrough_mode: PassthroughMode::DualGpu,
            iso_path: None,
            looking_glass: LookingGlassConfig::disabled(),
            vm_monitor: None,
            host_monitor: None,
            evdev_keyboard: None,
            evdev_mouse: None,
            additional_evdev: Vec::new(),
            use_hugepages: false,
            use_cpu_pinning: false,
            use_iothreads: false,
            enable_tpm: true,
            enable_hyperv: true,
            enable_secure_boot: false,
            audio_passthrough: AudioPassthroughMethod::None,
            network_type: NetworkType::Nat,
        }
    }

    fn dummy_gpu(slot: &str, vendor: GpuVendor) -> GpuInfo {
        let (vendor_id, device_id, model_name) = match vendor {
            GpuVendor::Nvidia => ("10de", "1f08", "NVIDIA test GPU"),
            GpuVendor::Amd => ("1002", "7590", "AMD test GPU"),
            GpuVendor::Intel => ("8086", "46a6", "Intel test GPU"),
            GpuVendor::Unknown(_) => ("ffff", "ffff", "Unknown test GPU"),
        };

        GpuInfo {
            pci_slot: slot.to_string(),
            vendor,
            gpu_type: GpuType::Discrete,
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

    fn dummy_system_profile() -> SystemProfile {
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
            gpus: Vec::new(),
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
}
