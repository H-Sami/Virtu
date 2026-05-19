//! End-to-end Phase A → reboot simulation → Phase B coverage for the
//! single-GPU hook installer (slice 9.5).
//!
//! These tests exercise the full apply → resume cycle for a single-GPU
//! passthrough plan against the `MemoryFileSystem`. Phase A captures the
//! snapshot, edits the bootloader, writes the VFIO modprobe snippet, and
//! rewrites the initramfs config. We then synthesize a `SystemProfile`
//! that matches what the host would look like after rebooting into the
//! new cmdline, feed it to `verify_phase_a_landed`, and run Phase B in
//! `HostCommandMode::Skip`. In Skip mode, `qemu-img create` and `virsh
//! define` are bypassed but `validate_bash_script` (which runs `bash -n`
//! locally) still fires when bash is on PATH; the hook installer runs
//! end-to-end: scripts written, executable bits set, manifest persisted,
//! and `RestoreAction::RemoveHookScripts` pushed.
//!
//! What we assert:
//!
//! - The verifier returns `Ready` for a post-reboot profile that
//!   reflects every Phase A change.
//! - The three hook scripts at
//!   `/etc/libvirt/hooks/qemu.d/<vm_name>` (dispatcher),
//!   `/etc/libvirt/hooks/qemu.d/<vm_name>.d/release`, and
//!   `/etc/libvirt/hooks/qemu.d/<vm_name>.d/reattach` are written,
//!   marked executable, and contain the expected anchor strings.
//! - The manifest carries `RestoreAction::RemoveHookScripts { vm_name }`
//!   so `virtu rollback --to <id>` will remove them.
//! - `verify_hook_install` returns an empty divergence list against the
//!   freshly installed scripts.
//! - Tampering with one helper surfaces as exactly one
//!   `Divergence::HookScriptDivergent` with the captured and live
//!   hashes filled in.
//! - Optional bash-syntax smoke (gated by `VIRTU_RUN_BASH_SYNTAX_SMOKE=1`)
//!   pipes every produced script through `validate_bash_script` to
//!   catch real bash regressions on a Linux dev host.
//!
//! No real virsh / qemu-img / virt-xml-validate is invoked. The
//! `validate_bash_script` wrapper is hermetic (`bash -n` parses without
//! executing) so the hook installer is allowed to call it even in Skip
//! mode.

use std::path::PathBuf;

use virtu::detect::bootloader::{self, BootloaderKind};
use virtu::detect::display_manager::{self, DisplayManager};
use virtu::detect::display_server::DisplayServer;
use virtu::detect::initramfs::{self, InitramfsSystem};
use virtu::detect::readiness::{self, KernelHeadersInfo};
use virtu::detect::virtualization::VirtInfo;
use virtu::detect::{
    audio, cpu, distro, gpu, iommu, memory, monitors, storage, usb, SystemProfile,
};
use virtu::engine::{
    build_compatibility_report, execute_phase_a, execute_phase_b, plan, verify_hook_install,
    verify_phase_a_landed, HostCommandMode, RegenerateMode, ResumeReadiness, StepKind,
};
use virtu::snapshot::{
    pending::DEFAULT_FILENAME as PENDING_FILENAME, FileSystem, MemoryFileSystem, PendingPlan,
    RestoreAction, SnapshotManifest, MANIFEST_FILENAME,
};
use virtu::vm::{
    GpuPassthroughMode, GpuRole, LookingGlassChoice, MonitorPlan, PassthroughConfig,
    SingleMonitorStrategy,
};

fn fixture(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(path)
}

