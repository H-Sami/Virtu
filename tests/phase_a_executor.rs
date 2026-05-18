//! Fixture-driven Phase-A executor tests (Milestone 6, slice 6.6).
//!
//! These tests run the entire Phase-A pipeline against an in-memory
//! filesystem: snapshot capture, bootloader rewrite, VFIO modprobe write,
//! initramfs rewrite, manifest persistence, and pending-plan persistence.
//! No real host filesystem is touched.

use std::path::{Path, PathBuf};

use virtu::detect::bootloader::{self, BootloaderKind};
use virtu::detect::display_server::DisplayServer;
use virtu::detect::initramfs::{self, InitramfsSystem};
use virtu::detect::readiness;
use virtu::detect::virtualization::VirtInfo;
use virtu::detect::{
    audio, cpu, display_manager, distro, gpu, iommu, memory, monitors, storage, usb, SystemProfile,
};
use virtu::engine::{build_compatibility_report, execute_phase_a, plan, StepKind};
use virtu::snapshot::{
    pending::DEFAULT_FILENAME as PENDING_FILENAME, FileSystem, MemoryFileSystem, PendingPlan,
    SnapshotManifest, MANIFEST_FILENAME,
};
use virtu::vm::PassthroughConfig;

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
                // Ensure modprobe.d exists; the file itself does not.
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

#[tokio::test]
async fn execute_phase_a_writes_grub_vfio_and_initramfs_and_pending_plan() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let config = PassthroughConfig::recommended_defaults(&profile).unwrap();
    let plan = plan(&profile, &report, &config).unwrap();

    // Sanity: the fixture profile chooses GRUB and mkinitcpio.
    assert_eq!(profile.bootloader.kind, BootloaderKind::Grub2);
    assert_eq!(profile.initramfs_system, InitramfsSystem::Mkinitcpio);

    let (fs, snapshots_root, state_root) = seed_filesystem_for_plan(&plan);

    let outcome = execute_phase_a(&plan, &profile, &config, &fs, &snapshots_root, &state_root)
        .expect("phase A should succeed against MemoryFileSystem");

    // Snapshot id is non-empty and points to a real manifest.
    assert!(!outcome.snapshot_id.is_empty());
    let manifest_path = snapshots_root
        .join(&outcome.snapshot_id)
        .join(MANIFEST_FILENAME);
    assert!(fs.exists(&manifest_path));

    // Bootloader file now contains the IOMMU and vfio-pci.ids params.
    let grub_path = plan
        .steps
        .iter()
        .find(|s| s.kind == StepKind::BootloaderWrite)
        .and_then(|s| s.touches.first())
        .cloned()
        .expect("bootloader step should have a target");
    let grub = String::from_utf8(fs.read(&grub_path).unwrap()).unwrap();
    assert!(grub.contains("intel_iommu=on"));
    assert!(grub.contains("iommu=pt"));
    assert!(grub.contains("vfio-pci.ids="));

    // VFIO modprobe file now exists with banner + softdeps.
    let vfio_path = plan
        .steps
        .iter()
        .find(|s| s.kind == StepKind::VfioConfig)
        .and_then(|s| s.touches.first())
        .cloned()
        .expect("vfio step should have a target");
    let vfio = String::from_utf8(fs.read(&vfio_path).unwrap()).unwrap();
    assert!(vfio.contains("# Managed by Virtu"));
    assert!(vfio.contains("options vfio-pci ids="));
    assert!(vfio.contains("softdep nvidia pre: vfio-pci"));

    // Initramfs file now contains the VFIO modules in MODULES=().
    let initramfs_path = plan
        .steps
        .iter()
        .find(|s| s.kind == StepKind::InitramfsWrite)
        .and_then(|s| s.touches.first())
        .cloned()
        .expect("initramfs step should have a target");
    let initramfs_str = String::from_utf8(fs.read(&initramfs_path).unwrap()).unwrap();
    assert!(initramfs_str.contains("MODULES=(vfio_pci vfio vfio_iommu_type1)"));

    // Manifest carries post-edit hashes for every Phase-A target.
    let manifest_bytes = fs.read(&manifest_path).unwrap();
    let manifest: SnapshotManifest =
        toml::from_str(&String::from_utf8(manifest_bytes).unwrap()).unwrap();
    for entry in &manifest.entries {
        if matches!(
            entry.produced_by,
            StepKind::BootloaderWrite | StepKind::VfioConfig | StepKind::InitramfsWrite
        ) {
            assert!(
                entry.post_edit_sha256.is_some(),
                "{} should have a post-edit hash",
                entry.original_path.display()
            );
        }
    }

    // PendingPlan was persisted with the right snapshot id and Phase-B
    // steps.
    let pending_path = state_root.join(PENDING_FILENAME);
    let pending_bytes = fs.read(&pending_path).unwrap();
    let pending: PendingPlan = toml::from_str(&String::from_utf8(pending_bytes).unwrap()).unwrap();
    assert_eq!(pending.snapshot_id, outcome.snapshot_id);
    let phase_b_kinds: Vec<_> = pending
        .remaining_steps
        .iter()
        .map(|s| s.kind.clone())
        .collect();
    for forbidden in [
        StepKind::Snapshot,
        StepKind::BootloaderWrite,
        StepKind::VfioConfig,
        StepKind::InitramfsWrite,
    ] {
        assert!(
            !phase_b_kinds.contains(&forbidden),
            "phase-B steps must not include {forbidden:?}"
        );
    }
}

