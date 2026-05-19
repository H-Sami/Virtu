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

#[cfg(test)]
mod tests {
    use super::render;
    use crate::vm::profile::vm_view;
    use crate::vm::xml::devices::fixtures::{
        amd_host_with_amd_passthrough, windows_dual_gpu_config_amd_passthrough,
    };
    use crate::vm::GuestOs;

    /// Windows 11 requires TPM 2.0 to install. The default config
    /// (Windows 11) must therefore emit a `<tpm>` block with the CRB
    /// model and emulator backend.
    #[test]
    fn tpm_renderer_emits_tpm_2_0_for_windows_11() {
        let profile = amd_host_with_amd_passthrough();
        let config = windows_dual_gpu_config_amd_passthrough();
        let view = vm_view(&profile, &config).expect("view");

        let xml = render(&view).expect("render");
        let expected = "    <tpm model='tpm-crb'>\n      \
                        <backend type='emulator' version='2.0'/>\n    \
                        </tpm>\n";
        assert_eq!(xml, expected);
    }

    /// Older guests that do not need TPM (Linux, Windows 10) get no
    /// `<tpm>` block. The fragment is empty so the libvirt domain is
    /// not loaded with a stale TPM device users did not ask for.
    #[test]
    fn tpm_renderer_emits_nothing_for_windows_10() {
        let profile = amd_host_with_amd_passthrough();
        let mut config = windows_dual_gpu_config_amd_passthrough();
        config.guest_os = GuestOs::Windows10;
        let view = vm_view(&profile, &config).expect("view");

        let xml = render(&view).expect("render");
        assert!(xml.is_empty());
    }
}
