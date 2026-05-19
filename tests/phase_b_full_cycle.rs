//! End-to-end Phase A → reboot simulation → Phase B coverage (slice 7.7).
//!
//! These tests exercise the full apply → resume cycle against the
//! `MemoryFileSystem`. Phase A captures the snapshot, edits the
//! bootloader, writes the VFIO modprobe snippet, and rewrites the
//! initramfs config. We then synthesize a `SystemProfile` that matches
//! what the host would look like after rebooting into the new cmdline,
//! feed it to `verify_phase_a_landed`, and run Phase B in
//! `HostCommandMode::Skip` (so `qemu-img` and `virsh` are not invoked).
//!
//! What we assert:
//!
//! - The verifier returns `Ready` for a post-reboot profile that
//!   reflects every Phase A change.
//! - Phase B writes the libvirt domain XML to `~/.virtu/<vm_name>.xml`
//!   via `snapshot_then_write` so the manifest knows about it.
//! - The manifest gains a `VmXmlGenerate` entry with a post-edit hash.
//! - When the user picked `DiskChoice::Create`, the manifest also gains
//!   a `VmRegister` entry for the disk path even though no qemu-img ran.
//! - Phase B clears `pending.toml`.
//!
//! No real virsh / qemu-img / virt-xml-validate is invoked. The
//! `validate_xml` wrapper is gated behind `HostCommandMode::Run`; with
//! `Skip` it is bypassed. A separate Linux-only smoke test in
//! `src/config/writers/commands.rs` covers the real-host validator.

use std::path::PathBuf;

