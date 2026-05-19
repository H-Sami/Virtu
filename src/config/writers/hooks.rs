//! Pure bash generators for libvirt single-GPU passthrough hooks (slice 9.1).
//!
//! Single-GPU passthrough relies on libvirt's per-domain hooks at
//! `/etc/libvirt/hooks/qemu.d/<vm_name>/{prepare,release}/{begin,end}/`.
//! When the user starts the VM, libvirt calls the **prepare/begin**
//! hook. That script must:
//!
//! 1. Stop the running display manager so it stops driving the GPU.
//! 2. Unbind any host driver that owns the GPU (and its companion audio
//!    device) from sysfs.
//! 3. Load `vfio-pci` if it is not already loaded.
//! 4. Bind the GPU and companion audio to `vfio-pci` by `<vendor>:<device>` id.
//!
//! When the VM stops, libvirt calls **release/end**. That script must
//! reverse the sequence: rebind the original driver and start the
//! display manager again.
//!
//! Both scripts begin with `set -eu` plus an error-trap that prints
//! recovery instructions to stderr. Failure of any step in the begin
//! script aborts before the next step runs, so the user never ends up
//! with a half-released GPU. Failure of the end script tells the user
//! exactly what to do (TTY login plus `systemctl start <dm>`).
//!
//! This module is **pure**: no filesystem writes, no command spawns,
//! no host-state inspection. The caller passes in the planning
//! parameters and gets back the bash content. Slice 9.3 wires the
//! installer; slice 9.4 wires the `bash -n` syntax check.

use crate::detect::display_manager::DisplayManager;
use crate::detect::gpu::GpuVendor;
use std::fmt::Write as FmtWrite;

/// Errors raised by the hook script generators.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HookScriptError {
    #[error(
        "cannot generate single-GPU hooks: display manager is Unknown. \
         Detection did not find a service Virtu can stop and restart, \
         which means the begin script would never release the GPU."
    )]
    UnknownDisplayManager,
    #[error(
        "cannot generate single-GPU hooks: display manager is `None` (TTY only). \
         Single-GPU passthrough hooks require a managed display manager service."
    )]
    NoDisplayManager,
    #[error("script writer failed unexpectedly: {0}")]
    Format(#[from] std::fmt::Error),
    #[error("invalid PCI id `{0}`: expected vendor:device hex pair")]
    InvalidPciId(String),
}

/// Plain-data input for the hook generators. The struct is built by the
/// planner / executor; this module never derives values from a live
/// host.
#[derive(Debug, Clone, PartialEq)]
pub struct HookContext {
    /// `PassthroughConfig::vm_name`. Used in error messages and recovery
    /// hints; the hook directory layout itself is keyed off this name
    /// by the installer (slice 9.3).
    pub vm_name: String,
    /// Display manager to stop in the begin script and start in the end
    /// script. Must not be `Unknown` or `None`.
    pub display_manager: DisplayManager,
    /// GPU vendor; selects the modprobe sequence.
    pub gpu_vendor: GpuVendor,
    /// Sorted, deduplicated list of `<vendor>:<device>` PCI ids that
    /// must be bound to vfio-pci when the VM starts. Always includes
    /// the GPU; includes its companion audio function when the host
    /// has one.
    pub vfio_pci_ids: Vec<String>,
}