/// Build a multi-GPU fixture profile (Intel iGPU + NVIDIA dGPU) with
/// SDDM detected. The single-GPU plan ignores the iGPU and points
/// passthrough at the dGPU; `derived_mode` then returns `SingleGpu`,
/// which is exactly the path the integration test wants to exercise.
async fn fixture_profile() -> SystemProfile {
    let sysfs = fixture("sysfs");
    let iommu_groups = iommu::detect_groups_from_sysfs_root(&sysfs).await.unwrap();
    let mut gpus = gpu::detect_all_from_sysfs_root(&sysfs).await.unwrap();

    for gpu in &mut gpus {
        gpu.iommu_isolated = iommu::is_gpu_isolated(&iommu_groups, &gpu.pci_slot);
        gpu.iommu_group_id = iommu::group_for_pci_slot(&iommu_groups, &gpu.pci_slot);
        gpu.vfio_compatible =
            gpu.iommu_isolated && gpu.current_driver.as_deref() != Some("vfio-pci");
    }

    let cpuinfo = std::fs::read_to_string(fixture("proc/cpuinfo-intel-2socket-ht")).unwrap();
    let meminfo = std::fs::read_to_string(fixture("proc/meminfo-basic")).unwrap();
    let os_release = std::fs::read_to_string(fixture("etc/os-release-arch")).unwrap();
    let readiness = readiness::detect_from_root(fixture("readiness"))
        .await
        .unwrap();

    let profile = SystemProfile {
        cpu: cpu::parse_cpuinfo(&cpuinfo, true, Vec::new()),
        gpus,
        iommu_groups,
        ram: memory::parse_meminfo(&meminfo),
        distro: distro::parse_distro_info(&os_release),
        bootloader: bootloader::detect_from_root(fixture("bootloaders/grub"), true)
            .await
            .unwrap(),
        initramfs_system: initramfs::detect_from_root(fixture("initramfs/arch"), false)
            .await
            .unwrap(),
        display_manager: display_manager::detect_from_root(fixture("display/sddm"))
            .await
            .unwrap(),
        display_server: DisplayServer::Wayland,
        audio: audio::detect_from_root(fixture("audio/pipewire"))
            .await
            .unwrap(),
        monitors: monitors::detect_from_drm_root(fixture("sysfs/class/drm"))
            .await
            .unwrap(),
        usb_devices: usb::detect_input_devices_from_root(fixture("usb"))
            .await
            .unwrap(),
        storage: storage::detect_from_root(fixture("storage"), false)
            .await
            .unwrap(),
        virtualization: VirtInfo {
            qemu_version: Some("QEMU emulator version 8.2.0".to_string()),
            libvirt_version: Some("10.0.0".to_string()),
            virsh_available: true,
            virt_manager_available: false,
            libvirtd_running: true,
        },
        secure_boot: readiness.secure_boot,
        kernel_cmdline: readiness.kernel_cmdline.clone(),
        readiness,
        scan_timestamp: chrono::Utc::now(),
    };

    // Sanity guard: the rest of the test wires single-GPU mode by
    // ignoring the iGPU, so we need to be sure detection actually
    // surfaced an SDDM display manager. If a future fixture change
    // ever drops SDDM, this assertion fails loudly here instead of
    // surfacing as a confusing `HookScriptError::UnknownDisplayManager`
    // deep inside Phase B.
    assert_eq!(profile.display_manager, DisplayManager::Sddm);
    assert!(
        !profile.gpus.is_empty(),
        "fixture must provide at least one GPU"
    );

    profile
}

/// Build a single-GPU `PassthroughConfig` keyed off the fixture profile.
/// The iGPU is moved to `Ignored` so only the dGPU remains as
/// `Passthrough`; `derived_mode` then returns `SingleGpu`, the planner
/// emits `StepKind::HookInstall`, and `vm_name` is set to a stable
/// value the rest of the test asserts on.
fn single_gpu_config(profile: &SystemProfile) -> PassthroughConfig {
    let mut config = PassthroughConfig::recommended_defaults(profile)
        .expect("recommended_defaults works for the multi-GPU fixture profile");
    for role in &mut config.gpu_roles {
        if role.role == GpuRole::Host {
            role.role = GpuRole::Ignored;
        }
    }
    config.gpu_mode = GpuPassthroughMode::SingleGpu;
    config.monitor_plan = MonitorPlan::OneMonitor {
        strategy: SingleMonitorStrategy::HookHandoff,
    };
    config.looking_glass = LookingGlassChoice::Disabled;
    config.vm_name = "virtu-singlegpu".to_string();
    config
}

