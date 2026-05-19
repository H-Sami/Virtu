//! Self-test for `tests/scripts/capture_fixture.sh` (slice 10.8).
//!
//! Runs against a fixture the user captured locally. The script
//! produces a sanitized tree under `tests/fixtures/<name>/`; this
//! test confirms the resulting tree is loadable by every
//! `*_from_root` parser entry point, so a future maintainer can
//! drop a captured fixture into `tests/fixtures/` and immediately
//! write hermetic regression tests against it.
//!
//! Gated by `VIRTU_RUN_CAPTURE_FIXTURE_SMOKE=<fixture-name>` so
//! the normal `cargo test` run stays hermetic. Skips when the
//! variable is unset or the named fixture is missing.
//!
//! Typical workflow:
//!
//! ```bash
//! tests/scripts/capture_fixture.sh nvidia-amd-cachyos-grub
//! VIRTU_RUN_CAPTURE_FIXTURE_SMOKE=nvidia-amd-cachyos-grub \
//!     cargo test --test capture_fixture_smoke
//! ```
//!
//! On success the test prints a summary of what each parser
//! found. Failures here mean the capture script's output does
//! not match what the parsers expect, which is a slice 10.8
//! regression worth investigating before checking the fixture
//! into the repo.

use std::path::PathBuf;

use virtu::detect::{
    audio, bootloader, cpu, display_manager, distro, gpu, initramfs, iommu, memory, monitors,
    readiness, storage,
};

fn fixture_dir() -> Option<PathBuf> {
    let name = std::env::var("VIRTU_RUN_CAPTURE_FIXTURE_SMOKE").ok()?;
    if name.is_empty() {
        return None;
    }
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(&name);
    if !dir.is_dir() {
        eprintln!(
            "VIRTU_RUN_CAPTURE_FIXTURE_SMOKE=`{name}` was set, but \
             {} does not exist; skipping.",
            dir.display()
        );
        return None;
    }
    Some(dir)
}

#[tokio::test]
async fn captured_fixture_is_loadable_by_every_from_root_parser() {
    let Some(root) = fixture_dir() else {
        // No fixture name supplied; run as a no-op so a normal
        // `cargo test` still passes.
        return;
    };

    println!(
        "\nValidating captured fixture against the *_from_root parsers:\n  {}\n",
        root.display()
    );

    // /proc — CPU, memory, kernel cmdline.
    let cpuinfo = std::fs::read_to_string(root.join("proc/cpuinfo"))
        .expect("captured fixture must contain proc/cpuinfo");
    let cpu_info = cpu::parse_cpuinfo(&cpuinfo, true, Vec::new());
    println!(
        "  cpu       : {} {} ({}p / {}t, IOMMU-capable={})",
        cpu_info.vendor,
        cpu_info.model_name,
        cpu_info.physical_cores,
        cpu_info.logical_cores,
        cpu_info.iommu_capable,
    );
    assert!(!cpu_info.vendor.is_empty(), "cpu vendor must be parseable");
    assert!(cpu_info.physical_cores > 0, "physical core count > 0");

    let meminfo = std::fs::read_to_string(root.join("proc/meminfo"))
        .expect("captured fixture must contain proc/meminfo");
    let mem_info = memory::parse_meminfo(&meminfo);
    println!("  memory    : {} GiB total", mem_info.total_gb());
    assert!(
        mem_info.total_gb() > 0,
        "memory must parse a positive total"
    );

    // /etc/os-release — distro identity.
    let os_release = std::fs::read_to_string(root.join("etc/os-release"))
        .expect("captured fixture must contain etc/os-release");
    let distro_info = distro::parse_distro_info(&os_release);
    println!(
        "  distro    : {} ({}); pkg_mgr={:?}",
        distro_info.pretty_name, distro_info.id, distro_info.package_manager,
    );

    // Bootloader — GRUB2 or systemd-boot. Both are accepted; we
    // only require that some bootloader is detected.
    let bootloader_info = bootloader::detect_from_root(&root, false)
        .await
        .expect("detect_from_root must not error");
    println!(
        "  bootloader: {:?} at {:?}",
        bootloader_info.kind,
        bootloader_info
            .config_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
    );
    assert_ne!(
        bootloader_info.kind,
        bootloader::BootloaderKind::Unknown,
        "captured fixture must produce a known bootloader; \
         was the host using rEFInd / EFISTUB / Syslinux?"
    );

    // Initramfs.
    let initramfs_kind = initramfs::detect_from_root(&root, false)
        .await
        .expect("initramfs detect_from_root must not error");
    println!("  initramfs : {initramfs_kind:?}");
    assert_ne!(
        initramfs_kind,
        initramfs::InitramfsSystem::Unknown,
        "captured fixture must produce a known initramfs system",
    );

    // /sys/bus/pci — GPU + companion audio.
    let gpus = gpu::detect_all_from_sysfs_root(root.join("sys"))
        .await
        .expect("gpu detect_all_from_sysfs_root must not error");
    println!("  gpus      : {} detected", gpus.len());
    for g in &gpus {
        println!(
            "    - {} {} [{}:{}] driver={:?} boot_vga={}",
            g.pci_slot, g.model_name, g.vendor_id, g.device_id, g.current_driver, g.is_boot_vga,
        );
    }
    assert!(
        !gpus.is_empty(),
        "captured fixture must expose at least one display-class \
         PCI device under sys/bus/pci/devices/",
    );

    // /sys/kernel/iommu_groups.
    let groups = iommu::detect_groups_from_sysfs_root(root.join("sys"))
        .await
        .expect("iommu detect_groups_from_sysfs_root must not error");
    println!("  iommu     : {} groups", groups.len());

    // /sys/class/drm — monitors.
    let drm_root = root.join("sys/class/drm");
    let monitors = monitors::detect_from_drm_root(&drm_root)
        .await
        .expect("monitors detect_from_drm_root must not error");
    let connected = monitors.iter().filter(|m| m.connected).count();
    println!(
        "  monitors  : {} connectors total ({} connected)",
        monitors.len(),
        connected
    );

    // Display manager.
    let dm = display_manager::detect_from_root(&root)
        .await
        .expect("display_manager detect_from_root must not error");
    println!("  dm        : {dm:?}");

    // Audio (best-effort; not every host has a fixture-recognized
    // socket layout).
    let audio_kind = audio::detect_from_root(&root)
        .await
        .expect("audio detect_from_root must not error");
    println!("  audio     : {audio_kind:?}");

    // Storage (best-effort).
    let storage_info = storage::detect_from_root(&root, false)
        .await
        .expect("storage detect_from_root must not error");
    println!(
        "  storage   : {} GiB available in {}",
        storage_info.available_gb(),
        storage_info.default_vm_dir.display(),
    );

    // Readiness — kernel headers, OVMF, libvirt domains, user
    // groups.
    let readiness_info = readiness::detect_from_root(&root)
        .await
        .expect("readiness detect_from_root must not error");
    println!(
        "  readiness : kernel={} headers_present={} ovmf_available={} libvirt_domains={}",
        readiness_info.kernel_version,
        readiness_info.kernel_headers.present,
        readiness_info.ovmf.available(),
        readiness_info.libvirt_domains.len(),
    );

    println!("\nFixture validates against every detector entry point.\n");
}
