//! Fixture-driven snapshot tests (slice 5.8).
//!
//! All tests use the in-memory [`MemoryFileSystem`] so cargo test does not
//! touch the real host filesystem.

use std::path::{Path, PathBuf};

use virtu::config::atomic_write::snapshot_then_write;
use virtu::detect::bootloader::{self, BootloaderKind};
use virtu::detect::display_server::DisplayServer;
use virtu::detect::initramfs::InitramfsSystem;
use virtu::detect::readiness;
use virtu::detect::virtualization::VirtInfo;
use virtu::detect::{
    audio, cpu, display_manager, distro, gpu, initramfs, iommu, memory, monitors, storage, usb,
    SystemProfile,
};
use virtu::engine::{build_compatibility_report, execute_snapshot_step, plan, StepKind};
use virtu::snapshot::{
    capture, capture_with_id, restore, FileSystem, MemoryFileSystem, RestoreAction, SnapshotEntry,
    SnapshotError, SnapshotManifest, FILES_SUBDIR, MANIFEST_FILENAME,
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

/// Capture a fixture-driven plan into a memory filesystem and return all the
/// pieces needed by individual tests.
async fn capture_fixture_plan() -> (
    MemoryFileSystem,
    PathBuf, // snapshots_root
    String,  // snapshot id
    SnapshotManifest,
) {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let config = PassthroughConfig::recommended_defaults(&profile).unwrap();
    let plan = plan(&profile, &report, &config).unwrap();

    let fs = MemoryFileSystem::new();
    // Seed each touched path with predictable original bytes so capture has
    // something to hash and copy. The snapshot step's own touch is skipped
    // by capture (it's the rollback baseline) so we don't seed it here.
    for step in &plan.steps {
        if step.kind == StepKind::Snapshot {
            continue;
        }
        for touch in &step.touches {
            if let Some(parent) = touch.parent() {
                if !parent.as_os_str().is_empty() {
                    fs.create_dir_all(parent).unwrap();
                }
            }
            // Half the touches are for files that don't exist yet (e.g.
            // /etc/modprobe.d/virtu-vfio.conf). Only seed when we want to
            // exercise the "file existed" path.
            if matches!(
                step.kind,
                StepKind::BootloaderWrite | StepKind::InitramfsWrite | StepKind::VmXmlGenerate
            ) {
                let seed = format!("seed-for-{}\n", touch.display());
                fs.write_atomic(touch, seed.as_bytes()).unwrap();
            }
        }
    }

    let snapshots_root = PathBuf::from("/tmp/virtu/snapshots");
    fs.create_dir_all(&snapshots_root).unwrap();

    let id = capture_with_id(&plan, &profile, &fs, &snapshots_root, "test-snapshot-id").unwrap();
    let manifest_path = snapshots_root.join(&id).join(MANIFEST_FILENAME);
    let bytes = fs.read(&manifest_path).unwrap();
    let manifest: SnapshotManifest = toml::from_str(&String::from_utf8(bytes).unwrap()).unwrap();
    (fs, snapshots_root, id, manifest)
}

#[tokio::test]
async fn capture_records_one_entry_per_touched_path_with_pre_hash() {
    let (fs, snapshots_root, id, manifest) = capture_fixture_plan().await;

    // Snapshot step is intentionally skipped, so its touches must not appear.
    for entry in &manifest.entries {
        assert_ne!(entry.produced_by, StepKind::Snapshot);
    }

    // Files that we seeded must have a non-empty pre-edit hash and a backup
    // copy under files/.
    let bootloader_target = manifest
        .entries
        .iter()
        .find(|e| e.produced_by == StepKind::BootloaderWrite)
        .expect("bootloader entry");
    assert!(bootloader_target.original_existed);
    assert_eq!(bootloader_target.pre_edit_sha256.len(), 64);
    let backup_absolute = snapshots_root
        .join(&id)
        .join(&bootloader_target.backup_path);
    assert_eq!(
        fs.read(&backup_absolute).unwrap(),
        format!("seed-for-{}\n", bootloader_target.original_path.display()).into_bytes()
    );

    // Files we did NOT seed must be flagged as not-yet-existing.
    if let Some(modprobe_entry) = manifest
        .entries
        .iter()
        .find(|e| e.produced_by == StepKind::VfioConfig)
    {
        assert!(!modprobe_entry.original_existed);
        assert!(modprobe_entry.pre_edit_sha256.is_empty());
    }

    // The manifest TOML lives inside the snapshot directory under FILES_SUBDIR's parent.
    assert!(fs.exists(&snapshots_root.join(&id).join(MANIFEST_FILENAME)));
    let _ = FILES_SUBDIR; // silence unused import lint
}

#[tokio::test]
async fn restore_rewrites_each_touched_path_back_to_pre_edit_bytes() {
    let (fs, snapshots_root, id, manifest) = capture_fixture_plan().await;

    // Mutate every backed-up file so restore has work to do.
    for entry in &manifest.entries {
        if entry.original_existed {
            fs.write_atomic(&entry.original_path, b"externally-mutated")
                .unwrap();
        }
    }

    let summary = restore(&id, &fs, &snapshots_root).unwrap();
    assert_eq!(summary.id, id);
    assert!(!summary.restored.is_empty());

    for entry in &manifest.entries {
        if entry.original_existed {
            let restored_bytes = fs.read(&entry.original_path).unwrap();
            assert_eq!(
                format!("seed-for-{}\n", entry.original_path.display()).into_bytes(),
                restored_bytes,
                "{} should be restored to pre-edit content",
                entry.original_path.display()
            );
        }
    }
}

#[tokio::test]
async fn snapshot_then_write_records_pre_and_post_hashes() {
    let (fs, _snapshots_root, _id, mut manifest) = capture_fixture_plan().await;

    // Mutate the bootloader config through the snapshot-aware primitive.
    let target = manifest
        .entries
        .iter()
        .find(|e| e.produced_by == StepKind::BootloaderWrite)
        .expect("bootloader entry")
        .original_path
        .clone();

    snapshot_then_write(&mut manifest, &fs, &target, b"new-bootloader-content").unwrap();

    let entry = manifest.entry_for(&target).unwrap();
    assert_eq!(entry.pre_edit_sha256.len(), 64);
    let post = entry.post_edit_sha256.clone().unwrap();
    assert_eq!(post.len(), 64);
    assert_ne!(post, entry.pre_edit_sha256);
    assert_eq!(fs.read(&target).unwrap(), b"new-bootloader-content");
}

#[tokio::test]
async fn restore_is_idempotent() {
    let (fs, snapshots_root, id, _manifest) = capture_fixture_plan().await;

    // First restore.
    let first = restore(&id, &fs, &snapshots_root).unwrap();
    assert!(first.id == id);

    // Second restore against the same filesystem must not error and must
    // report every entry as already-at-pre-edit (or already-absent for the
    // entries that didn't exist at capture time).
    let second = restore(&id, &fs, &snapshots_root).unwrap();
    assert!(second.restored.is_empty());
    assert!(!second.already_at_pre_edit.is_empty() || !second.deleted_or_already_absent.is_empty());
}

#[tokio::test]
async fn shared_basenames_get_distinct_backup_paths_and_round_trip() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let config = PassthroughConfig::recommended_defaults(&profile).unwrap();
    let plan = plan(&profile, &report, &config).unwrap();

    let fs = MemoryFileSystem::new();
    fs.create_dir_all(Path::new("/etc/foo")).unwrap();
    fs.create_dir_all(Path::new("/etc/baz")).unwrap();
    fs.write_atomic(Path::new("/etc/foo/bar.conf"), b"foo")
        .unwrap();
    fs.write_atomic(Path::new("/etc/baz/bar.conf"), b"baz")
        .unwrap();

    let snapshots_root = PathBuf::from("/snap");
    fs.create_dir_all(&snapshots_root).unwrap();

    // Hand-craft a tiny plan whose two touches share a basename. We reuse
    // the captured plan's structure so derive_restore_actions has something
    // to chew on, then push the two synthetic targets in.
    let mut tweaked = plan.clone();
    if let Some(step) = tweaked
        .steps
        .iter_mut()
        .find(|s| s.kind == StepKind::BootloaderWrite)
    {
        step.touches = vec![
            PathBuf::from("/etc/foo/bar.conf"),
            PathBuf::from("/etc/baz/bar.conf"),
        ];
    }

    let id = capture(&tweaked, &profile, &fs, &snapshots_root).unwrap();
    let manifest_path = snapshots_root.join(&id).join(MANIFEST_FILENAME);
    let raw = fs.read(&manifest_path).unwrap();
    let manifest: SnapshotManifest = toml::from_str(&String::from_utf8(raw).unwrap()).unwrap();

    // Both files appear with distinct backup paths.
    let foo = manifest
        .entries
        .iter()
        .find(|e| e.original_path == Path::new("/etc/foo/bar.conf"))
        .expect("foo entry");
    let baz = manifest
        .entries
        .iter()
        .find(|e| e.original_path == Path::new("/etc/baz/bar.conf"))
        .expect("baz entry");
    assert_ne!(foo.backup_path, baz.backup_path);

    // Mutate both, then round-trip.
    fs.write_atomic(Path::new("/etc/foo/bar.conf"), b"mutated-foo")
        .unwrap();
    fs.write_atomic(Path::new("/etc/baz/bar.conf"), b"mutated-baz")
        .unwrap();
    restore(&id, &fs, &snapshots_root).unwrap();
    assert_eq!(fs.read(Path::new("/etc/foo/bar.conf")).unwrap(), b"foo");
    assert_eq!(fs.read(Path::new("/etc/baz/bar.conf")).unwrap(), b"baz");
}

