use crate::detect::gpu::GpuVendor;
use crate::detect::SystemProfile;
use crate::vm::cpu_topology::calculate_pinning;
use crate::vm::profile::VmView;
use crate::vm::xml::XmlError;
use std::fmt::Write as FmtWrite;

/// Render the resource-allocation chunk that libvirt's domain schema
/// expects **before** `<os>`: `<vcpu>`, `<iothreads>`, and the
/// `<cputune>` pinning block. Splitting this from the post-`<os>`
/// CPU/clock chunk is required because libvirt's Relax-NG groups
/// these elements with `<memory>`/`<currentMemory>` in a pre-OS
/// interleave; emitting them after `<features>` makes
/// `virt-xml-validate` reject the document with "Extra element
/// features in interleave".
pub fn render_resources(view: &VmView<'_>, system: &SystemProfile) -> Result<String, XmlError> {
    let mut xml = String::new();

    writeln!(xml, "  <vcpu placement='static'>{}</vcpu>", view.vcpu_count)?;

    if view.use_cpu_pinning && view.use_iothreads {
        writeln!(xml, "  <iothreads>1</iothreads>")?;
    }

    write_cpu_tune(&mut xml, view, system)?;

    Ok(xml)
}

/// Render the post-`<os>` chunk: `<cpu>` topology and `<clock>`.
/// libvirt's schema places these after `<features>`, so the builder
/// invokes this helper after `features::render`.
pub fn render_processor(view: &VmView<'_>, system: &SystemProfile) -> Result<String, XmlError> {
    let mut xml = String::new();

    write_cpu(&mut xml, view, system)?;
    write_clock(&mut xml, view)?;

    Ok(xml)
}

fn write_cpu(xml: &mut String, view: &VmView<'_>, system: &SystemProfile) -> Result<(), XmlError> {
    writeln!(
        xml,
        "  <cpu mode='host-passthrough' check='none' migratable='off'>"
    )?;

    let threads_per_core = if system.cpu.has_hyperthreading { 2 } else { 1 };
    let vcpu_count = view.vcpu_count;
    let sockets = 1u32;
    let cores = (vcpu_count / threads_per_core).max(1);
    let threads = threads_per_core;

    writeln!(
        xml,
        "    <topology sockets='{sockets}' dies='1' cores='{cores}' threads='{threads}'/>"
    )?;
    writeln!(xml, "    <cache mode='passthrough'/>")?;

    if system.cpu.vendor.contains("AMD") {
        writeln!(xml, "    <feature policy='require' name='topoext'/>")?;
    }

    if view.passthrough_gpu.vendor == GpuVendor::Nvidia && view.enable_hyperv {
        writeln!(xml, "    <vendor_id state='on' value='AuthenticAMD'/>")?;
    }

    writeln!(xml, "  </cpu>")?;

    Ok(())
}

fn write_clock(xml: &mut String, view: &VmView<'_>) -> Result<(), XmlError> {
    let offset = if view.guest_os.benefits_from_hyperv() {
        "localtime"
    } else {
        "utc"
    };

    writeln!(xml, "  <clock offset='{offset}'>")?;
    writeln!(xml, "    <timer name='rtc' tickpolicy='catchup'/>")?;
    writeln!(xml, "    <timer name='pit' tickpolicy='delay'/>")?;
    writeln!(xml, "    <timer name='hpet' present='no'/>")?;

    if view.enable_hyperv {
        writeln!(xml, "    <timer name='hypervclock' present='yes'/>")?;
    }

    writeln!(xml, "  </clock>")?;
    Ok(())
}

