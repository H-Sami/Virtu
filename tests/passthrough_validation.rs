//! Read-only validation tests for `PassthroughConfig` against fixture
//! `SystemProfile`s. These tests do not touch live host state and run on any
//! platform.

use std::path::PathBuf;

use virtu::detect::audio::AudioSystem;
use virtu::detect::bootloader::{self, BootloaderKind};
use virtu::detect::display_server::DisplayServer;
use virtu::detect::initramfs::InitramfsSystem;
use virtu::detect::readiness;
use virtu::detect::virtualization::VirtInfo;
use virtu::detect::{
    audio, cpu, display_manager, distro, gpu, initramfs, iommu, memory, monitors, storage, usb,
    SystemProfile,
};
use virtu::engine::build_compatibility_report;
use virtu::vm::{
    validate, AudioChoice, DiskChoice, DiskFormat, GpuPassthroughMode, GpuRole, LookingGlassChoice,
    LookingGlassInstallMode, MonitorPlan, NetworkChoice, PassthroughConfig, Resolution,
    SingleMonitorStrategy, ValidationIssueId, VmResources,
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

#[tokio::test]
async fn recommended_defaults_match_dual_gpu_fixture_profile() {
    let profile = fixture_profile().await;
    let config = PassthroughConfig::recommended_defaults(&profile).expect("config");

    // iGPU on host, dGPU passthrough.
    assert_eq!(config.gpu_mode, GpuPassthroughMode::IgpuHost);
    assert_eq!(config.gpu_roles.len(), 2);
    assert_eq!(
        config
            .gpu_roles
            .iter()
            .find(|role| role.pci_slot == "0000:00:02.0")
            .unwrap()
            .role,
        GpuRole::Host
    );
    assert_eq!(
        config
            .gpu_roles
            .iter()
            .find(|role| role.pci_slot == "0000:01:00.0")
            .unwrap()
            .role,
        GpuRole::Passthrough
    );

    // Two-monitor recommendation maps each GPU to its connected DRM connector.
    match &config.monitor_plan {
        MonitorPlan::TwoMonitors {
            host_connector,
            vm_connector,
        } => {
            assert_eq!(host_connector, "eDP-1");
            assert_eq!(vm_connector, "DP-1");
        }
        other => panic!("expected TwoMonitors, got {other:?}"),
    }

    // Two-monitor plan should not auto-enable Looking Glass.
    assert!(matches!(config.looking_glass, LookingGlassChoice::Disabled));

    // Resources fall in the expected envelope for the fixture host.
    assert_eq!(config.resources.ram_mb, 16_000);
    assert!(config.resources.vcpu_count >= 2);
    assert!(config.resources.vcpu_count < profile.cpu.logical_cores);

    match &config.resources.disk {
        DiskChoice::Create {
            path,
            size_gb,
            format,
        } => {
            assert!(path.starts_with(&profile.storage.default_vm_dir));
            assert_eq!(*size_gb, 100);
            assert_eq!(*format, DiskFormat::Qcow2);
            assert!(!path.exists(), "recommended disk path must not pre-exist");
        }
        other => panic!("expected Create, got {other:?}"),
    }

    assert!(matches!(config.audio, AudioChoice::HostAudio));
    assert!(matches!(config.network, NetworkChoice::Nat));
}

#[tokio::test]
async fn valid_recommended_config_passes_validation() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let config = PassthroughConfig::recommended_defaults(&profile).unwrap();

    let result = validate(&profile, &report, &config);

    assert!(result.is_ok(), "unexpected errors: {:?}", result.issues);
    assert!(!result.has_warnings());
}

#[tokio::test]
async fn compatibility_blockers_propagate_to_validation_report() {
    let mut profile = fixture_profile().await;
    profile.iommu_groups.clear();
    for gpu in &mut profile.gpus {
        gpu.iommu_isolated = false;
        gpu.iommu_group_id = None;
        gpu.vfio_compatible = false;
    }
    profile.bootloader.kind = BootloaderKind::Unknown;
    profile.bootloader.config_path = None;
    profile.initramfs_system = InitramfsSystem::Unknown;

    let report = build_compatibility_report(&profile);
    let mut config = PassthroughConfig::recommended_defaults(&profile).unwrap();
    // Force gpu_mode consistency since recommended_defaults is computed against
    // the broken profile too.
    config.gpu_mode = config.derived_mode(&profile).unwrap_or(config.gpu_mode);

    let result = validate(&profile, &report, &config);

    assert!(result.has_issue(ValidationIssueId::CompatibilityBlocked));
    assert!(result.has_errors());
}