/// Seed the in-memory FS with the host-side files Phase A reads (and
/// expects to mutate). Returns (fs, snapshots_root, state_root).
fn seed_filesystem_for_plan(plan: &virtu::engine::Plan) -> (MemoryFileSystem, PathBuf, PathBuf) {
    let fs = MemoryFileSystem::new();
    let snapshots_root = PathBuf::from("/var/lib/virtu/snapshots");
    let state_root = PathBuf::from("/var/lib/virtu/state");
    fs.create_dir_all(&snapshots_root).unwrap();
    fs.create_dir_all(&state_root).unwrap();

    for step in &plan.steps {
        match step.kind {
            StepKind::BootloaderWrite => {
                if let Some(target) = step.touches.first() {
                    if let Some(parent) = target.parent() {
                        fs.create_dir_all(parent).unwrap();
                    }
                    fs.write_atomic(
                        target,
                        b"GRUB_DEFAULT=0\nGRUB_TIMEOUT=5\nGRUB_CMDLINE_LINUX_DEFAULT=\"quiet splash\"\n",
                    )
                    .unwrap();
                }
            }
            StepKind::InitramfsWrite => {
                if let Some(target) = step.touches.first() {
                    if let Some(parent) = target.parent() {
                        fs.create_dir_all(parent).unwrap();
                    }
                    fs.write_atomic(
                        target,
                        b"MODULES=()\nHOOKS=(base udev autodetect modconf block filesystems keyboard fsck)\n",
                    )
                    .unwrap();
                }
            }
            StepKind::VfioConfig => {
                if let Some(target) = step.touches.first() {
                    if let Some(parent) = target.parent() {
                        fs.create_dir_all(parent).unwrap();
                    }
                }
            }
            _ => {}
        }
    }

    (fs, snapshots_root, state_root)
}

/// Build a synthetic post-reboot `SystemProfile` that the verifier will
/// accept as `Ready` for the supplied pending plan. The host fingerprint
/// stays identical (same distro, kernel, bootloader, initramfs, display
/// manager) but the kernel cmdline and loaded modules now reflect the
/// values Phase A wrote. `display_manager` is preserved so the hook
/// installer reads the same DM in Phase B as in Phase A.
fn simulate_post_reboot_profile(
    pre_reboot: &SystemProfile,
    pending: &PendingPlan,
) -> SystemProfile {
    let mut profile = pre_reboot.clone();

    let cpu_param = if pre_reboot.cpu.vendor.to_lowercase().contains("amd") {
        "amd_iommu=on"
    } else {
        "intel_iommu=on"
    };
    let mut new_cmdline = format!("BOOT_IMAGE=/vmlinuz {cpu_param} iommu=pt");
    if !pending.host_fingerprint.passthrough_pci_ids.is_empty() {
        new_cmdline.push_str(&format!(
            " vfio-pci.ids={}",
            pending.host_fingerprint.passthrough_pci_ids.join(",")
        ));
    }
    profile.kernel_cmdline = new_cmdline.clone();
    profile.readiness.kernel_cmdline = new_cmdline;
    profile.readiness.kernel_cmdline_params = profile
        .readiness
        .kernel_cmdline
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();

    if !profile
        .readiness
        .loaded_modules
        .iter()
        .any(|m| m == "vfio_pci")
    {
        profile
            .readiness
            .loaded_modules
            .push("vfio_pci".to_string());
    }

    for pci_id in &pending.host_fingerprint.passthrough_pci_ids {
        let mut parts = pci_id.splitn(2, ':');
        let vendor = parts.next().unwrap_or_default();
        let device = parts.next().unwrap_or_default();
        for gpu in profile.gpus.iter_mut() {
            if gpu.vendor_id.eq_ignore_ascii_case(vendor)
                && gpu.device_id.eq_ignore_ascii_case(device)
            {
                gpu.current_driver = Some("vfio-pci".to_string());
            }
        }
    }

    if profile.readiness.kernel_headers.path.is_none() {
        profile.readiness.kernel_headers = KernelHeadersInfo {
            present: true,
            path: Some(PathBuf::from("/usr/lib/modules/6.10.0/build")),
        };
    }

    profile
}