fn write_cpu_tune(
    xml: &mut String,
    view: &VmView<'_>,
    system: &SystemProfile,
) -> Result<(), XmlError> {
    if !view.use_cpu_pinning {
        return Ok(());
    }

    let pinning = calculate_pinning(&system.cpu, view.vcpu_count);

    writeln!(xml, "  <cputune>")?;

    for (vcpu, cpuset) in &pinning.vcpu_pins {
        writeln!(xml, "    <vcpupin vcpu='{vcpu}' cpuset='{cpuset}'/>")?;
    }

    writeln!(
        xml,
        "    <emulatorpin cpuset='{}'/>",
        pinning.emulator_cpuset
    )?;

    if view.use_iothreads {
        writeln!(
            xml,
            "    <iothreadpin iothread='1' cpuset='{}'/>",
            pinning.emulator_cpuset
        )?;
    }

    writeln!(xml, "  </cputune>")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{render_processor, render_resources};
    use crate::vm::profile::vm_view;
    use crate::vm::xml::devices::fixtures::{
        amd_host_with_amd_passthrough, nvidia_passthrough_profile,
        windows_dual_gpu_config_amd_passthrough, windows_dual_gpu_config_nvidia_passthrough,
    };

    /// Default Windows-on-AMD-passthrough emits host-passthrough mode,
    /// 1 socket / 2 cores / 2 threads (4 vCPUs split across HT pairs),
    /// `topoext` (AMD requires it), the Hyper-V clock timer, and a
    /// utc clock offset because the guest is Windows-11.
    ///
    /// The renderer is split in two: `render_resources` produces the
    /// pre-`<os>` chunk (`<vcpu>`, `<iothreads>`, `<cputune>`),
    /// `render_processor` produces the post-`<os>` chunk (`<cpu>`,
    /// `<clock>`). We assert on the union here because that is what
    /// the orchestrator emits into the final document.
    #[test]
    fn cpu_renderer_amd_default_emits_pinning_and_topoext_and_hyperv_clock() {
        let profile = amd_host_with_amd_passthrough();
        let config = windows_dual_gpu_config_amd_passthrough();
        let view = vm_view(&profile, &config).expect("view");

        let resources = render_resources(&view, &profile).expect("render resources");
        let processor = render_processor(&view, &profile).expect("render processor");
        let xml = format!("{resources}{processor}");

        assert!(xml.contains("<cpu mode='host-passthrough' check='none' migratable='off'>"));
        assert!(xml.contains("<topology sockets='1' dies='1' cores='2' threads='2'/>"));
        assert!(xml.contains("<cache mode='passthrough'/>"));
        assert!(xml.contains("<feature policy='require' name='topoext'/>"));
        assert!(xml.contains("<vcpu placement='static'>4</vcpu>"));

        // Windows benefits from Hyper-V → localtime + hypervclock timer.
        assert!(xml.contains("<clock offset='localtime'>"));
        assert!(xml.contains("<timer name='hpet' present='no'/>"));
        assert!(xml.contains("<timer name='hypervclock' present='yes'/>"));

        // Pinning + emulator pin + iothread pin are present.
        assert!(xml.contains("<cputune>"));
        assert!(xml.contains("<vcpupin"));
        assert!(xml.contains("<emulatorpin"));
        assert!(xml.contains("<iothreadpin iothread='1'"));
        assert!(xml.contains("<iothreads>1</iothreads>"));
    }

    /// NVIDIA passthrough with Hyper-V enabled: the Hyper-V vendor-id
    /// spoof block ships so the NVIDIA driver does not detect the
    /// hypervisor and refuse to load (Code 43). This is the GeForce
    /// driver's well-known anti-virtualization check.
    #[test]
    fn cpu_renderer_nvidia_passthrough_includes_hyperv_vendor_id_spoof() {
        let profile = nvidia_passthrough_profile();
        let config = windows_dual_gpu_config_nvidia_passthrough();
        let view = vm_view(&profile, &config).expect("view");

        let xml = render_processor(&view, &profile).expect("render processor");
        assert!(xml.contains("<vendor_id state='on' value='AuthenticAMD'/>"));
    }

    /// `use_cpu_pinning = false` removes the entire `<cputune>` and
    /// `<iothreads>` blocks from the resources chunk. We pin both so
    /// a future change cannot silently land vCPU pinning when the
    /// user opted out.
    #[test]
    fn cpu_renderer_omits_cputune_when_pinning_disabled() {
        let profile = amd_host_with_amd_passthrough();
        let config = windows_dual_gpu_config_amd_passthrough();
        let mut view = vm_view(&profile, &config).expect("view");
        view.use_cpu_pinning = false;

        let xml = render_resources(&view, &profile).expect("render resources");
        assert!(!xml.contains("<cputune>"));
        assert!(!xml.contains("<iothreads>"));
        assert!(!xml.contains("<vcpupin"));
        // <vcpu> still ships even without pinning; pinning is the
        // optional layer.
        assert!(xml.contains("<vcpu placement='static'>"));
    }

    /// `render_resources` must NOT emit `<cpu>` or `<clock>`: those
    /// belong post-`<os>`. Pinning this prevents a future regression
    /// that re-merges the two halves and breaks the schema interleave.
    #[test]
    fn render_resources_does_not_emit_post_os_elements() {
        let profile = amd_host_with_amd_passthrough();
        let config = windows_dual_gpu_config_amd_passthrough();
        let view = vm_view(&profile, &config).expect("view");

        let xml = render_resources(&view, &profile).expect("render resources");
        assert!(!xml.contains("<cpu mode="));
        assert!(!xml.contains("<clock"));
    }

    /// `render_processor` must NOT emit `<vcpu>`, `<iothreads>`, or
    /// `<cputune>`: those belong pre-`<os>`. Pinning prevents the
    /// dual emission that would corrupt the document.
    #[test]
    fn render_processor_does_not_emit_pre_os_elements() {
        let profile = amd_host_with_amd_passthrough();
        let config = windows_dual_gpu_config_amd_passthrough();
        let view = vm_view(&profile, &config).expect("view");

        let xml = render_processor(&view, &profile).expect("render processor");
        assert!(!xml.contains("<vcpu "));
        assert!(!xml.contains("<iothreads>"));
        assert!(!xml.contains("<cputune>"));
    }
}