#[tokio::test]
async fn unknown_pci_slot_in_role_assignment_is_error() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let mut config = PassthroughConfig::recommended_defaults(&profile).unwrap();
    config.gpu_roles[0].pci_slot = "0000:ff:ff.0".to_string();

    let result = validate(&profile, &report, &config);

    assert!(result.has_issue(ValidationIssueId::GpuRoleSlotUnknown));
    assert!(result.has_issue(ValidationIssueId::GpuRoleMissing));
}

#[tokio::test]
async fn duplicate_role_assignments_are_error() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let mut config = PassthroughConfig::recommended_defaults(&profile).unwrap();
    let dup = config.gpu_roles[0].clone();
    config.gpu_roles.push(dup);

    let result = validate(&profile, &report, &config);

    assert!(result.has_issue(ValidationIssueId::GpuRoleDuplicate));
}

#[tokio::test]
async fn detected_gpu_without_role_is_error() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let mut config = PassthroughConfig::recommended_defaults(&profile).unwrap();
    config.gpu_roles.pop();

    let result = validate(&profile, &report, &config);

    assert!(result.has_issue(ValidationIssueId::GpuRoleMissing));
}

#[tokio::test]
async fn passthrough_gpu_must_be_iommu_isolated() {
    let mut profile = fixture_profile().await;
    for gpu in &mut profile.gpus {
        if gpu.pci_slot == "0000:01:00.0" {
            gpu.iommu_isolated = false;
            gpu.vfio_compatible = false;
        }
    }
    let report = build_compatibility_report(&profile);
    let config = PassthroughConfig::recommended_defaults(&profile).unwrap();

    let result = validate(&profile, &report, &config);

    assert!(result.has_issue(ValidationIssueId::PassthroughGpuNotIsolated));
}

#[tokio::test]
async fn stated_mode_must_match_role_assignments() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let mut config = PassthroughConfig::recommended_defaults(&profile).unwrap();
    config.gpu_mode = GpuPassthroughMode::SingleGpu;

    let result = validate(&profile, &report, &config);

    assert!(result.has_issue(ValidationIssueId::GpuModeMismatch));
}

#[tokio::test]
async fn single_gpu_mode_warns_but_does_not_block() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);

    // User keeps a multi-GPU host but explicitly chooses single-GPU mode and
    // ignores the iGPU. Validation must respect that and only warn.
    let mut config = PassthroughConfig::recommended_defaults(&profile).unwrap();
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

    let result = validate(&profile, &report, &config);

    assert!(
        !result.has_errors(),
        "single-GPU should be allowed: {:?}",
        result.issues
    );
    assert!(result.has_issue(ValidationIssueId::SingleGpuRiskAcknowledged));
}

#[tokio::test]
async fn multi_gpu_passthrough_is_blocked_for_now() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let mut config = PassthroughConfig::recommended_defaults(&profile).unwrap();
    for role in &mut config.gpu_roles {
        role.role = GpuRole::Passthrough;
    }
    config.gpu_mode = GpuPassthroughMode::MultiGpu;

    let result = validate(&profile, &report, &config);

    assert!(result.has_issue(ValidationIssueId::MultiGpuNotImplemented));
}

#[tokio::test]
async fn two_monitor_plans_must_use_two_different_known_connectors() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let mut config = PassthroughConfig::recommended_defaults(&profile).unwrap();

    config.monitor_plan = MonitorPlan::TwoMonitors {
        host_connector: "DP-1".to_string(),
        vm_connector: "DP-1".to_string(),
    };
    let result = validate(&profile, &report, &config);
    assert!(result.has_issue(ValidationIssueId::MonitorConnectorsCollide));

    config.monitor_plan = MonitorPlan::TwoMonitors {
        host_connector: "DP-99".to_string(),
        vm_connector: "DP-1".to_string(),
    };
    let result = validate(&profile, &report, &config);
    assert!(result.has_issue(ValidationIssueId::MonitorConnectorUnknown));
}