#[tokio::test]
async fn single_gpu_phase_a_then_phase_b_installs_hook_scripts_and_verifies_clean() {
    // 1. Build the profile + single-GPU config + plan.
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let config = single_gpu_config(&profile);
    let plan = plan(&profile, &report, &config).expect("single-GPU plan must build");

    // Sanity: planner emitted the HookInstall step.
    assert_eq!(profile.bootloader.kind, BootloaderKind::Grub2);
    assert_eq!(profile.initramfs_system, InitramfsSystem::Mkinitcpio);
    let hook_step = plan
        .steps
        .iter()
        .find(|s| s.kind == StepKind::HookInstall)
        .expect("single-GPU plan must include a HookInstall step");
    assert!(
        hook_step.must_confirm(),
        "hook step must require confirmation"
    );

    let (fs, snapshots_root, state_root) = seed_filesystem_for_plan(&plan);

    // 2. Run Phase A.
    let phase_a = execute_phase_a(
        &plan,
        &profile,
        &config,
        &fs,
        &snapshots_root,
        &state_root,
        RegenerateMode::Skip,
    )
    .expect("phase A succeeds for the single-GPU plan");

    // 3. Read the persisted PendingPlan back so Phase B sees exactly
    //    what the CLI would. The pending record must carry the
    //    HookInstall step in its remaining work.
    let pending_path = state_root.join(PENDING_FILENAME);
    let pending_bytes = fs.read(&pending_path).unwrap();
    let pending: PendingPlan = toml::from_str(&String::from_utf8(pending_bytes).unwrap()).unwrap();
    assert_eq!(pending.snapshot_id, phase_a.snapshot_id);
    assert!(
        pending
            .remaining_steps
            .iter()
            .any(|s| s.kind == StepKind::HookInstall),
        "pending plan must carry the HookInstall step into Phase B"
    );

    // 4. Simulate the reboot and run the verifier.
    let post_reboot = simulate_post_reboot_profile(&profile, &pending);
    match verify_phase_a_landed(&post_reboot, &pending) {
        ResumeReadiness::Ready => {}
        other => panic!("verifier must report Ready after the reboot simulation, got {other:?}"),
    }

    // 5. Run Phase B in Skip mode. validate_bash_script still runs
    //    when bash is on PATH; the assertion below tolerates either
    //    case (bash present or absent) so the test stays portable.
    let phase_b = execute_phase_b(
        &pending,
        &post_reboot,
        &fs,
        &snapshots_root,
        &state_root,
        HostCommandMode::Skip,
    )
    .expect("phase B succeeds and installs the hook scripts");

    assert!(
        phase_b.completed_steps.contains(&StepKind::HookInstall),
        "Phase B must run the HookInstall step (got {:?})",
        phase_b.completed_steps
    );
    // The single-GPU plan still emits VmXmlGenerate + VmRegister +
    // Verify alongside HookInstall; those run as usual. We pin
    // HookInstall as the focus and keep an eye on Verify so the
    // `Phase B Verify` summary still surfaces.
    assert!(phase_b.completed_steps.contains(&StepKind::VmXmlGenerate));
    assert!(phase_b.completed_steps.contains(&StepKind::VmRegister));
    assert!(phase_b.completed_steps.contains(&StepKind::Verify));

    // 6. Three hook scripts on disk, all executable, with the right
    //    anchor strings.
    let vm_name = config.vm_name.clone();
    let dispatcher = PathBuf::from(format!("/etc/libvirt/hooks/qemu.d/{vm_name}"));
    let release = PathBuf::from(format!("/etc/libvirt/hooks/qemu.d/{vm_name}.d/release"));
    let reattach = PathBuf::from(format!("/etc/libvirt/hooks/qemu.d/{vm_name}.d/reattach"));
    for path in [&dispatcher, &release, &reattach] {
        assert!(fs.exists(path), "hook script {} must exist", path.display());
        assert!(
            fs.is_executable(path),
            "hook script {} must be marked executable",
            path.display()
        );
    }

    let dispatcher_text = String::from_utf8(fs.read(&dispatcher).unwrap()).unwrap();
    assert!(
        dispatcher_text.contains(&format!(
            "HOOK_DIR=\"/etc/libvirt/hooks/qemu.d/{vm_name}.d\""
        )),
        "dispatcher must point at the per-VM helper dir"
    );
    assert!(
        dispatcher_text.contains(&format!("if [ \"$vm\" != '{vm_name}' ]; then")),
        "dispatcher must short-circuit for other domains"
    );

    let release_text = String::from_utf8(fs.read(&release).unwrap()).unwrap();
    assert!(release_text.contains("systemctl stop sddm"));
    assert!(release_text.contains("modprobe vfio-pci"));
    // NVIDIA fixture: the documented unbind sequence must show up in
    // order. Anchor on each line individually so a future reordering
    // fails with a clear diff.
    let drm_pos = release_text
        .find("modprobe -r nvidia_drm")
        .expect("release script must unload nvidia_drm");
    let modeset_pos = release_text
        .find("modprobe -r nvidia_modeset")
        .expect("release script must unload nvidia_modeset");
    let uvm_pos = release_text
        .find("modprobe -r nvidia_uvm")
        .expect("release script must unload nvidia_uvm");
    let nvidia_pos = release_text
        .find("modprobe -r nvidia ")
        .expect("release script must unload the nvidia base module");
    assert!(drm_pos < modeset_pos && modeset_pos < uvm_pos && uvm_pos < nvidia_pos);

    let reattach_text = String::from_utf8(fs.read(&reattach).unwrap()).unwrap();
    assert!(reattach_text.contains("systemctl start sddm"));
    assert!(reattach_text.contains("modprobe nvidia"));

    // 7. Manifest carries `RemoveHookScripts { vm_name }`.
    let manifest_path = snapshots_root
        .join(&phase_a.snapshot_id)
        .join(MANIFEST_FILENAME);
    let manifest_bytes = fs.read(&manifest_path).unwrap();
    let manifest: SnapshotManifest =
        toml::from_str(&String::from_utf8(manifest_bytes).unwrap()).unwrap();
    assert!(
        manifest.restore_actions.iter().any(|a| matches!(
            a,
            RestoreAction::RemoveHookScripts { vm_name: name } if name == &vm_name
        )),
        "manifest must record a RemoveHookScripts action keyed on the VM name"
    );
    // In Skip mode no virsh define ran, so no UndefineLibvirtDomain
    // restore action should have been pushed.
    assert!(!manifest
        .restore_actions
        .iter()
        .any(|a| matches!(a, RestoreAction::UndefineLibvirtDomain { .. })));

    // 8. The fresh install has no divergences.
    let divergences = verify_hook_install(&manifest, &fs, &vm_name);
    assert!(
        divergences.is_empty(),
        "verify_hook_install must report no divergences against the freshly written scripts; got {divergences:?}"
    );

    // 9. Pending was cleared.
    assert!(!fs.exists(&pending_path));
    assert!(phase_b.pending_cleared);
}