/// Generate the contents of `prepare/begin/<vm_name>`.
///
/// The script:
/// - sets a strict shell (`set -eu`),
/// - installs an ERR trap that tells the user how to recover via TTY,
/// - stops the display manager,
/// - unbinds the host GPU driver (vendor-specific),
/// - loads vfio-pci,
/// - binds each PCI id in `vfio_pci_ids` to vfio-pci by writing to
///   `/sys/bus/pci/drivers/vfio-pci/new_id`.
pub fn release_script(ctx: &HookContext) -> Result<String, HookScriptError> {
    let dm_service = display_manager_service(&ctx.display_manager)?;
    validate_pci_ids(&ctx.vfio_pci_ids)?;

    let mut script = String::new();
    write_header(
        &mut script,
        &ctx.vm_name,
        "prepare/begin",
        "release the GPU from the host",
    )?;
    write_recovery_trap(
        &mut script,
        dm_service,
        "If you are reading this, the GPU release failed. \
         Recover by switching to a TTY (Ctrl+Alt+F2), logging in, and running:",
    )?;

    writeln!(
        script,
        "echo 'Virtu: stopping display manager {dm_service}'"
    )?;
    writeln!(script, "systemctl stop {dm_service}")?;
    writeln!(script)?;

    writeln!(script, "echo 'Virtu: unbinding host GPU drivers'")?;
    write_unbind_drivers(&mut script, &ctx.gpu_vendor)?;
    writeln!(script)?;

    writeln!(script, "echo 'Virtu: loading vfio-pci'")?;
    writeln!(script, "modprobe vfio-pci")?;
    writeln!(script)?;

    writeln!(
        script,
        "echo 'Virtu: binding GPU + companion audio to vfio-pci'"
    )?;
    for id in &ctx.vfio_pci_ids {
        let (vendor, device) = pci_id_split(id)?;
        writeln!(
            script,
            "echo '{vendor} {device}' > /sys/bus/pci/drivers/vfio-pci/new_id"
        )?;
    }
    writeln!(script)?;

    writeln!(script, "echo 'Virtu: GPU ready for VM `{}`'", ctx.vm_name)?;
    Ok(script)
}

/// Generate the contents of `release/end/<vm_name>`.
///
/// The script reverses the begin sequence:
/// - removes each PCI id from vfio-pci's id table,
/// - reloads the original driver,
/// - starts the display manager.
///
/// We do not unload `vfio-pci` itself: another single-GPU plan on the
/// same host might still need it loaded.
pub fn reattach_script(ctx: &HookContext) -> Result<String, HookScriptError> {
    let dm_service = display_manager_service(&ctx.display_manager)?;
    validate_pci_ids(&ctx.vfio_pci_ids)?;

    let mut script = String::new();
    write_header(
        &mut script,
        &ctx.vm_name,
        "release/end",
        "re-attach the GPU to the host",
    )?;
    write_recovery_trap(
        &mut script,
        dm_service,
        "If you are reading this, the GPU re-attach failed. \
         Recover by switching to a TTY (Ctrl+Alt+F2), logging in, and running:",
    )?;

    writeln!(
        script,
        "echo 'Virtu: removing GPU + companion audio from vfio-pci binding table'"
    )?;
    for id in &ctx.vfio_pci_ids {
        let (vendor, device) = pci_id_split(id)?;
        writeln!(
            script,
            "echo '{vendor} {device}' > /sys/bus/pci/drivers/vfio-pci/remove_id || true"
        )?;
    }
    writeln!(script)?;

    writeln!(script, "echo 'Virtu: reloading host GPU driver'")?;
    write_rebind_drivers(&mut script, &ctx.gpu_vendor)?;
    writeln!(script)?;

    writeln!(
        script,
        "echo 'Virtu: starting display manager {dm_service}'"
    )?;
    writeln!(script, "systemctl start {dm_service}")?;
    writeln!(script)?;

    writeln!(script, "echo 'Virtu: host GPU available again'")?;
    Ok(script)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn display_manager_service(dm: &DisplayManager) -> Result<&'static str, HookScriptError> {
    match dm {
        DisplayManager::Gdm => Ok("gdm"),
        DisplayManager::Sddm => Ok("sddm"),
        DisplayManager::LightDm => Ok("lightdm"),
        DisplayManager::Greetd => Ok("greetd"),
        DisplayManager::Ly => Ok("ly"),
        DisplayManager::Lxdm => Ok("lxdm"),
        DisplayManager::None => Err(HookScriptError::NoDisplayManager),
        DisplayManager::Unknown => Err(HookScriptError::UnknownDisplayManager),
    }
}

fn write_header(
    script: &mut String,
    vm_name: &str,
    stage: &str,
    summary: &str,
) -> Result<(), HookScriptError> {
    writeln!(script, "#!/usr/bin/env bash")?;
    writeln!(script, "# Virtu single-GPU hook for VM `{vm_name}`")?;
    writeln!(script, "# Stage: {stage}")?;
    writeln!(script, "# Purpose: {summary}")?;
    writeln!(
        script,
        "# Generated by Virtu. Do not edit by hand: rerun `virtu apply` to refresh."
    )?;
    writeln!(script, "set -eu")?;
    writeln!(script)?;
    Ok(())
}