#[tokio::test]
async fn hook_handoff_requires_single_gpu_mode() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let mut config = PassthroughConfig::recommended_defaults(&profile).unwrap();
    config.monitor_plan = MonitorPlan::OneMonitor {
        strategy: SingleMonitorStrategy::HookHandoff,
    };

    let result = validate(&profile, &report, &config);

    assert!(result.has_issue(ValidationIssueId::HookHandoffRequiresSingleGpu));
}

#[tokio::test]
async fn hook_handoff_with_unknown_display_manager_is_rejected_at_plan_time() {
    // Regression for the post-slice-10.8 audit (Finding A): the
    // hook generator (`config::writers::hooks::release_script`)
    // refuses Unknown / None display managers, but the refusal
    // surfaces inside Phase B — *after* Phase A has already
    // mutated the bootloader, initramfs, and VFIO modprobe. The
    // user is then left with VFIO bound but no working hooks.
    // Validation must catch this combination at plan time so
    // Phase A never runs against an impossible plan.
    let mut profile = fixture_profile().await;
    profile.display_manager = virtu::detect::display_manager::DisplayManager::Unknown;

    let report = build_compatibility_report(&profile);
    let mut config = PassthroughConfig::recommended_defaults(&profile).unwrap();
    // Force the single-GPU + hook-handoff combination the rule
    // gates on. The fixture profile has two GPUs, so we ignore the
    // host iGPU and switch to single-GPU mode.
    for role in &mut config.gpu_roles {
        if role.role == GpuRole::Host {
            role.role = GpuRole::Ignored;
        }
    }
    config.gpu_mode = GpuPassthroughMode::SingleGpu;
    config.monitor_plan = MonitorPlan::OneMonitor {
        strategy: SingleMonitorStrategy::HookHandoff,
    };

    let result = validate(&profile, &report, &config);

    assert!(
        result.has_issue(ValidationIssueId::HookHandoffRequiresKnownDisplayManager),
        "expected validation error for Unknown DM, got issues: {:?}",
        result.issues
    );
    assert!(
        result.has_errors(),
        "the rule must be error severity, not just a warning"
    );
}

#[tokio::test]
async fn hook_handoff_with_no_display_manager_is_rejected_at_plan_time() {
    // Companion to the Unknown case above. A TTY-only host with no
    // managed display manager service hits the same defense-in-
    // depth gap: Phase A would mutate the host, then Phase B would
    // refuse at HookInstall.
    let mut profile = fixture_profile().await;
    profile.display_manager = virtu::detect::display_manager::DisplayManager::None;

    let report = build_compatibility_report(&profile);
    let mut config = PassthroughConfig::recommended_defaults(&profile).unwrap();
    for role in &mut config.gpu_roles {
        if role.role == GpuRole::Host {
            role.role = GpuRole::Ignored;
        }
    }
    config.gpu_mode = GpuPassthroughMode::SingleGpu;
    config.monitor_plan = MonitorPlan::OneMonitor {
        strategy: SingleMonitorStrategy::HookHandoff,
    };

    let result = validate(&profile, &report, &config);

    assert!(result.has_issue(ValidationIssueId::HookHandoffRequiresKnownDisplayManager));
    assert!(result.has_errors());
}

#[tokio::test]
async fn hook_handoff_does_not_fire_when_strategy_is_switch_inputs() {
    // Defense-in-depth: the new rule must only gate on the
    // HookHandoff strategy. SwitchInputs and LookingGlassOnly
    // single-monitor plans don't install hooks, so they don't
    // care what the display manager is. The fixture profile has
    // SDDM by default, so we use Unknown here to make the absence
    // of the new error meaningful.
    let mut profile = fixture_profile().await;
    profile.display_manager = virtu::detect::display_manager::DisplayManager::Unknown;

    let report = build_compatibility_report(&profile);
    let mut config = PassthroughConfig::recommended_defaults(&profile).unwrap();
    for role in &mut config.gpu_roles {
        if role.role == GpuRole::Host {
            role.role = GpuRole::Ignored;
        }
    }
    config.gpu_mode = GpuPassthroughMode::SingleGpu;
    config.monitor_plan = MonitorPlan::OneMonitor {
        strategy: SingleMonitorStrategy::SwitchInputs,
    };

    let result = validate(&profile, &report, &config);

    assert!(
        !result.has_issue(ValidationIssueId::HookHandoffRequiresKnownDisplayManager),
        "rule must only fire when the strategy is HookHandoff; got: {:?}",
        result.issues
    );
}