#[tokio::test]
async fn tampered_hook_helper_surfaces_as_divergent_in_verify_hook_install() {
    // Defense-in-depth: install the hooks normally, then overwrite the
    // release helper with different bytes. `verify_hook_install` must
    // surface that as exactly one `Divergence::HookScriptDivergent`
    // entry with both the captured and live hashes filled in. This
    // protects against a future bug in the verifier silently masking
    // host-side tampering.
    use virtu::engine::Divergence;

    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let config = single_gpu_config(&profile);
    let plan = plan(&profile, &report, &config).expect("single-GPU plan must build");
    let (fs, snapshots_root, state_root) = seed_filesystem_for_plan(&plan);

    let phase_a = execute_phase_a(
        &plan,
        &profile,
        &config,
        &fs,
        &snapshots_root,
        &state_root,
        RegenerateMode::Skip,
    )
    .unwrap();

    let pending_bytes = fs.read(&state_root.join(PENDING_FILENAME)).unwrap();
    let pending: PendingPlan = toml::from_str(&String::from_utf8(pending_bytes).unwrap()).unwrap();
    let post_reboot = simulate_post_reboot_profile(&profile, &pending);

    execute_phase_b(
        &pending,
        &post_reboot,
        &fs,
        &snapshots_root,
        &state_root,
        HostCommandMode::Skip,
    )
    .unwrap();

    // Tamper: overwrite the release helper with different bytes. We
    // remove + rewrite because `MemoryFileSystem::write_atomic` happily
    // overwrites in place; using remove_file first also clears the
    // executable bit, which would normally produce a second divergence.
    // We re-set it so the test isolates exactly the divergent-content
    // path. The captured manifest hash is left untouched.
    let release_path = PathBuf::from(format!(
        "/etc/libvirt/hooks/qemu.d/{}.d/release",
        config.vm_name
    ));
    let tampered_bytes = b"#!/usr/bin/env bash\necho 'tampered'\n";
    fs.remove_file(&release_path).unwrap();
    fs.write_atomic(&release_path, tampered_bytes).unwrap();
    fs.set_executable(&release_path).unwrap();

    let manifest_path = snapshots_root
        .join(&phase_a.snapshot_id)
        .join(MANIFEST_FILENAME);
    let manifest: SnapshotManifest =
        toml::from_str(&String::from_utf8(fs.read(&manifest_path).unwrap()).unwrap()).unwrap();

    let divergences = verify_hook_install(&manifest, &fs, &config.vm_name);
    assert_eq!(
        divergences.len(),
        1,
        "expected exactly one divergence after tampering with the release helper, got {divergences:?}"
    );
    match &divergences[0] {
        Divergence::HookScriptDivergent {
            vm_name,
            path,
            expected_sha256,
            actual_sha256,
        } => {
            assert_eq!(vm_name, &config.vm_name);
            assert_eq!(path, &release_path.display().to_string());
            assert_ne!(
                expected_sha256, actual_sha256,
                "captured and live hashes must differ when bytes change"
            );
            assert!(!expected_sha256.is_empty());
            assert!(!actual_sha256.is_empty());
        }
        other => panic!("expected HookScriptDivergent, got {other:?}"),
    }
}

