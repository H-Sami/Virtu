use crate::vm::profile::VmView;
use crate::vm::xml::XmlError;
use std::fmt::Write as FmtWrite;

pub fn render(view: &VmView<'_>) -> Result<String, XmlError> {
    let mut xml = String::new();

    write_primary_disk(&mut xml, view)?;
    write_iso_disk(&mut xml, view)?;
    write_scsi_controller(&mut xml, view)?;

    Ok(xml)
}

fn write_primary_disk(xml: &mut String, view: &VmView<'_>) -> Result<(), XmlError> {
    let disk_format = view.disk.format.extension();
    let iothread_attr = if view.use_iothreads {
        " iothread='1'"
    } else {
        ""
    };

    writeln!(xml, "    <disk type='file' device='disk'>")?;
    writeln!(
        xml,
        "      <driver name='qemu' type='{disk_format}' cache='none' io='native' discard='unmap'{iothread_attr}/>"
    )?;
    writeln!(xml, "      <source file='{}'/>", view.disk.path.display())?;
    writeln!(xml, "      <target dev='sda' bus='scsi'/>")?;
    writeln!(xml, "      <boot order='1'/>")?;
    writeln!(xml, "    </disk>")?;

    Ok(())
}

fn write_iso_disk(xml: &mut String, view: &VmView<'_>) -> Result<(), XmlError> {
    if let Some(iso) = view.iso_path {
        writeln!(xml, "    <disk type='file' device='cdrom'>")?;
        writeln!(xml, "      <driver name='qemu' type='raw'/>")?;
        writeln!(xml, "      <source file='{}'/>", iso.display())?;
        writeln!(xml, "      <target dev='sdb' bus='scsi'/>")?;
        writeln!(xml, "      <readonly/>")?;
        writeln!(xml, "    </disk>")?;
    }

    Ok(())
}

fn write_scsi_controller(xml: &mut String, view: &VmView<'_>) -> Result<(), XmlError> {
    writeln!(
        xml,
        "    <controller type='scsi' index='0' model='virtio-scsi'>"
    )?;
    if view.use_iothreads {
        writeln!(xml, "      <driver iothread='1'/>")?;
    }
    writeln!(xml, "    </controller>")?;

    Ok(())
}