use virtu::detect::bootloader::{self, BootloaderKind};
use virtu::detect::display_server::DisplayServer;
use virtu::detect::initramfs::{self, InitramfsSystem};
use virtu::detect::readiness::{self, KernelHeadersInfo};
use virtu::detect::virtualization::VirtInfo;
use virtu::detect::{
    audio, cpu, display_manager, distro, gpu, iommu, memory, monitors, storage, usb, SystemProfile,
};
use virtu::engine::{
    build_compatibility_report, execute_phase_a, execute_phase_b, plan, verify_phase_a_landed,
    HostCommandMode, RegenerateMode, ResumeReadiness, StepKind,
};
use virtu::snapshot::{
    pending::DEFAULT_FILENAME as PENDING_FILENAME, FileSystem, MemoryFileSystem, PendingPlan,
    RestoreAction, SnapshotManifest, MANIFEST_FILENAME,
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
/// stays identical (same distro, kernel, bootloader, initramfs) but the
/// kernel cmdline and loaded modules now reflect the values Phase A
/// wrote to the bootloader config and modprobe snippet.
fn simulate_post_reboot_profile(
    pre_reboot: &SystemProfile,
    pending: &PendingPlan,
) -> SystemProfile {
    let mut profile = pre_reboot.clone();

    // 1. Kernel cmdline: include intel_iommu=on / amd_iommu=on, iommu=pt,
    //    and the vfio-pci.ids list the planner declared.
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

    // 2. The vfio_pci module is now loaded.
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

    // 3. Bind every passthrough GPU to vfio-pci (mirrors what
    //    /sys/bus/pci/devices/<slot>/driver would show after a successful
    //    reboot).
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

    // 4. Make sure kernel headers and OVMF still look fine — Phase B
    //    does not depend on these for `Ready`, but a real host would
    //    have them.
    if profile.readiness.kernel_headers.path.is_none() {
        profile.readiness.kernel_headers = KernelHeadersInfo {
            present: true,
            path: Some(PathBuf::from("/usr/lib/modules/6.10.0/build")),
        };
    }

    profile
}

#[tokio::test]
async fn full_cycle_phase_a_then_phase_b_writes_xml_and_records_manifest_entry() {
    // 1. Build the plan and seed the in-memory FS.
    let profile = fixture_profile().await;
    let report = build_compatibility_report(&profile);
    let mut config = PassthroughConfig::recommended_defaults(&profile).unwrap();
    config.vm_name = "virtu-fullcycle".to_string();
    let plan = plan(&profile, &report, &config).unwrap();

    assert_eq!(profile.bootloader.kind, BootloaderKind::Grub2);
    assert_eq!(profile.initramfs_system, InitramfsSystem::Mkinitcpio);

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
    .expect("phase A succeeds");

    // 3. Read the persisted PendingPlan back so Phase B uses the on-disk
    //    record exactly as the CLI would.
    let pending_path = state_root.join(PENDING_FILENAME);
    let pending_bytes = fs.read(&pending_path).unwrap();
    let pending: PendingPlan = toml::from_str(&String::from_utf8(pending_bytes).unwrap()).unwrap();
    assert_eq!(pending.snapshot_id, phase_a.snapshot_id);

    // 4. Simulate the reboot: synthesize a post-reboot profile and run
    //    the verifier. It must report Ready.
    let post_reboot = simulate_post_reboot_profile(&profile, &pending);
    let readiness = verify_phase_a_landed(&post_reboot, &pending);
    match readiness {
        ResumeReadiness::Ready => {}
        other => {
            panic!("verifier must report Ready for a fresh post-reboot profile, got {other:?}")
        }
    }

    // 5. Run Phase B in Skip mode so virt-xml-validate / qemu-img /
    //    virsh do not run.
    let phase_b = execute_phase_b(
        &pending,
        &post_reboot,
        &fs,
        &snapshots_root,
        &state_root,
        HostCommandMode::Skip,
    )
    .expect("phase B succeeds");

    // 6. The XML file lives on disk under ~/.virtu/<vm_name>.xml as the
    //    planner declared (the path is keyed on vm_name; we therefore
    //    look it up through the manifest rather than hard-coding a
    //    `$HOME` value).
    let manifest_path = snapshots_root
        .join(&phase_a.snapshot_id)
        .join(MANIFEST_FILENAME);
    let manifest_bytes = fs.read(&manifest_path).unwrap();
    let manifest: SnapshotManifest =
        toml::from_str(&String::from_utf8(manifest_bytes).unwrap()).unwrap();

    let xml_entry = manifest
        .entries
        .iter()
        .find(|entry| entry.produced_by == StepKind::VmXmlGenerate)
        .expect("manifest must record the VmXmlGenerate write");
    assert!(!xml_entry.original_existed);
    assert!(xml_entry.post_edit_sha256.is_some());

    let xml = String::from_utf8(fs.read(&xml_entry.original_path).unwrap()).unwrap();
    assert!(xml.contains("<domain type='kvm'"));
    assert!(xml.contains("<name>virtu-fullcycle</name>"));
    assert!(xml.contains("<hostdev mode='subsystem' type='pci' managed='yes'>"));
    // No Looking Glass shmem block. Ever.
    assert!(!xml.contains("<shmem name='looking-glass'>"));
    assert!(!xml.contains("ivshmem"));

    // 7. Phase B reports VmXmlGenerate and VmRegister as completed,
    //    Verify as completed (it's read-only). HookInstall (M9) and
    //    LookingGlassInstall (cut from v1.0) only appear in the plan
    //    when the user explicitly opts in; for the recommended-defaults
    //    fixture profile neither step is emitted, so we assert they are
    //    not in `completed_steps` either.
    assert!(phase_b.completed_steps.contains(&StepKind::VmXmlGenerate));
    assert!(phase_b.completed_steps.contains(&StepKind::VmRegister));
    assert!(phase_b.completed_steps.contains(&StepKind::Verify));
    assert!(!phase_b.completed_steps.contains(&StepKind::HookInstall));
    assert!(!phase_b
        .completed_steps
        .contains(&StepKind::LookingGlassInstall));

    // 8. The pending record is gone.
    assert!(!fs.exists(&pending_path));
    assert!(phase_b.pending_cleared);

    // 9. The manifest also picked up the disk-image entry for the
    //    new qcow2 (DiskChoice::Create). qemu-img did not run (Skip
    //    mode) so no actual file exists at the disk path, but the
    //    manifest must declare it so a later rollback knows to delete
    //    it if it ever lands on disk.
    let disk_entry = manifest
        .entries
        .iter()
        .find(|entry| entry.produced_by == StepKind::VmRegister)
        .expect("manifest must declare the disk image for VmRegister");
    assert!(!disk_entry.original_existed);

    // 10. In Skip mode, virsh define did not run, so no
    //     UndefineLibvirtDomain restore action is appended. We pin
    //     this so a future change cannot silently start advising the
    //     user to undefine a domain that does not exist.
    assert!(!manifest
        .restore_actions
        .iter()
        .any(|a| matches!(a, RestoreAction::UndefineLibvirtDomain { .. })));
}