#[tokio::test]
async fn looking_glass_requires_a_passthrough_gpu() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let mut config = PassthroughConfig::recommended_defaults(&profile).unwrap();
    for role in &mut config.gpu_roles {
        role.role = GpuRole::Ignored;
    }
    config.looking_glass = LookingGlassChoice::Enabled {
        install_mode: LookingGlassInstallMode::Manual,
        target_resolution: Resolution::new(1920, 1080),
    };

    let result = validate(&profile, &report, &config);

    assert!(result.has_issue(ValidationIssueId::LookingGlassRequiresPassthrough));
}

#[tokio::test]
async fn looking_glass_zero_resolution_is_invalid() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let mut config = PassthroughConfig::recommended_defaults(&profile).unwrap();
    config.looking_glass = LookingGlassChoice::Enabled {
        install_mode: LookingGlassInstallMode::Manual,
        target_resolution: Resolution::new(0, 0),
    };

    let result = validate(&profile, &report, &config);

    assert!(result.has_issue(ValidationIssueId::LookingGlassResolutionInvalid));
}

#[tokio::test]
async fn looking_glass_auto_build_warns_until_installer_ships() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let mut config = PassthroughConfig::recommended_defaults(&profile).unwrap();
    config.looking_glass = LookingGlassChoice::Enabled {
        install_mode: LookingGlassInstallMode::AutoBuild,
        target_resolution: Resolution::new(1920, 1080),
    };

    let result = validate(&profile, &report, &config);

    assert!(!result.has_errors(), "{:?}", result.issues);
    assert!(result.has_issue(ValidationIssueId::LookingGlassAutoBuildNotImplemented));
}

#[tokio::test]
async fn ram_and_vcpu_bounds_are_enforced() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let host_threads = profile.cpu.logical_cores;
    let host_total_mb = profile.ram.total_kb / 1024;

    let base = PassthroughConfig::recommended_defaults(&profile).unwrap();

    let mut too_little_ram = base.clone();
    too_little_ram.resources = VmResources {
        ram_mb: 512,
        ..base.resources.clone()
    };
    let result = validate(&profile, &report, &too_little_ram);
    assert!(result.has_issue(ValidationIssueId::RamTooSmall));

    let mut too_much_ram = base.clone();
    too_much_ram.resources = VmResources {
        ram_mb: host_total_mb + 8192,
        ..base.resources.clone()
    };
    let result = validate(&profile, &report, &too_much_ram);
    assert!(result.has_issue(ValidationIssueId::RamExceedsHost));

    let mut too_few_vcpu = base.clone();
    too_few_vcpu.resources = VmResources {
        vcpu_count: 1,
        ..base.resources.clone()
    };
    let result = validate(&profile, &report, &too_few_vcpu);
    assert!(result.has_issue(ValidationIssueId::VcpuTooLow));

    let mut too_many_vcpu = base.clone();
    too_many_vcpu.resources = VmResources {
        vcpu_count: host_threads,
        ..base.resources.clone()
    };
    let result = validate(&profile, &report, &too_many_vcpu);
    assert!(result.has_issue(ValidationIssueId::VcpuExceedsHost));
}

#[tokio::test]
async fn create_disk_path_must_not_already_exist() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let mut config = PassthroughConfig::recommended_defaults(&profile).unwrap();
    let existing = profile.storage.default_vm_dir.join(".available-bytes");
    assert!(existing.exists(), "fixture sanity check");
    config.resources.disk = DiskChoice::Create {
        path: existing,
        size_gb: 100,
        format: DiskFormat::Qcow2,
    };

    let result = validate(&profile, &report, &config);

    assert!(result.has_issue(ValidationIssueId::DiskPathExistsForCreate));
}

