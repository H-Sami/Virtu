use crate::vm::profile::VmView;
use crate::vm::xml::XmlError;
use std::fmt::Write as FmtWrite;

pub fn render(view: &VmView<'_>) -> Result<String, XmlError> {
    let mut xml = String::new();
    let gpu = view.passthrough_gpu;
    let (domain, bus, slot, function) = parse_pci_slot(&gpu.pci_slot).unwrap_or((0, 1, 0, 0));

    writeln!(xml, "    <!-- GPU: {} -->", gpu.model_name)?;
    writeln!(
        xml,
        "    <hostdev mode='subsystem' type='pci' managed='yes'>"
    )?;
    writeln!(xml, "      <source>")?;
    writeln!(
        xml,
        "        <address domain='0x{domain:04x}' bus='0x{bus:02x}' slot='0x{slot:02x}' function='0x{function:01x}'/>"
    )?;
    writeln!(xml, "      </source>")?;

    if gpu.rom_accessible {
        let rom_path = format!(
            "/var/lib/libvirt/vbios/{}.rom",
            gpu.pci_slot.replace([':', '.'], "_")
        );
        writeln!(xml, "      <rom file='{rom_path}'/>")?;
    }

    writeln!(xml, "    </hostdev>")?;

    if let Some(audio) = &gpu.companion_audio {
        let (domain, bus, slot, function) = parse_pci_slot(&audio.pci_slot).unwrap_or((0, 1, 0, 1));
        writeln!(xml, "    <!-- GPU Audio companion -->")?;
        writeln!(
            xml,
            "    <hostdev mode='subsystem' type='pci' managed='yes'>"
        )?;
        writeln!(xml, "      <source>")?;
        writeln!(
            xml,
            "        <address domain='0x{domain:04x}' bus='0x{bus:02x}' slot='0x{slot:02x}' function='0x{function:01x}'/>"
        )?;
        writeln!(xml, "      </source>")?;
        writeln!(xml, "    </hostdev>")?;
    }

    Ok(xml)
}

fn parse_pci_slot(slot: &str) -> Option<(u32, u32, u32, u32)> {
    let parts: Vec<&str> = slot.split(':').collect();
    if parts.len() < 3 {
        return None;
    }

    let domain = u32::from_str_radix(parts[0], 16).ok()?;
    let bus = u32::from_str_radix(parts[1], 16).ok()?;
    let dev_fn: Vec<&str> = parts[2].split('.').collect();
    let device = u32::from_str_radix(dev_fn.first()?, 16).ok()?;
    let function = u32::from_str_radix(dev_fn.get(1)?, 16).ok()?;

    Some((domain, bus, device, function))
}

#[cfg(test)]
mod tests {
    use super::render;
    use crate::vm::profile::vm_view;
    use crate::vm::xml::devices::fixtures::{
        amd_host_with_amd_passthrough, windows_dual_gpu_config_amd_passthrough,
    };

    /// Default AMD passthrough at 0000:01:00.0 with no companion audio:
    /// renders one `<hostdev>` block with the canonical PCI address
    /// breakdown libvirt expects.
    #[test]
    fn gpu_hostdev_renderer_emits_managed_pci_address_for_amd_card() {
        let profile = amd_host_with_amd_passthrough();
        let config = windows_dual_gpu_config_amd_passthrough();
        let view = vm_view(&profile, &config).expect("view");

        let xml = render(&view).expect("render");
        assert!(xml.contains("<!-- GPU: AMD Radeon RX 9060 XT -->"));
        assert!(xml.contains("<hostdev mode='subsystem' type='pci' managed='yes'>"));
        assert!(xml.contains("<address domain='0x0000' bus='0x01' slot='0x00' function='0x0'/>"));
        // No companion audio in the default fixture.
        assert!(!xml.contains("GPU Audio companion"));
        // ROM file only emitted when rom_accessible = true.
        assert!(!xml.contains("<rom file="));
    }

    /// Companion audio, when detected, gets its own `<hostdev>` block
    /// using its own PCI address. This is the classic NVIDIA HDMI-audio
    /// case where the audio function lives at `<slot>.1`.
    #[test]
    fn gpu_hostdev_renderer_emits_companion_audio_hostdev() {
        use crate::detect::gpu::CompanionDevice;
        let mut profile = amd_host_with_amd_passthrough();
        if let Some(gpu) = profile
            .gpus
            .iter_mut()
            .find(|g| g.pci_slot == "0000:01:00.0")
        {
            gpu.companion_audio = Some(CompanionDevice {
                pci_slot: "0000:01:00.1".to_string(),
                vendor_id: "1002".to_string(),
                device_id: "ab40".to_string(),
                class: "0x040300".to_string(),
                current_driver: None,
            });
        }
        let config = windows_dual_gpu_config_amd_passthrough();
        let view = vm_view(&profile, &config).expect("view");

        let xml = render(&view).expect("render");
        assert!(xml.contains("<!-- GPU Audio companion -->"));
        assert!(xml.contains("<address domain='0x0000' bus='0x01' slot='0x00' function='0x1'/>"));
    }

    /// `rom_accessible = true` adds a `<rom file=...>` element pointing
    /// at the local vbios cache. The path is keyed on the PCI slot to
    /// avoid collisions when more than one GPU is passed through.
    #[test]
    fn gpu_hostdev_renderer_emits_rom_file_when_accessible() {
        let mut profile = amd_host_with_amd_passthrough();
        if let Some(gpu) = profile
            .gpus
            .iter_mut()
            .find(|g| g.pci_slot == "0000:01:00.0")
        {
            gpu.rom_accessible = true;
        }
        let config = windows_dual_gpu_config_amd_passthrough();
        let view = vm_view(&profile, &config).expect("view");

        let xml = render(&view).expect("render");
        assert!(xml.contains("<rom file='/var/lib/libvirt/vbios/0000_01_00_0.rom'/>"));
    }
}