fn write_recovery_trap(
    script: &mut String,
    dm_service: &str,
    headline: &str,
) -> Result<(), HookScriptError> {
    // Single-quoted heredoc so $-substitutions inside the message are
    // preserved verbatim; we only interpolate the dm_service into the
    // trap body via format!.
    writeln!(
        script,
        "_virtu_recovery() {{\n  echo 'Virtu hook failed at:' >&2\n  echo \"  ${{BASH_SOURCE[0]}}:${{LINENO}}\" >&2\n  echo '' >&2\n  echo '{headline}' >&2\n  echo '  systemctl start {dm_service}' >&2\n  echo '' >&2\n  echo 'Then re-bind the host GPU driver if needed:' >&2\n  echo '  modprobe -r vfio_pci' >&2\n}}\ntrap _virtu_recovery ERR\n"
    )?;
    Ok(())
}

fn write_unbind_drivers(script: &mut String, vendor: &GpuVendor) -> Result<(), HookScriptError> {
    match vendor {
        GpuVendor::Nvidia => {
            writeln!(
                script,
                "systemctl stop nvidia-persistenced.service 2>/dev/null || true"
            )?;
            writeln!(script, "modprobe -r nvidia_drm || true")?;
            writeln!(script, "modprobe -r nvidia_modeset || true")?;
            writeln!(script, "modprobe -r nvidia_uvm || true")?;
            writeln!(script, "modprobe -r nvidia || true")?;
        }
        GpuVendor::Amd => {
            writeln!(script, "modprobe -r amdgpu || true")?;
            writeln!(script, "modprobe -r radeon || true")?;
        }
        GpuVendor::Intel => {
            writeln!(script, "modprobe -r i915 || true")?;
        }
        GpuVendor::Unknown(_) => {
            writeln!(
                script,
                "echo 'Virtu: GPU vendor is unknown; skipping driver unbind. The host driver may still hold the GPU when the VM starts.' >&2"
            )?;
        }
    }
    Ok(())
}

fn write_rebind_drivers(script: &mut String, vendor: &GpuVendor) -> Result<(), HookScriptError> {
    match vendor {
        GpuVendor::Nvidia => {
            writeln!(script, "modprobe nvidia")?;
            writeln!(script, "modprobe nvidia_modeset")?;
            writeln!(script, "modprobe nvidia_uvm")?;
            writeln!(script, "modprobe nvidia_drm")?;
            writeln!(
                script,
                "systemctl start nvidia-persistenced.service 2>/dev/null || true"
            )?;
        }
        GpuVendor::Amd => {
            writeln!(script, "modprobe amdgpu")?;
        }
        GpuVendor::Intel => {
            writeln!(script, "modprobe i915")?;
        }
        GpuVendor::Unknown(_) => {
            writeln!(
                script,
                "echo 'Virtu: GPU vendor unknown; the host driver was not unbound, so nothing to rebind.' >&2"
            )?;
        }
    }
    Ok(())
}

fn pci_id_split(id: &str) -> Result<(&str, &str), HookScriptError> {
    let mut parts = id.splitn(2, ':');
    let vendor = parts
        .next()
        .ok_or_else(|| HookScriptError::InvalidPciId(id.to_string()))?;
    let device = parts
        .next()
        .ok_or_else(|| HookScriptError::InvalidPciId(id.to_string()))?;
    if vendor.len() != 4
        || device.len() != 4
        || !vendor.chars().all(|c| c.is_ascii_hexdigit())
        || !device.chars().all(|c| c.is_ascii_hexdigit())
    {
        return Err(HookScriptError::InvalidPciId(id.to_string()));
    }
    Ok((vendor, device))
}