#[tokio::test]
async fn existing_disk_path_must_actually_exist() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let mut config = PassthroughConfig::recommended_defaults(&profile).unwrap();
    config.resources.disk = DiskChoice::Existing {
        path: profile
            .storage
            .default_vm_dir
            .join("definitely-not-a-real-disk.qcow2"),
    };

    let result = validate(&profile, &report, &config);

    assert!(result.has_issue(ValidationIssueId::DiskPathMissingForExisting));
}

#[tokio::test]
async fn host_audio_choice_requires_pipewire_or_pulseaudio_backend() {
    let mut profile = fixture_profile().await;
    profile.audio = AudioSystem::Unknown;
    let report = build_compatibility_report(&profile);
    let mut config = PassthroughConfig::recommended_defaults(&profile).unwrap();
    config.audio = AudioChoice::HostAudio;

    let result = validate(&profile, &report, &config);

    assert!(result.has_issue(ValidationIssueId::AudioBackendMissing));
}

#[tokio::test]
async fn bridge_network_requires_an_interface_name() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let mut config = PassthroughConfig::recommended_defaults(&profile).unwrap();
    config.network = NetworkChoice::Bridge {
        interface: "   ".to_string(),
    };

    let result = validate(&profile, &report, &config);

    assert!(result.has_issue(ValidationIssueId::NetworkInterfaceMissing));
}

#[tokio::test]
async fn duplicate_evdev_paths_are_rejected() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let mut config = PassthroughConfig::recommended_defaults(&profile).unwrap();

    let dup_path = profile.storage.default_vm_dir.join(".available-bytes");
    config.input.keyboard_evdev = Some(dup_path.clone());
    config.input.mouse_evdev = Some(dup_path);

    let result = validate(&profile, &report, &config);

    assert!(result.has_issue(ValidationIssueId::EvdevPathDuplicated));
}

#[tokio::test]
async fn missing_evdev_paths_emit_warning_only() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let mut config = PassthroughConfig::recommended_defaults(&profile).unwrap();
    config.input.keyboard_evdev = Some(PathBuf::from(
        "/this/path/should/not/exist/virtu-test-keyboard",
    ));

    let result = validate(&profile, &report, &config);

    assert!(!result.has_errors(), "{:?}", result.issues);
    assert!(result.has_issue(ValidationIssueId::EvdevPathUnknown));
}

// --- Edge-case regression tests for recommended-defaults math ---------------
//
// These assert that `recommended_defaults` itself never produces a config that
// would fail `validate`, even on small or unusual hosts. Older versions of
// `recommended_vm_ram_mb` and `recommend_vcpu_count` could panic or produce
// an instantly-invalid config on tiny hosts.

#[test]
fn recommended_vm_ram_handles_small_hosts_without_panicking() {
    use virtu::detect::memory::MemInfo;

    fn meminfo(total_kb: u64) -> MemInfo {
        MemInfo {
            total_kb,
            available_kb: total_kb,
            hugepage_size_kb: 2048,
            hugepages_total: 0,
            hugepages_free: 0,
        }
    }

    // Empty host -> 0, no panic.
    assert_eq!(meminfo(0).recommended_vm_ram_mb(), 0);
    // 4 GiB host -> reserve takes everything.
    assert_eq!(meminfo(4 * 1024 * 1024).recommended_vm_ram_mb(), 0);
    // 6 GiB host -> 2 GiB available for VM.
    assert_eq!(meminfo(6 * 1024 * 1024).recommended_vm_ram_mb(), 2048);
    // 8 GiB host -> half-of-host wins.
    assert_eq!(meminfo(8 * 1024 * 1024).recommended_vm_ram_mb(), 4096);
    // 32 GiB host -> half-of-host = 16 GiB.
    assert_eq!(meminfo(32 * 1024 * 1024).recommended_vm_ram_mb(), 16 * 1024);
}

