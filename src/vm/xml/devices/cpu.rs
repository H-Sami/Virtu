use crate::detect::gpu::GpuVendor;
use crate::detect::SystemProfile;
use crate::vm::cpu_topology::calculate_pinning;
use crate::vm::profile::VmProfile;
use crate::vm::xml::XmlError;
use std::fmt::Write as FmtWrite;

pub fn render(profile: &VmProfile, system: &SystemProfile) -> Result<String, XmlError> {
    let mut xml = String::new();

    write_cpu(&mut xml, profile, system)?;
    write_clock(&mut xml, profile)?;
    write_cpu_tune(&mut xml, profile, system)?;

    Ok(xml)
}

fn write_cpu(
    xml: &mut String,
    profile: &VmProfile,
    system: &SystemProfile,
) -> Result<(), XmlError> {
    writeln!(
        xml,
        "  <cpu mode='host-passthrough' check='none' migratable='off'>"
    )?;

    let threads_per_core = if system.cpu.has_hyperthreading { 2 } else { 1 };
    let vcpu_count = profile.vcpu_count;
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

    if profile.passthrough_gpu.vendor == GpuVendor::Nvidia && profile.enable_hyperv {
        writeln!(xml, "    <vendor_id state='on' value='AuthenticAMD'/>")?;
    }

    writeln!(xml, "  </cpu>")?;
    writeln!(xml, "  <vcpu placement='static'>{vcpu_count}</vcpu>")?;

    Ok(())
}

fn write_clock(xml: &mut String, profile: &VmProfile) -> Result<(), XmlError> {
    let offset = if profile.guest_os.benefits_from_hyperv() {
        "localtime"
    } else {
        "utc"
    };

    writeln!(xml, "  <clock offset='{offset}'>")?;
    writeln!(xml, "    <timer name='rtc' tickpolicy='catchup'/>")?;
    writeln!(xml, "    <timer name='pit' tickpolicy='delay'/>")?;
    writeln!(xml, "    <timer name='hpet' present='no'/>")?;

    if profile.enable_hyperv {
        writeln!(xml, "    <timer name='hypervclock' present='yes'/>")?;
    }

    writeln!(xml, "  </clock>")?;
    Ok(())
}

fn write_cpu_tune(
    xml: &mut String,
    profile: &VmProfile,
    system: &SystemProfile,
) -> Result<(), XmlError> {
    if !profile.use_cpu_pinning {
        return Ok(());
    }

    let pinning = calculate_pinning(&system.cpu, profile.vcpu_count);

    writeln!(xml, "  <cputune>")?;

    for (vcpu, cpuset) in &pinning.vcpu_pins {
        writeln!(xml, "    <vcpupin vcpu='{vcpu}' cpuset='{cpuset}'/>")?;
    }

    writeln!(
        xml,
        "    <emulatorpin cpuset='{}'/>",
        pinning.emulator_cpuset
    )?;

    if profile.use_iothreads {
        writeln!(
            xml,
            "    <iothreadpin iothread='1' cpuset='{}'/>",
            pinning.emulator_cpuset
        )?;
    }

    writeln!(xml, "  </cputune>")?;

    if profile.use_iothreads {
        writeln!(xml, "  <iothreads>1</iothreads>")?;
    }

    Ok(())
}