/// Optional real-host syntax check. Pipes every script the integration
/// test produced through `bash -n` (via the `validate_bash_script`
/// wrapper) and asserts they all parse. Gated behind the same env-var
/// pattern as `validate_xml_real_host_smoke` and the per-vendor combo
/// smoke in `src/config/writers/hooks.rs::tests` so normal `cargo test`
/// stays hermetic.
///
/// Set `VIRTU_RUN_BASH_SYNTAX_SMOKE=1` to opt in.
#[tokio::test]
async fn integration_hook_scripts_pass_bash_syntax_check_when_opted_in() {
    if std::env::var("VIRTU_RUN_BASH_SYNTAX_SMOKE").ok().as_deref() != Some("1") {
        return;
    }
    if which::which("bash").is_err() {
        return;
    }

    use virtu::config::writers::commands::validate_bash_script;

    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let config = single_gpu_config(&profile);
    let plan = plan(&profile, &report, &config).unwrap();
    let (fs, snapshots_root, state_root) = seed_filesystem_for_plan(&plan);

    execute_phase_a(
        &plan,
        &profile,
        &config,
        &fs,
        &snapshots_root,
        &state_root,
        RegenerateMode::Skip,
    )
    .unwrap();
    let pending: PendingPlan = toml::from_str(
        &String::from_utf8(fs.read(&state_root.join(PENDING_FILENAME)).unwrap()).unwrap(),
    )
    .unwrap();
    let post_reboot = simulate_post_reboot_profile(&profile, &pending);
    execute_phase_b(
        &pending,
        &post_reboot,
        &fs,
        &snapshots_root,
        &state_root,
        HostCommandMode::Skip,
    )
    .unwrap();

    let vm_name = &config.vm_name;
    for path in [
        PathBuf::from(format!("/etc/libvirt/hooks/qemu.d/{vm_name}")),
        PathBuf::from(format!("/etc/libvirt/hooks/qemu.d/{vm_name}.d/release")),
        PathBuf::from(format!("/etc/libvirt/hooks/qemu.d/{vm_name}.d/reattach")),
    ] {
        let content = String::from_utf8(fs.read(&path).unwrap()).unwrap();
        validate_bash_script(&content).unwrap_or_else(|err| {
            panic!("bash -n rejected {}: {err}", path.display());
        });
    }
}