#[tokio::test]
async fn recommended_defaults_pass_validation_on_low_thread_hosts() {
    // 4 logical threads is the smallest realistic host for passthrough; the
    // recommender must produce a config that validates cleanly there.
    let mut profile = fixture_profile().await;
    profile.cpu.physical_cores = 2;
    profile.cpu.logical_cores = 4;
    profile.cpu.has_hyperthreading = true;
    profile.cpu.core_to_threads.clear();
    profile.cpu.core_to_threads.insert(0, vec![0, 2]);
    profile.cpu.core_to_threads.insert(1, vec![1, 3]);

    let report = build_compatibility_report(&profile);
    let config = PassthroughConfig::recommended_defaults(&profile).unwrap();
    let result = validate(&profile, &report, &config);

    assert!(
        !result.has_errors(),
        "small-host defaults should not fail validation: {:?}",
        result.issues
    );
    assert!(config.resources.vcpu_count < profile.cpu.logical_cores);
}

#[tokio::test]
async fn vm_name_default_is_present_and_valid() {
    let profile = fixture_profile().await;
    let config = PassthroughConfig::recommended_defaults(&profile).expect("config");
    assert_eq!(config.vm_name, "virtu-windows");

    let report = build_compatibility_report(&profile);
    let result = validate(&profile, &report, &config);
    let vm_name_errors: Vec<_> = result
        .issues
        .iter()
        .filter(|i| {
            matches!(
                i.id,
                virtu::vm::ValidationIssueId::VmNameEmpty
                    | virtu::vm::ValidationIssueId::VmNameInvalidChars
                    | virtu::vm::ValidationIssueId::VmNameCollidesWithDomain
            )
        })
        .collect();
    assert!(
        vm_name_errors.is_empty(),
        "default vm_name must pass validation: {vm_name_errors:?}"
    );
}

#[tokio::test]
async fn vm_name_empty_is_rejected() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let mut config = PassthroughConfig::recommended_defaults(&profile).expect("config");
    config.vm_name = String::new();

    let result = validate(&profile, &report, &config);
    assert!(result
        .issues
        .iter()
        .any(|i| i.id == virtu::vm::ValidationIssueId::VmNameEmpty));
}

#[tokio::test]
async fn vm_name_invalid_characters_are_rejected() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let mut config = PassthroughConfig::recommended_defaults(&profile).expect("config");
    // Spaces, slashes, and unicode letters are all rejected by libvirt.
    for bad in ["my vm", "my/vm", "win10@home", "résumé", ""] {
        config.vm_name = bad.to_string();
        let result = validate(&profile, &report, &config);
        let hit = result.issues.iter().any(|i| {
            matches!(
                i.id,
                virtu::vm::ValidationIssueId::VmNameInvalidChars
                    | virtu::vm::ValidationIssueId::VmNameEmpty
            )
        });
        assert!(
            hit,
            "validate must reject vm_name {bad:?}; got issues: {:?}",
            result.issues
        );
    }
}

#[tokio::test]
async fn vm_name_collision_with_existing_libvirt_domain_is_rejected() {
    let mut profile = fixture_profile().await;
    profile
        .readiness
        .libvirt_domains
        .push(virtu::detect::readiness::LibvirtDomainInfo {
            id: None,
            name: "virtu-windows".to_string(),
            state: "shut off".to_string(),
        });

    let report = build_compatibility_report(&profile);

    // recommended_defaults should now skip "virtu-windows" and pick
    // "virtu-windows-2".
    let recommended = PassthroughConfig::recommended_defaults(&profile).expect("config");
    assert_eq!(recommended.vm_name, "virtu-windows-2");
    let recommended_result = validate(&profile, &report, &recommended);
    assert!(!recommended_result
        .issues
        .iter()
        .any(|i| i.id == virtu::vm::ValidationIssueId::VmNameCollidesWithDomain));

    // But a user-supplied colliding name must be rejected.
    let mut colliding = recommended.clone();
    colliding.vm_name = "virtu-windows".to_string();
    let result = validate(&profile, &report, &colliding);
    assert!(result
        .issues
        .iter()
        .any(|i| i.id == virtu::vm::ValidationIssueId::VmNameCollidesWithDomain));
}