#[tokio::test]
async fn execute_phase_a_is_idempotent_when_run_twice_against_same_fs() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let config = PassthroughConfig::recommended_defaults(&profile).unwrap();
    let plan = plan(&profile, &report, &config).unwrap();
    let (fs, snapshots_root, state_root) = seed_filesystem_for_plan(&plan);

    let first = execute_phase_a(&plan, &profile, &config, &fs, &snapshots_root, &state_root)
        .expect("first phase A should succeed");

    // Snapshot the post-Phase-A grub bytes so we can compare after second
    // run.
    let grub_path = plan
        .steps
        .iter()
        .find(|s| s.kind == StepKind::BootloaderWrite)
        .and_then(|s| s.touches.first())
        .cloned()
        .unwrap();
    let after_first = fs.read(&grub_path).unwrap();

    // Remove the pending plan so the second invocation isn't blocked by the
    // run_apply guard. (The executor itself doesn't enforce the guard;
    // run_apply does. Direct calls into execute_phase_a are still
    // idempotent on the host files themselves.)
    let _ = fs.remove_file(&state_root.join(PENDING_FILENAME));

    // Sub-second sleep to make sure the second snapshot id differs from
    // the first; manifest ids have millisecond precision.
    std::thread::sleep(std::time::Duration::from_millis(2));

    let second = execute_phase_a(&plan, &profile, &config, &fs, &snapshots_root, &state_root)
        .expect("second phase A should succeed");

    // The host bytes must be unchanged after the second run.
    assert_ne!(first.snapshot_id, second.snapshot_id);
    let after_second = fs.read(&grub_path).unwrap();
    assert_eq!(
        after_first, after_second,
        "second Phase A must be a no-op on host bytes"
    );
}

#[test]
fn writers_module_is_reachable_through_public_api() {
    // Smoke test: the writers are public so external integration tests can
    // call them directly.
    use virtu::config::writers::grub::rewrite_grub_default;
    let out = rewrite_grub_default(
        "GRUB_CMDLINE_LINUX_DEFAULT=\"quiet\"\n",
        &["intel_iommu=on".to_string()],
    )
    .unwrap();
    assert!(out.contains("intel_iommu=on"));
}

#[test]
fn fixture_helper_path_resolves_against_manifest_dir() {
    // Quick sanity check that the fixture helper does not silently look at
    // CWD. If it ever did, the failure mode would be very confusing.
    let path = fixture("etc/os-release-arch");
    assert!(path.is_absolute());
    assert!(path.ends_with("tests/fixtures/etc/os-release-arch"));
}

// Use `Path` via prelude in case future test cases need it.
#[allow(dead_code)]
fn _path_marker() -> &'static Path {
    Path::new("/")
}
