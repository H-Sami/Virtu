use crate::detect::SystemProfile;
use crate::kb::KnowledgeBase;
use crate::vm::profile::VmView;
use crate::vm::xml::XmlError;
use std::fmt::Write as FmtWrite;

pub fn render(
    view: &VmView<'_>,
    system: &SystemProfile,
    kb: &KnowledgeBase,
) -> Result<String, XmlError> {
    let mut xml = String::new();

    writeln!(xml, "  <os firmware='efi'>")?;
    writeln!(xml, "    <type arch='x86_64' machine='q35'>hvm</type>")?;

    let ovmf_paths = kb.paths_for_distro(&system.distro);
    let ovmf_code = ovmf_paths
        .ovmf_code
        .as_deref()
        .unwrap_or("/usr/share/OVMF/OVMF_CODE.fd");
    let ovmf_vars = ovmf_paths
        .ovmf_vars
        .as_deref()
        .unwrap_or("/usr/share/OVMF/OVMF_VARS.fd");

    writeln!(
        xml,
        "    <loader readonly='yes' type='pflash'>{ovmf_code}</loader>"
    )?;
    writeln!(xml, "    <nvram>{ovmf_vars}</nvram>")?;

    if view.enable_secure_boot {
        writeln!(xml, "    <smmbios mode='host'/>")?;
    }

    writeln!(xml, "    <bootmenu enable='yes' timeout='3000'/>")?;

    if view.iso_path.is_some() {
        writeln!(xml, "    <boot dev='cdrom'/>")?;
    }
    writeln!(xml, "    <boot dev='hd'/>")?;

    writeln!(xml, "  </os>")?;
    Ok(xml)
}

#[cfg(test)]
mod tests {
    use super::render;
    use crate::kb::KnowledgeBase;
    use crate::vm::profile::vm_view;
    use crate::vm::xml::devices::fixtures::{
        amd_host_with_amd_passthrough, windows_dual_gpu_config_amd_passthrough,
    };
    use std::path::PathBuf;

    /// q35 + EFI + OVMF paths from the bundled KB. With no ISO and a
    /// hard-disk-only Windows-11 install, only `<boot dev='hd'/>` is
    /// emitted (the bootmenu is enabled).
    #[test]
    fn firmware_renderer_emits_q35_efi_ovmf_block_with_hd_only_boot() {
        let profile = amd_host_with_amd_passthrough();
        let config = windows_dual_gpu_config_amd_passthrough();
        let view = vm_view(&profile, &config).expect("view");
        let kb = KnowledgeBase::bundled();

        let xml = render(&view, &profile, &kb).expect("render");
        assert!(xml.starts_with("  <os firmware='efi'>\n"));
        assert!(xml.contains("<type arch='x86_64' machine='q35'>hvm</type>"));
        assert!(xml.contains("<loader readonly='yes' type='pflash'>"));
        assert!(xml.contains("<nvram>"));
        assert!(xml.contains("<bootmenu enable='yes' timeout='3000'/>"));
        assert!(xml.contains("<boot dev='hd'/>"));
        assert!(!xml.contains("<boot dev='cdrom'/>"));
        assert!(xml.trim_end().ends_with("</os>"));
    }

    /// When an ISO is attached, `<boot dev='cdrom'/>` precedes the hard
    /// disk so the guest installs from the ISO on first boot.
    #[test]
    fn firmware_renderer_includes_cdrom_boot_entry_when_iso_present() {
        let profile = amd_host_with_amd_passthrough();
        let mut config = windows_dual_gpu_config_amd_passthrough();
        config.iso_path = Some(PathBuf::from("/isos/windows.iso"));
        let view = vm_view(&profile, &config).expect("view");
        let kb = KnowledgeBase::bundled();

        let xml = render(&view, &profile, &kb).expect("render");
        let cdrom = xml
            .find("<boot dev='cdrom'/>")
            .expect("cdrom boot entry must be present");
        let hd = xml
            .find("<boot dev='hd'/>")
            .expect("hd boot entry must be present");
        assert!(cdrom < hd, "cdrom must come before hd so the install boots");
    }
}