#[tokio::test]
async fn manifest_round_trips_through_toml_with_full_plan() {
    let (fs, snapshots_root, id, _) = capture_fixture_plan().await;
    let raw = fs
        .read(&snapshots_root.join(&id).join(MANIFEST_FILENAME))
        .unwrap();
    let parsed: SnapshotManifest =
        toml::from_str(&String::from_utf8(raw.clone()).unwrap()).unwrap();
    let reserialized = toml::to_string(&parsed).unwrap();
    let reparsed: SnapshotManifest = toml::from_str(&reserialized).unwrap();
    assert_eq!(parsed, reparsed);
    assert_eq!(parsed.id, id);
}

#[test]
fn restore_rejects_missing_manifest() {
    let fs = MemoryFileSystem::new();
    let snapshots_root = PathBuf::from("/snapshots");
    fs.create_dir_all(&snapshots_root).unwrap();
    let err = restore("does-not-exist", &fs, &snapshots_root).unwrap_err();
    matches!(err, SnapshotError::MissingManifest { .. });
}

#[tokio::test]
async fn execute_snapshot_step_returns_id_and_manifest() {
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let config = PassthroughConfig::recommended_defaults(&profile).unwrap();
    let plan = plan(&profile, &report, &config).unwrap();

    let fs = MemoryFileSystem::new();
    let snapshots_root = PathBuf::from("/snap");
    fs.create_dir_all(&snapshots_root).unwrap();
    // Seed at least one expected target so capture has something concrete.
    if let Some(target) = plan
        .steps
        .iter()
        .find(|s| s.kind == StepKind::BootloaderWrite)
        .and_then(|s| s.touches.first())
    {
        if let Some(parent) = target.parent() {
            fs.create_dir_all(parent).unwrap();
        }
        fs.write_atomic(target, b"seed").unwrap();
    }

    let (id, manifest) = execute_snapshot_step(&plan, &profile, &fs, &snapshots_root).unwrap();
    assert!(!id.is_empty());
    assert_eq!(manifest.id, id);
    // Restore actions are derived deterministically from the plan.
    assert!(matches!(
        manifest.restore_actions.last(),
        Some(RestoreAction::RecommendReboot) | None
    ));
    let _ = SnapshotEntry {
        // silence unused import lint
        original_path: PathBuf::new(),
        backup_path: PathBuf::new(),
        pre_edit_sha256: String::new(),
        post_edit_sha256: None,
        original_existed: false,
        produced_by: StepKind::Verify,
    };
    let _ = BootloaderKind::Unknown;
    let _ = InitramfsSystem::Unknown;
}
