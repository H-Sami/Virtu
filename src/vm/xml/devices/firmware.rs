use crate::detect::SystemProfile;
use crate::kb::KnowledgeBase;
use crate::vm::profile::VmProfile;
use crate::vm::xml::XmlError;
use std::fmt::Write as FmtWrite;

pub fn render(
    profile: &VmProfile,
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

    if profile.enable_secure_boot {
        writeln!(xml, "    <smmbios mode='host'/>")?;
    }

    writeln!(xml, "    <bootmenu enable='yes' timeout='3000'/>")?;

    if profile.iso_path.is_some() {
        writeln!(xml, "    <boot dev='cdrom'/>")?;
    }
    writeln!(xml, "    <boot dev='hd'/>")?;

    writeln!(xml, "  </os>")?;
    Ok(xml)
}
