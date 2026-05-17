pub mod devices;

use crate::detect::SystemProfile;
use crate::kb::KnowledgeBase;
use crate::vm::profile::{NetworkType, VmProfile};
use anyhow::Result;
use std::fmt::Write as FmtWrite;

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
        self.write_os(&mut xml)?;
        self.write_features(&mut xml)?;
        self.write_cpu(&mut xml)?;
        self.write_clock(&mut xml)?;
        self.write_memory(&mut xml)?;
        self.write_cpu_tune(&mut xml)?;
        self.write_devices(&mut xml)?;
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

    fn write_os(&self, xml: &mut String) -> Result<()> {
        writeln!(xml, "  <os firmware='efi'>")?;
        writeln!(xml, "    <type arch='x86_64' machine='q35'>hvm</type>")?;

        // OVMF path from KB per distro
        let ovmf_code = self
            .kb
            .paths_for_distro(&self.system.distro)
            .ovmf_code
            .as_deref()
            .unwrap_or("/usr/share/OVMF/OVMF_CODE.fd");
        let ovmf_vars = self
            .kb
            .paths_for_distro(&self.system.distro)
            .ovmf_vars
            .as_deref()
            .unwrap_or("/usr/share/OVMF/OVMF_VARS.fd");

        writeln!(
            xml,
            "    <loader readonly='yes' type='pflash'>{ovmf_code}</loader>"
        )?;
        writeln!(xml, "    <nvram>{ovmf_vars}</nvram>")?;

        if self.profile.enable_secure_boot {
            writeln!(xml, "    <smmbios mode='host'/>")?;
        }

        writeln!(xml, "    <bootmenu enable='yes' timeout='3000'/>")?;

        // Boot order: disk first, then CD-ROM
        if self.profile.iso_path.is_some() {
            writeln!(xml, "    <boot dev='cdrom'/>")?;
        }
        writeln!(xml, "    <boot dev='hd'/>")?;

        writeln!(xml, "  </os>")?;
        Ok(())
    }

    fn write_features(&self, xml: &mut String) -> Result<()> {
        writeln!(xml, "  <features>")?;
        writeln!(xml, "    <acpi/>")?;
        writeln!(xml, "    <apic/>")?;

        // Hyper-V enlightenments for Windows guests — significant performance improvement
        if self.profile.enable_hyperv {
            writeln!(xml, "    <hyperv mode='custom'>")?;
            writeln!(xml, "      <relaxed state='on'/>")?;
            writeln!(xml, "      <vapic state='on'/>")?;
            writeln!(xml, "      <spinlocks state='on' retries='8191'/>")?;
            writeln!(xml, "      <vpindex state='on'/>")?;
            writeln!(xml, "      <synic state='on'/>")?;
            writeln!(xml, "      <stimer state='on' direct='on'/>")?;
            writeln!(xml, "      <reset state='on'/>")?;
            writeln!(xml, "      <frequencies state='on'/>")?;
            writeln!(xml, "      <reenlightenment state='on'/>")?;
            writeln!(xml, "      <tlbflush state='on'/>")?;
            writeln!(xml, "      <ipi state='on'/>")?;
            writeln!(xml, "    </hyperv>")?;
            writeln!(xml, "    <ioapic driver='kvm'/>")?;
        }

        // NVIDIA Error 43 fix: hide the KVM signature from the guest
        if self.profile.passthrough_gpu.vendor == crate::detect::gpu::GpuVendor::Nvidia {
            writeln!(xml, "    <kvm>")?;
            writeln!(xml, "      <hidden state='on'/>")?;
            writeln!(xml, "    </kvm>")?;
        }

        writeln!(xml, "  </features>")?;
        Ok(())
    }

    fn write_cpu(&self, xml: &mut String) -> Result<()> {
        writeln!(
            xml,
            "  <cpu mode='host-passthrough' check='none' migratable='off'>"
        )?;

        // Physical topology
        let threads_per_core = if self.system.cpu.has_hyperthreading {
            2
        } else {
            1
        };
        let vcpu_count = self.profile.vcpu_count;
        let sockets = 1u32;
        let cores = (vcpu_count / threads_per_core).max(1);
        let threads = threads_per_core;

        writeln!(
            xml,
            "    <topology sockets='{sockets}' dies='1' cores='{cores}' threads='{threads}'/>"
        )?;

        // Host cache passthrough — eliminates cache-related latency
        writeln!(xml, "    <cache mode='passthrough'/>")?;

        // AMD-specific: expose topology extension for correct core detection in guest
        if self.system.cpu.vendor.contains("AMD") || self.system.cpu.vendor == "AuthenticAMD" {
            writeln!(xml, "    <feature policy='require' name='topoext'/>")?;
        }

        // NVIDIA vendor ID spoof to bypass VM detection in driver
        if self.profile.passthrough_gpu.vendor == crate::detect::gpu::GpuVendor::Nvidia
            && self.profile.enable_hyperv
        {
            writeln!(xml, "    <vendor_id state='on' value='AuthenticAMD'/>")?;
        }

        writeln!(xml, "  </cpu>")?;
        writeln!(xml, "  <vcpu placement='static'>{vcpu_count}</vcpu>")?;

        Ok(())
    }

    fn write_clock(&self, xml: &mut String) -> Result<()> {
        // Windows guests: use localtime to avoid clock issues
        let offset = if self.profile.guest_os.benefits_from_hyperv() {
            "localtime"
        } else {
            "utc"
        };

        writeln!(xml, "  <clock offset='{offset}'>")?;
        writeln!(xml, "    <timer name='rtc' tickpolicy='catchup'/>")?;
        writeln!(xml, "    <timer name='pit' tickpolicy='delay'/>")?;
        writeln!(xml, "    <timer name='hpet' present='no'/>")?;

        if self.profile.enable_hyperv {
            writeln!(xml, "    <timer name='hypervclock' present='yes'/>")?;
        }

        writeln!(xml, "  </clock>")?;
        Ok(())
    }

    fn write_memory(&self, xml: &mut String) -> Result<()> {
        let ram_kb = self.profile.ram_mb * 1024;
        writeln!(xml, "  <memory unit='KiB'>{ram_kb}</memory>")?;
        writeln!(xml, "  <currentMemory unit='KiB'>{ram_kb}</currentMemory>")?;

        if self.profile.use_hugepages {
            writeln!(xml, "  <memoryBacking>")?;
            writeln!(xml, "    <hugepages/>")?;
            writeln!(xml, "    <nosharepages/>")?;
            writeln!(xml, "    <locked/>")?;
            writeln!(xml, "    <source type='memfd'/>")?;
            writeln!(xml, "    <access mode='shared'/>")?;
            writeln!(xml, "  </memoryBacking>")?;
        }

        Ok(())
    }

    fn write_cpu_tune(&self, xml: &mut String) -> Result<()> {
        if !self.profile.use_cpu_pinning {
            return Ok(());
        }

        let pinning =
            super::cpu_topology::calculate_pinning(&self.system.cpu, self.profile.vcpu_count);

        writeln!(xml, "  <cputune>")?;

        for (vcpu, cpuset) in &pinning.vcpu_pins {
            writeln!(xml, "    <vcpupin vcpu='{vcpu}' cpuset='{cpuset}'/>")?;
        }

        writeln!(
            xml,
            "    <emulatorpin cpuset='{}'/>",
            pinning.emulator_cpuset
        )?;

        if self.profile.use_iothreads {
            writeln!(
                xml,
                "    <iothreadpin iothread='1' cpuset='{}'/>",
                pinning.emulator_cpuset
            )?;
        }

        writeln!(xml, "  </cputune>")?;

        if self.profile.use_iothreads {
            writeln!(xml, "  <iothreads>1</iothreads>")?;
        }

        Ok(())
    }

    fn write_devices(&self, xml: &mut String) -> Result<()> {
        writeln!(xml, "  <devices>")?;
        writeln!(xml, "    <emulator>/usr/bin/qemu-system-x86_64</emulator>")?;

        // Main disk
        self.write_disk(xml)?;

        // ISO if present
        if let Some(iso) = &self.profile.iso_path {
            writeln!(xml, "    <disk type='file' device='cdrom'>")?;
            writeln!(xml, "      <driver name='qemu' type='raw'/>")?;
            writeln!(xml, "      <source file='{}'/>", iso.display())?;
            writeln!(xml, "      <target dev='sdb' bus='scsi'/>")?;
            writeln!(xml, "      <readonly/>")?;
            writeln!(xml, "    </disk>")?;
        }

        // SCSI controller for virtio-scsi
        writeln!(
            xml,
            "    <controller type='scsi' index='0' model='virtio-scsi'>"
        )?;
        if self.profile.use_iothreads {
            writeln!(xml, "      <driver iothread='1'/>")?;
        }
        writeln!(xml, "    </controller>")?;

        // Network
        self.write_network(xml)?;

        // GPU passthrough
        self.write_gpu_hostdev(xml)?;

        // USB controllers
        writeln!(
            xml,
            "    <controller type='usb' model='qemu-xhci' ports='15'/>"
        )?;

        // Evdev keyboard passthrough
        if let Some(kbd) = &self.profile.evdev_keyboard {
            if let Some(path) = &kbd.evdev_path {
                writeln!(xml, "    <input type='evdev'>")?;
                writeln!(
                    xml,
                    "      <source dev='{}' grab='all' grabToggle='ctrl-ctrl' repeat='on'/>",
                    path.display()
                )?;
                writeln!(xml, "    </input>")?;
            }
        }

        // Evdev mouse passthrough
        if let Some(mouse) = &self.profile.evdev_mouse {
            if let Some(path) = &mouse.evdev_path {
                writeln!(xml, "    <input type='evdev'>")?;
                writeln!(xml, "      <source dev='{}'/>", path.display())?;
                writeln!(xml, "    </input>")?;
            }
        }

        // Tablet input for cursor integration when no evdev is used
        if self.profile.evdev_keyboard.is_none() {
            writeln!(xml, "    <input type='tablet' bus='usb'/>")?;
        }

        // Audio
        self.write_audio(xml)?;

        // Looking Glass IVSHMEM
        if self.profile.looking_glass.enabled {
            writeln!(xml, "    <shmem name='looking-glass'>")?;
            writeln!(xml, "      <model type='ivshmem-plain'/>")?;
            writeln!(
                xml,
                "      <size unit='M'>{}</size>",
                self.profile.looking_glass.buffer_size_mb
            )?;
            writeln!(xml, "    </shmem>")?;
        }

        // TPM for Windows 11
        if self.profile.enable_tpm {
            writeln!(xml, "    <tpm model='tpm-crb'>")?;
            writeln!(xml, "      <backend type='emulator' version='2.0'/>")?;
            writeln!(xml, "    </tpm>")?;
        }

        // Serial console (useful for debugging)
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
        Ok(())
    }

    fn write_disk(&self, xml: &mut String) -> Result<()> {
        let disk_format = if self
            .profile
            .disk_path
            .extension()
            .map(|e| e == "qcow2")
            .unwrap_or(false)
        {
            "qcow2"
        } else {
            "raw"
        };

        writeln!(xml, "    <disk type='file' device='disk'>")?;
        writeln!(xml, "      <driver name='qemu' type='{disk_format}' cache='none' io='native' discard='unmap'{}/>",
            if self.profile.use_iothreads { " iothread='1'" } else { "" })?;
        writeln!(
            xml,
            "      <source file='{}'/>",
            self.profile.disk_path.display()
        )?;
        writeln!(xml, "      <target dev='sda' bus='scsi'/>")?;
        writeln!(xml, "      <boot order='1'/>")?;
        writeln!(xml, "    </disk>")?;
        Ok(())
    }

    fn write_network(&self, xml: &mut String) -> Result<()> {
        let vcpu_count = self.profile.vcpu_count;
        // vhost queues should match vCPU count for best throughput
        let queues = vcpu_count.min(8); // Practical maximum

        match &self.profile.network_type {
            NetworkType::Nat => {
                writeln!(xml, "    <interface type='network'>")?;
                writeln!(xml, "      <source network='default'/>")?;
                writeln!(xml, "      <model type='virtio'/>")?;
                writeln!(xml, "      <driver name='vhost' queues='{queues}'/>")?;
                writeln!(xml, "    </interface>")?;
            }
            NetworkType::Bridge { interface } => {
                writeln!(xml, "    <interface type='bridge'>")?;
                writeln!(xml, "      <source bridge='{interface}'/>")?;
                writeln!(xml, "      <model type='virtio'/>")?;
                writeln!(xml, "      <driver name='vhost' queues='{queues}'/>")?;
                writeln!(xml, "    </interface>")?;
            }
        }
        Ok(())
    }

    fn write_gpu_hostdev(&self, xml: &mut String) -> Result<()> {
        let gpu = &self.profile.passthrough_gpu;

        // Parse PCI slot "0000:01:00.0" → domain, bus, slot, function
        let (domain, bus, slot, function) = parse_pci_slot(&gpu.pci_slot).unwrap_or((0, 1, 0, 0));

        writeln!(xml, "    <!-- GPU: {} -->", gpu.model_name)?;
        writeln!(
            xml,
            "    <hostdev mode='subsystem' type='pci' managed='yes'>"
        )?;
        writeln!(xml, "      <source>")?;
        writeln!(xml, "        <address domain='0x{domain:04x}' bus='0x{bus:02x}' slot='0x{slot:02x}' function='0x{function:01x}'/>")?;
        writeln!(xml, "      </source>")?;

        // Add ROM if accessible
        if gpu.rom_accessible {
            let rom_path = format!(
                "/var/lib/libvirt/vbios/{}.rom",
                gpu.pci_slot.replace(':', "_").replace('.', "_")
            );
            writeln!(xml, "      <rom file='{rom_path}'/>")?;
        }

        writeln!(xml, "    </hostdev>")?;

        // Companion audio device
        if let Some(audio) = &gpu.companion_audio {
            let (d, b, s, f) = parse_pci_slot(&audio.pci_slot).unwrap_or((0, 1, 0, 1));
            writeln!(xml, "    <!-- GPU Audio companion -->")?;
            writeln!(
                xml,
                "    <hostdev mode='subsystem' type='pci' managed='yes'>"
            )?;
            writeln!(xml, "      <source>")?;
            writeln!(xml, "        <address domain='0x{d:04x}' bus='0x{b:02x}' slot='0x{s:02x}' function='0x{f:01x}'/>")?;
            writeln!(xml, "      </source>")?;
            writeln!(xml, "    </hostdev>")?;
        }

        Ok(())
    }

    fn write_audio(&self, xml: &mut String) -> Result<()> {
        use crate::vm::profile::AudioPassthroughMethod;

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

        // AMD reset bug mitigation — add pcie_acs_override if needed
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

/// Parse "0000:01:00.0" into (domain, bus, slot, function) as u32 values
fn parse_pci_slot(slot: &str) -> Option<(u32, u32, u32, u32)> {
    let parts: Vec<&str> = slot.split(':').collect();
    if parts.len() < 3 {
        return None;
    }
    let domain = u32::from_str_radix(parts[0], 16).ok()?;
    let bus = u32::from_str_radix(parts[1], 16).ok()?;
    let dev_fn: Vec<&str> = parts[2].split('.').collect();
    let device = u32::from_str_radix(dev_fn.get(0)?, 16).ok()?;
    let function = u32::from_str_radix(dev_fn.get(1)?, 16).ok()?;
    Some((domain, bus, device, function))
}

#[cfg(unix)]
fn current_uid() -> u32 {
    unsafe { libc::getuid() }
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    1000
}
