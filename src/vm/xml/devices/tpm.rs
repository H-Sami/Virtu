use crate::vm::profile::VmView;
use crate::vm::xml::XmlError;
use std::fmt::Write as FmtWrite;

pub fn render(view: &VmView<'_>) -> Result<String, XmlError> {
    let mut xml = String::new();

    if view.enable_tpm {
        writeln!(xml, "    <tpm model='tpm-crb'>")?;
        writeln!(xml, "      <backend type='emulator' version='2.0'/>")?;
        writeln!(xml, "    </tpm>")?;
    }

    Ok(xml)
}
