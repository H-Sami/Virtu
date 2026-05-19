use crate::vm::profile::VmView;
use crate::vm::xml::XmlError;
use std::fmt::Write as FmtWrite;

pub fn render(view: &VmView<'_>) -> Result<String, XmlError> {
    let mut xml = String::new();
    let ram_kb = view.ram_mb * 1024;

    writeln!(xml, "  <memory unit='KiB'>{ram_kb}</memory>")?;
    writeln!(xml, "  <currentMemory unit='KiB'>{ram_kb}</currentMemory>")?;

    if view.use_hugepages {
        writeln!(xml, "  <memoryBacking>")?;
        writeln!(xml, "    <hugepages/>")?;
        writeln!(xml, "    <nosharepages/>")?;
        writeln!(xml, "    <locked/>")?;
        writeln!(xml, "    <source type='memfd'/>")?;
        writeln!(xml, "    <access mode='shared'/>")?;
        writeln!(xml, "  </memoryBacking>")?;
    }

    Ok(xml)
}

#[cfg(test)]
mod tests {
    use super::render;
    use crate::vm::profile::vm_view;
    use crate::vm::xml::devices::fixtures::{
        amd_host_with_amd_passthrough, windows_dual_gpu_config_amd_passthrough,
    };

    /// Memory renderer is byte-exact for the standard 8 GiB Windows
    /// build. A change here is loud: any reformat or block-reorder
    /// requires updating this golden.
    #[test]
    fn memory_renderer_emits_exact_xml_for_default_config() {
        let profile = amd_host_with_amd_passthrough();
        let config = windows_dual_gpu_config_amd_passthrough();
        let view = vm_view(&profile, &config).expect("view");

        let xml = render(&view).expect("render");
        let expected = "  <memory unit='KiB'>8388608</memory>\n  <currentMemory unit='KiB'>8388608</currentMemory>\n";
        assert_eq!(xml, expected);
    }

    /// When the view enables hugepages, the renderer emits the
    /// `<memoryBacking>` block in the documented order. This is the
    /// only opt-in branch in the file; pinning it prevents a
    /// future renderer change from silently disabling huge pages.
    #[test]
    fn memory_renderer_emits_hugepages_block_when_enabled() {
        let profile = amd_host_with_amd_passthrough();
        let config = windows_dual_gpu_config_amd_passthrough();
        let mut view = vm_view(&profile, &config).expect("view");
        view.use_hugepages = true;

        let xml = render(&view).expect("render");
        let expected = "  <memory unit='KiB'>8388608</memory>\n  \
                        <currentMemory unit='KiB'>8388608</currentMemory>\n  \
                        <memoryBacking>\n    \
                        <hugepages/>\n    \
                        <nosharepages/>\n    \
                        <locked/>\n    \
                        <source type='memfd'/>\n    \
                        <access mode='shared'/>\n  \
                        </memoryBacking>\n";
        assert_eq!(xml, expected);
    }
}