fn validate_pci_ids(ids: &[String]) -> Result<(), HookScriptError> {
    for id in ids {
        pci_id_split(id)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(vendor: GpuVendor, dm: DisplayManager) -> HookContext {
        HookContext {
            vm_name: "virtu-windows".to_string(),
            display_manager: dm,
            gpu_vendor: vendor,
            vfio_pci_ids: vec!["10de:1f08".to_string(), "10de:10f9".to_string()],
        }
    }

    #[test]
    fn release_script_for_nvidia_sddm_includes_driver_unload_and_vfio_bind() {
        let script =
            release_script(&ctx(GpuVendor::Nvidia, DisplayManager::Sddm)).expect("renders");
        // Strict shell + recovery trap.
        assert!(script.starts_with("#!/usr/bin/env bash\n"));
        assert!(script.contains("set -eu"));
        assert!(script.contains("trap _virtu_recovery ERR"));
        assert!(script.contains("systemctl start sddm"));
        // Stops the display manager.
        assert!(script.contains("systemctl stop sddm"));
        // Unbinds the NVIDIA stack in the documented order.
        let drm = script
            .find("modprobe -r nvidia_drm")
            .expect("nvidia_drm unload present");
        let modeset = script
            .find("modprobe -r nvidia_modeset")
            .expect("nvidia_modeset unload present");
        let uvm = script
            .find("modprobe -r nvidia_uvm")
            .expect("nvidia_uvm unload present");
        let nvidia = script
            .find("modprobe -r nvidia ")
            .expect("nvidia base unload present");
        assert!(drm < modeset && modeset < uvm && uvm < nvidia);
        // Loads vfio-pci.
        assert!(script.contains("modprobe vfio-pci"));
        // Binds each PCI id by vendor + device with a space, the format
        // /sys/bus/pci/drivers/vfio-pci/new_id expects.
        assert!(script.contains("echo '10de 1f08' > /sys/bus/pci/drivers/vfio-pci/new_id"));
        assert!(script.contains("echo '10de 10f9' > /sys/bus/pci/drivers/vfio-pci/new_id"));
    }

    #[test]
    fn release_script_for_amd_gdm_unloads_amdgpu_then_radeon() {
        let script = release_script(&ctx(GpuVendor::Amd, DisplayManager::Gdm)).expect("renders");
        assert!(script.contains("systemctl stop gdm"));
        let amdgpu = script.find("modprobe -r amdgpu").expect("amdgpu unload");
        let radeon = script.find("modprobe -r radeon").expect("radeon unload");
        assert!(amdgpu < radeon);
        assert!(!script.contains("modprobe -r nvidia"));
    }

    #[test]
    fn release_script_for_intel_lightdm_unloads_i915() {
        let script =
            release_script(&ctx(GpuVendor::Intel, DisplayManager::LightDm)).expect("renders");
        assert!(script.contains("systemctl stop lightdm"));
        assert!(script.contains("modprobe -r i915"));
        assert!(!script.contains("modprobe -r nvidia"));
        assert!(!script.contains("modprobe -r amdgpu"));
    }

    #[test]
    fn release_script_for_unknown_vendor_skips_unbind_and_warns() {
        let script = release_script(&ctx(
            GpuVendor::Unknown("ffff".to_string()),
            DisplayManager::Sddm,
        ))
        .expect("renders");
        assert!(script.contains("GPU vendor is unknown"));
        // The unbind block must not contain real driver removals. The
        // recovery trap mentions `modprobe -r vfio_pci` as a hint for
        // the user to run from TTY if everything failed; that line
        // does not count as an unbind step. We anchor on the unbind
        // banner and assert no `modprobe -r <name>` follows it before
        // the next echo banner.
        let unbind_banner = script
            .find("Virtu: unbinding host GPU drivers")
            .expect("unbind banner present");
        let next_banner = script
            .find("Virtu: loading vfio-pci")
            .expect("next banner present");
        let unbind_block = &script[unbind_banner..next_banner];
        assert!(!unbind_block.contains("modprobe -r"));
    }

    #[test]
    fn reattach_script_reverses_the_sequence() {
        let script =
            reattach_script(&ctx(GpuVendor::Nvidia, DisplayManager::Sddm)).expect("renders");
        // Use unique anchors that only appear once each (the recovery
        // trap mentions `systemctl start sddm` too, so naive `find`
        // would land on the trap text).
        let remove = script
            .find("/sys/bus/pci/drivers/vfio-pci/remove_id")
            .expect("remove_id present");
        let reload_banner = script
            .find("Virtu: reloading host GPU driver")
            .expect("reload banner present");
        let start_banner = script
            .find("Virtu: starting display manager sddm")
            .expect("start banner present");
        assert!(remove < reload_banner);
        assert!(reload_banner < start_banner);
    }

    #[test]
    fn reattach_script_starts_amd_driver_then_display_manager() {
        let script =
            reattach_script(&ctx(GpuVendor::Amd, DisplayManager::Greetd)).expect("renders");
        let amd = script.find("modprobe amdgpu").expect("amdgpu reload");
        let start_banner = script
            .find("Virtu: starting display manager greetd")
            .expect("greetd start banner");
        assert!(amd < start_banner);
    }

    #[test]
    fn release_script_refuses_unknown_display_manager() {
        let err = release_script(&ctx(GpuVendor::Nvidia, DisplayManager::Unknown)).unwrap_err();
        assert_eq!(err, HookScriptError::UnknownDisplayManager);
    }

    #[test]
    fn release_script_refuses_none_display_manager() {
        // TTY-only hosts cannot run hook-handoff: nothing for the begin
        // script to stop. Better to refuse than to ship a hook that
        // silently does nothing.
        let err = release_script(&ctx(GpuVendor::Amd, DisplayManager::None)).unwrap_err();
        assert_eq!(err, HookScriptError::NoDisplayManager);
    }

    #[test]
    fn release_script_refuses_invalid_pci_ids() {
        let mut input = ctx(GpuVendor::Nvidia, DisplayManager::Sddm);
        input.vfio_pci_ids = vec!["10de1f08".to_string()]; // missing colon
        let err = release_script(&input).unwrap_err();
        assert_eq!(err, HookScriptError::InvalidPciId("10de1f08".to_string()));
    }

    #[test]
    fn release_script_refuses_non_hex_pci_ids() {
        let mut input = ctx(GpuVendor::Nvidia, DisplayManager::Sddm);
        input.vfio_pci_ids = vec!["zzzz:1f08".to_string()];
        let err = release_script(&input).unwrap_err();
        assert!(matches!(err, HookScriptError::InvalidPciId(_)));
    }

    #[test]
    fn release_script_includes_recovery_trap_with_dm_service() {
        let script =
            release_script(&ctx(GpuVendor::Nvidia, DisplayManager::Sddm)).expect("renders");
        // The recovery trap must mention the actual display-manager
        // service so the user knows exactly which command to run from
        // the TTY.
        assert!(script.contains("'  systemctl start sddm'"));
        assert!(script.contains("trap _virtu_recovery ERR"));
    }

    /// Optional real-host syntax check. Pipes every variant we
    /// generate through `bash -n` and asserts they all parse. Gated
    /// behind the same env-var pattern as `validate_xml_real_host_smoke`
    /// so normal `cargo test` stays hermetic.
    ///
    /// Set `VIRTU_RUN_BASH_SYNTAX_SMOKE=1` to opt in.
    #[test]
    fn hook_scripts_pass_bash_n_for_every_vendor_dm_combo() {
        if std::env::var("VIRTU_RUN_BASH_SYNTAX_SMOKE").ok().as_deref() != Some("1") {
            return;
        }
        if which::which("bash").is_err() {
            return;
        }

        use std::io::Write;
        use std::process::Command;
        let vendors = [
            GpuVendor::Nvidia,
            GpuVendor::Amd,
            GpuVendor::Intel,
            GpuVendor::Unknown("ffff".to_string()),
        ];
        let dms = [
            DisplayManager::Gdm,
            DisplayManager::Sddm,
            DisplayManager::LightDm,
            DisplayManager::Greetd,
            DisplayManager::Ly,
            DisplayManager::Lxdm,
        ];

        for vendor in &vendors {
            for dm in &dms {
                let mut input = ctx(vendor.clone(), dm.clone());
                input.vfio_pci_ids = vec!["10de:1f08".to_string()];
                for (label, content) in [
                    ("release", release_script(&input).unwrap()),
                    ("reattach", reattach_script(&input).unwrap()),
                ] {
                    let mut tmp = tempfile::Builder::new()
                        .prefix("virtu-hook-")
                        .suffix(".sh")
                        .tempfile()
                        .unwrap();
                    tmp.as_file_mut().write_all(content.as_bytes()).unwrap();
                    tmp.as_file_mut().flush().unwrap();
                    let output = Command::new("bash")
                        .arg("-n")
                        .arg(tmp.path())
                        .output()
                        .unwrap();
                    assert!(
                        output.status.success(),
                        "{label} script for vendor={vendor:?} dm={dm:?} failed bash -n: \
                         stderr={}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            }
        }
    }
}
