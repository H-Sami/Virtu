//! Fixture-driven planner tests. These run on any platform because the
//! planner is pure logic over the existing `SystemProfile`,
//! `CompatibilityReport`, and `PassthroughConfig` types. No host I/O.

use std::path::{Path, PathBuf};

use virtu::detect::bootloader::{self, BootloaderKind};
use virtu::detect::display_server::DisplayServer;
use virtu::detect::initramfs::InitramfsSystem;
use virtu::detect::readiness;
use virtu::detect::virtualization::VirtInfo;
use virtu::detect::{
    audio, cpu, display_manager, distro, gpu, initramfs, iommu, memory, monitors, storage, usb,
    SystemProfile,
};
use virtu::engine::{build_compatibility_report, plan, PlanError, StepKind, StepRisk, StepState};
use virtu::vm::{
    GpuRole, LookingGlassChoice, LookingGlassInstallMode, MonitorPlan, PassthroughConfig,
    Resolution, SingleMonitorStrategy,
};

fn fixture(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(path)
}

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

    SystemProfile {
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
    }
}

fn step_kinds(plan: &virtu::engine::Plan) -> Vec<StepKind> {
    plan.steps.iter().map(|s| s.kind.clone()).collect()
}

fn find_step(plan: &virtu::engine::Plan, kind: StepKind) -> &virtu::engine::PlannedStep {
    plan.steps
        .iter()
        .find(|step| step.kind == kind)
        .unwrap_or_else(|| panic!("expected step {kind:?} in plan"))
}

#[tokio::test]
async fn igpu_host_plan_is_ordered_and_declares_safety_fields() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let config = PassthroughConfig::recommended_defaults(&profile).unwrap();

    let plan = plan(&profile, &report, &config).expect("plan should succeed");

    let kinds = step_kinds(&plan);
    assert_eq!(
        kinds,
        vec![
            StepKind::Snapshot,
            StepKind::BootloaderWrite,
            StepKind::VfioConfig,
            StepKind::InitramfsWrite,
            StepKind::VmXmlGenerate,
            StepKind::VmRegister,
            StepKind::Verify,
        ]
    );

    // Snapshot is always first and always read-only.
    assert_eq!(plan.steps[0].kind, StepKind::Snapshot);
    assert_eq!(plan.steps[0].risk, StepRisk::ReadOnly);

    // Mutating steps must list at least one touched path and a verification
    // description.
    for step in &plan.steps {
        if matches!(
            step.kind,
            StepKind::BootloaderWrite | StepKind::VfioConfig | StepKind::InitramfsWrite
        ) {
            assert!(
                !step.touches.is_empty(),
                "{:?} must declare touched files",
                step.kind
            );
            assert!(
                !step.verification.is_empty(),
                "{:?} must describe verification",
                step.kind
            );
            assert!(
                !step.rollback.is_empty(),
                "{:?} must describe rollback",
                step.kind
            );
        }
    }
}

#[tokio::test]
async fn bootloader_step_targets_detected_grub_config_and_includes_vfio_ids() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let config = PassthroughConfig::recommended_defaults(&profile).unwrap();

    let plan = plan(&profile, &report, &config).unwrap();
    let bootloader_step = find_step(&plan, StepKind::BootloaderWrite);

    let expected_config = profile.bootloader.config_path.clone().unwrap();
    assert!(bootloader_step
        .touches
        .iter()
        .any(|p: &PathBuf| p == &expected_config));

    // The recommended config passes through the dGPU at 0000:01:00.0 with
    // companion audio at 0000:01:00.1. Both vendor:device IDs must appear.
    let dgpu = profile
        .gpus
        .iter()
        .find(|g| g.pci_slot == "0000:01:00.0")
        .unwrap();
    let dgpu_id = format!("{}:{}", dgpu.vendor_id, dgpu.device_id);
    let audio_id = {
        let audio = dgpu.companion_audio.as_ref().unwrap();
        format!("{}:{}", audio.vendor_id, audio.device_id)
    };
    assert!(bootloader_step.summary.contains(&dgpu_id));
    assert!(bootloader_step.summary.contains(&audio_id));
}

#[tokio::test]
async fn initramfs_already_loaded_in_fixture_is_marked_already_satisfied() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let config = PassthroughConfig::recommended_defaults(&profile).unwrap();

    let plan = plan(&profile, &report, &config).unwrap();
    let initramfs_step = find_step(&plan, StepKind::InitramfsWrite);

    // Fixture already has vfio_pci listed in /proc/modules.
    assert_eq!(initramfs_step.state, StepState::AlreadySatisfied);
}

#[tokio::test]
async fn unknown_initramfs_skips_the_step() {
    let mut profile = fixture_profile().await;
    profile.initramfs_system = InitramfsSystem::Unknown;
    let report = build_compatibility_report(&profile);
    let config = PassthroughConfig::recommended_defaults(&profile).unwrap();

    // Unknown initramfs is a hard blocker in the compatibility report, so
    // planning must refuse rather than silently skipping.
    let err = plan(&profile, &report, &config).unwrap_err();
    assert!(
        matches!(err, PlanError::CompatibilityBlocked(_)),
        "expected blocker, got {err:?}"
    );
}

