use crate::vm::profile::VmProfile;
use crate::vm::xml::XmlError;
use std::fmt::Write as FmtWrite;

pub fn render(profile: &VmProfile) -> Result<String, XmlError> {
    let mut xml = String::new();

    write_primary_disk(&mut xml, profile)?;
    write_iso_disk(&mut xml, profile)?;
    write_scsi_controller(&mut xml, profile)?;

    Ok(xml)
}

fn write_primary_disk(xml: &mut String, profile: &VmProfile) -> Result<(), XmlError> {
    let disk_format = if profile
        .disk_path
        .extension()
        .map(|extension| extension == "qcow2")
        .unwrap_or(false)
    {
        "qcow2"
    } else {
        "raw"
    };
    let iothread_attr = if profile.use_iothreads {
        " iothread='1'"
    } else {
        ""
    };

    writeln!(xml, "    <disk type='file' device='disk'>")?;
    writeln!(
        xml,
        "      <driver name='qemu' type='{disk_format}' cache='none' io='native' discard='unmap'{iothread_attr}/>"
    )?;
    writeln!(
        xml,
        "      <source file='{}'/>",
        profile.disk_path.display()
    )?;
    writeln!(xml, "      <target dev='sda' bus='scsi'/>")?;
    writeln!(xml, "      <boot order='1'/>")?;
    writeln!(xml, "    </disk>")?;

    Ok(())
}

fn write_iso_disk(xml: &mut String, profile: &VmProfile) -> Result<(), XmlError> {
    if let Some(iso) = &profile.iso_path {
        writeln!(xml, "    <disk type='file' device='cdrom'>")?;
        writeln!(xml, "      <driver name='qemu' type='raw'/>")?;
        writeln!(xml, "      <source file='{}'/>", iso.display())?;
        writeln!(xml, "      <target dev='sdb' bus='scsi'/>")?;
        writeln!(xml, "      <readonly/>")?;
        writeln!(xml, "    </disk>")?;
    }

    Ok(())
}

fn write_scsi_controller(xml: &mut String, profile: &VmProfile) -> Result<(), XmlError> {
    writeln!(
        xml,
        "    <controller type='scsi' index='0' model='virtio-scsi'>"
    )?;
    if profile.use_iothreads {
        writeln!(xml, "      <driver iothread='1'/>")?;
    }
    writeln!(xml, "    </controller>")?;

    Ok(())
}
