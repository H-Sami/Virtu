use crate::detect::gpu::GpuVendor;
use crate::vm::profile::VmView;
use crate::vm::xml::XmlError;
use std::fmt::Write as FmtWrite;

pub fn render(view: &VmView<'_>) -> Result<String, XmlError> {
    let mut xml = String::new();

    writeln!(xml, "  <features>")?;
    writeln!(xml, "    <acpi/>")?;
    writeln!(xml, "    <apic/>")?;

    // OVMF Secure Boot relies on SMM to keep the variable store
    // tamper-resistant. Emitting `<smm state='on'/>` is the
    // schema-correct way to express that; the OS firmware block
    // (in firmware.rs) only writes `<loader>`/`<nvram>`/etc., and
    // the misnamed `<smmbios>` element does not exist in the
    // libvirt domain schema.
    if view.enable_secure_boot {
        writeln!(xml, "    <smm state='on'/>")?;
    }

    if view.enable_hyperv {
        writeln!(xml, "    <hyperv mode='custom'>")?;
        writeln!(xml, "      <relaxed state='on'/>")?;
        writeln!(xml, "      <vapic state='on'/>")?;
        writeln!(xml, "      <spinlocks state='on' retries='8191'/>")?;
        writeln!(xml, "      <vpindex state='on'/>")?;
        writeln!(xml, "      <synic state='on'/>")?;
        // libvirt's domain schema models `direct` as a child element
        // of `<stimer>`, not an attribute. Earlier revisions emitted
        // `<stimer state='on' direct='on'/>`, which `virt-xml-validate`
        // rejected with "Extra element features in interleave".
        writeln!(xml, "      <stimer state='on'>")?;
        writeln!(xml, "        <direct state='on'/>")?;
        writeln!(xml, "      </stimer>")?;
        writeln!(xml, "      <reset state='on'/>")?;
        writeln!(xml, "      <frequencies state='on'/>")?;
        writeln!(xml, "      <reenlightenment state='on'/>")?;
        writeln!(xml, "      <tlbflush state='on'/>")?;
        writeln!(xml, "      <ipi state='on'/>")?;
        writeln!(xml, "    </hyperv>")?;
        writeln!(xml, "    <ioapic driver='kvm'/>")?;
    }

    if view.passthrough_gpu.vendor == GpuVendor::Nvidia {
        writeln!(xml, "    <kvm>")?;
        writeln!(xml, "      <hidden state='on'/>")?;
        writeln!(xml, "    </kvm>")?;
    }

    writeln!(xml, "  </features>")?;
    Ok(xml)
}

#[cfg(test)]
mod tests {
    use super::render;
    use crate::vm::profile::vm_view;
    use crate::vm::xml::devices::fixtures::{
        amd_host_with_amd_passthrough, nvidia_passthrough_profile,
        windows_dual_gpu_config_amd_passthrough, windows_dual_gpu_config_nvidia_passthrough,
    };

    /// Default Windows-on-AMD: ACPI + APIC + Hyper-V enlightenments,
    /// no `<kvm><hidden state='on'/></kvm>` block (AMD does not need
    /// the NVIDIA Code-43 workaround).
    #[test]
    fn features_renderer_amd_emits_full_hyperv_block_no_kvm_hidden() {
        let profile = amd_host_with_amd_passthrough();
        let config = windows_dual_gpu_config_amd_passthrough();
        let view = vm_view(&profile, &config).expect("view");

        let xml = render(&view).expect("render");
        assert!(xml.starts_with("  <features>\n"));
        assert!(xml.contains("<acpi/>"));
        assert!(xml.contains("<apic/>"));
        assert!(xml.contains("<hyperv mode='custom'>"));
        assert!(xml.contains("<vapic state='on'/>"));
        assert!(xml.contains("<spinlocks state='on' retries='8191'/>"));
        // `<stimer>` ships with `<direct>` as a child element, not
        // a `direct='on'` attribute. The latter is rejected by
        // libvirt's Relax-NG schema; this assertion locks in the
        // correct shape.
        assert!(xml.contains("<stimer state='on'>"));
        assert!(xml.contains("<direct state='on'/>"));
        assert!(!xml.contains("<stimer state='on' direct='on'/>"));
        assert!(xml.contains("<ipi state='on'/>"));
        assert!(xml.contains("<ioapic driver='kvm'/>"));
        assert!(!xml.contains("<kvm>"));
        assert!(xml.trim_end().ends_with("</features>"));
    }

    /// NVIDIA passthrough requires the `<kvm><hidden state='on'/></kvm>`
    /// block so the GeForce driver does not detect the hypervisor and
    /// refuse to load with Code 43.
    #[test]
    fn features_renderer_nvidia_emits_kvm_hidden_block() {
        let profile = nvidia_passthrough_profile();
        let config = windows_dual_gpu_config_nvidia_passthrough();
        let view = vm_view(&profile, &config).expect("view");

        let xml = render(&view).expect("render");
        assert!(xml.contains("<kvm>"));
        assert!(xml.contains("<hidden state='on'/>"));
    }

    /// When Hyper-V is disabled (e.g. Linux guests), the entire
    /// `<hyperv>` block disappears. We pin its absence so a future
    /// renderer change cannot silently re-enable it.
    #[test]
    fn features_renderer_omits_hyperv_block_for_non_hyperv_guests() {
        let profile = amd_host_with_amd_passthrough();
        let config = windows_dual_gpu_config_amd_passthrough();
        let mut view = vm_view(&profile, &config).expect("view");
        view.enable_hyperv = false;

        let xml = render(&view).expect("render");
        assert!(!xml.contains("<hyperv"));
        assert!(!xml.contains("<ioapic driver='kvm'/>"));
    }

    /// OVMF Secure Boot needs SMM enabled in `<features>` so the
    /// firmware variable store is tamper-resistant. The element is
    /// `<smm state='on'/>` — older revisions of this code emitted
    /// `<smmbios mode='host'/>` from the firmware renderer, which is
    /// not a real libvirt schema element and gets rejected by
    /// `virt-xml-validate`. Pinning the schema-correct shape here
    /// ensures the default Windows-11 plan (which sets
    /// `enable_secure_boot=true`) produces XML libvirt accepts.
    #[test]
    fn features_renderer_emits_smm_when_secure_boot_enabled() {
        let profile = amd_host_with_amd_passthrough();
        let config = windows_dual_gpu_config_amd_passthrough();
        let mut view = vm_view(&profile, &config).expect("view");
        view.enable_secure_boot = true;

        let xml = render(&view).expect("render");
        assert!(xml.contains("<smm state='on'/>"));
        // Defense-in-depth: the misnamed legacy element must never
        // come back.
        assert!(!xml.contains("smmbios"));
    }

    /// Without Secure Boot, the `<smm>` block must be absent. This
    /// pins the off-state so a future change cannot silently force
    /// SMM on for every guest (some workloads explicitly disable it
    /// to free a vCPU mode).
    #[test]
    fn features_renderer_omits_smm_when_secure_boot_disabled() {
        let profile = amd_host_with_amd_passthrough();
        let config = windows_dual_gpu_config_amd_passthrough();
        let mut view = vm_view(&profile, &config).expect("view");
        view.enable_secure_boot = false;

        let xml = render(&view).expect("render");
        assert!(!xml.contains("<smm"));
    }
}
