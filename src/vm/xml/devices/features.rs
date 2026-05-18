use crate::detect::gpu::GpuVendor;
use crate::vm::profile::VmProfile;
use crate::vm::xml::XmlError;
use std::fmt::Write as FmtWrite;

pub fn render(profile: &VmProfile) -> Result<String, XmlError> {
    let mut xml = String::new();

    writeln!(xml, "  <features>")?;
    writeln!(xml, "    <acpi/>")?;
    writeln!(xml, "    <apic/>")?;

    if profile.enable_hyperv {
        writeln!(xml, "    <hyperv mode='custom'>")?;
        writeln!(xml, "      <relaxed state='on'/>")?;
        writeln!(xml, "      <vapic state='on'/>")?;
        writeln!(xml, "      <spinlocks state='on' retries='8191'/>")?;
        writeln!(xml, "      <vpindex state='on'/>")?;
        writeln!(xml, "      <synic state='on'/>")?;
        writeln!(xml, "      <stimer state='on' direct='on'/>")?;
        writeln!(xml, "      <reset state='on'/>")?;
        writeln!(xml, "      <frequencies state='on'/>")?;
        writeln!(xml, "      <reenlightenment state='on'/>")?;
        writeln!(xml, "      <tlbflush state='on'/>")?;
        writeln!(xml, "      <ipi state='on'/>")?;
        writeln!(xml, "    </hyperv>")?;
        writeln!(xml, "    <ioapic driver='kvm'/>")?;
    }

    if profile.passthrough_gpu.vendor == GpuVendor::Nvidia {
        writeln!(xml, "    <kvm>")?;
        writeln!(xml, "      <hidden state='on'/>")?;
        writeln!(xml, "    </kvm>")?;
    }

    writeln!(xml, "  </features>")?;
    Ok(xml)
}
