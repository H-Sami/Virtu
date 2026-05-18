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
