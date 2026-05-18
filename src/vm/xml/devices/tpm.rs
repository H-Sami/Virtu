use crate::vm::profile::VmProfile;
use crate::vm::xml::XmlError;
use std::fmt::Write as FmtWrite;

pub fn render(profile: &VmProfile) -> Result<String, XmlError> {
    let mut xml = String::new();

    if profile.enable_tpm {
        writeln!(xml, "    <tpm model='tpm-crb'>")?;
        writeln!(xml, "      <backend type='emulator' version='2.0'/>")?;
        writeln!(xml, "    </tpm>")?;
    }

    Ok(xml)
}
