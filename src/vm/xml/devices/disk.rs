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

#[cfg(test)]
mod tests {
    use super::render;
    use crate::vm::profile::vm_view;
    use crate::vm::xml::devices::fixtures::{
        amd_host_with_amd_passthrough, windows_dual_gpu_config_amd_passthrough,
    };
    use std::path::PathBuf;

    /// Default Windows config: a brand-new 100 GiB qcow2 image, no ISO.
    /// Pinning the byte sequence catches accidental driver/cache/io
    /// flag changes that would silently ship a slower or unsafe disk.
    #[test]
    fn disk_renderer_emits_exact_xml_for_create_qcow2_no_iso() {
        let profile = amd_host_with_amd_passthrough();
        let config = windows_dual_gpu_config_amd_passthrough();
        let view = vm_view(&profile, &config).expect("view");

        let xml = render(&view).expect("render");
        let expected = "    <disk type='file' device='disk'>\n      \
                        <driver name='qemu' type='qcow2' cache='none' io='native' discard='unmap' iothread='1'/>\n      \
                        <source file='/var/lib/libvirt/images/virtu-windows.qcow2'/>\n      \
                        <target dev='sda' bus='scsi'/>\n      \
                        <boot order='1'/>\n    \
                        </disk>\n    \
                        <controller type='scsi' index='0' model='virtio-scsi'>\n      \
                        <driver iothread='1'/>\n    \
                        </controller>\n";
        assert_eq!(xml, expected);
    }

    /// With an ISO attached, the renderer adds a second `<disk>` of
    /// device='cdrom' with `<readonly/>` and uses target dev='sdb' so
    /// it does not collide with the primary disk's 'sda'.
    #[test]
    fn disk_renderer_includes_cdrom_block_when_iso_path_is_set() {
        let profile = amd_host_with_amd_passthrough();
        let mut config = windows_dual_gpu_config_amd_passthrough();
        config.iso_path = Some(PathBuf::from("/isos/windows.iso"));
        let view = vm_view(&profile, &config).expect("view");

        let xml = render(&view).expect("render");
        assert!(xml.contains("<disk type='file' device='cdrom'>"));
        assert!(xml.contains("<source file='/isos/windows.iso'/>"));
        assert!(xml.contains("<target dev='sdb' bus='scsi'/>"));
        assert!(xml.contains("<readonly/>"));
    }

    /// Without iothreads, the driver line drops the `iothread='1'`
    /// attribute and the SCSI controller drops the `<driver iothread>`
    /// child. We pin both so a future change to disable iothreads is
    /// reflected accurately.
    #[test]
    fn disk_renderer_omits_iothread_attrs_when_iothreads_disabled() {
        let profile = amd_host_with_amd_passthrough();
        let config = windows_dual_gpu_config_amd_passthrough();
        let mut view = vm_view(&profile, &config).expect("view");
        view.use_iothreads = false;

        let xml = render(&view).expect("render");
        assert!(!xml.contains("iothread='1'"));
        assert!(!xml.contains("<driver iothread='1'/>"));
    }
}
