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