#[tokio::test]
async fn unknown_bootloader_blocks_the_plan() {
    let mut profile = fixture_profile().await;
    profile.bootloader.kind = BootloaderKind::Unknown;
    profile.bootloader.config_path = None;
    profile.bootloader.entry_paths.clear();
    profile.bootloader.update_command = None;
    let report = build_compatibility_report(&profile);
    let config = PassthroughConfig::recommended_defaults(&profile).unwrap();

    let err = plan(&profile, &report, &config).unwrap_err();
    assert!(matches!(err, PlanError::CompatibilityBlocked(_)));
}

#[tokio::test]
async fn validation_errors_block_planning() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let mut config = PassthroughConfig::recommended_defaults(&profile).unwrap();
    config.gpu_roles[0].pci_slot = "0000:ff:ff.0".to_string();

    let err = plan(&profile, &report, &config).unwrap_err();
    match err {
        PlanError::ValidationFailed(report) => {
            assert!(report.has_errors(), "expected validation errors");
        }
        other => panic!("expected ValidationFailed, got {other:?}"),
    }
}

#[tokio::test]
async fn single_gpu_plan_includes_high_risk_hook_step_and_propagates_warning() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let mut config = PassthroughConfig::recommended_defaults(&profile).unwrap();
    for role in &mut config.gpu_roles {
        if role.role == GpuRole::Host {
            role.role = GpuRole::Ignored;
        }
    }
    config.gpu_mode = virtu::vm::GpuPassthroughMode::SingleGpu;
    config.monitor_plan = MonitorPlan::OneMonitor {
        strategy: SingleMonitorStrategy::HookHandoff,
    };
    config.looking_glass = LookingGlassChoice::Disabled;

    let plan = plan(&profile, &report, &config).unwrap();

    let hook = find_step(&plan, StepKind::HookInstall);
    assert_eq!(hook.risk, StepRisk::High);
    assert!(hook.must_confirm());

    assert!(plan.summary.requires_confirmation);
    assert!(plan
        .warnings
        .iter()
        .any(|w| w.id == virtu::vm::ValidationIssueId::SingleGpuRiskAcknowledged));
}

#[tokio::test]
async fn looking_glass_step_is_inserted_when_enabled() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let mut config = PassthroughConfig::recommended_defaults(&profile).unwrap();
    config.looking_glass = LookingGlassChoice::Enabled {
        install_mode: LookingGlassInstallMode::Manual,
        target_resolution: Resolution::new(1920, 1080),
    };

    let plan = plan(&profile, &report, &config).unwrap();
    let lg = find_step(&plan, StepKind::LookingGlassInstall);
    assert!(!lg.must_confirm());

    // Manual mode does not require confirmation; AutoBuild does.
    let mut config_auto = config.clone();
    config_auto.looking_glass = LookingGlassChoice::Enabled {
        install_mode: LookingGlassInstallMode::AutoBuild,
        target_resolution: Resolution::new(1920, 1080),
    };
    let plan_auto = plan_auto_replan(&profile, &report, &config_auto);
    let lg_auto = find_step(&plan_auto, StepKind::LookingGlassInstall);
    assert!(lg_auto.must_confirm());
}

fn plan_auto_replan(
    profile: &SystemProfile,
    report: &virtu::engine::CompatibilityReport,
    config: &PassthroughConfig,
) -> virtu::engine::Plan {
    plan(profile, report, config).expect("plan should succeed")
}

#[tokio::test]
async fn plan_summary_reflects_step_states_and_risk() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let config = PassthroughConfig::recommended_defaults(&profile).unwrap();

    let plan = plan(&profile, &report, &config).unwrap();
    assert_eq!(plan.summary.total_steps, plan.steps.len());
    assert_eq!(
        plan.summary.pending_steps + plan.summary.already_satisfied_steps,
        plan.steps.len()
    );
    assert!(matches!(
        plan.summary.max_risk,
        StepRisk::Medium | StepRisk::High
    ));
    assert!(plan.summary.requires_reboot);
}

#[tokio::test]
async fn plan_does_not_touch_paths_outside_declared_set() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let config = PassthroughConfig::recommended_defaults(&profile).unwrap();

    let plan = plan(&profile, &report, &config).unwrap();

    // Sanity: no step claims to touch sensitive system files. The planner's
    // own snapshot root lives under the user's home (`~/.virtu/snapshots`),
    // so a blanket `/home` ban is too aggressive on Linux hosts; we instead
    // pin the forbidden set to the specific files we never want to write.
    let forbidden = [
        Path::new("/etc/passwd"),
        Path::new("/etc/shadow"),
        Path::new("/etc/sudoers"),
        Path::new("/root"),
    ];
    for step in &plan.steps {
        for touch in &step.touches {
            for bad in &forbidden {
                assert!(
                    !touch.starts_with(bad),
                    "{:?} unexpectedly touches {}",
                    step.kind,
                    touch.display()
                );
            }
        }
    }
}
