use std::path::PathBuf;
use virtu::detect::audio::AudioSystem;
use virtu::detect::bootloader::{self, BootloaderKind};
use virtu::detect::display_manager::DisplayManager;
use virtu::detect::distro::{self, DistroFamily, PackageManager};
use virtu::detect::gpu::{GpuType, GpuVendor};
use virtu::detect::initramfs::InitramfsSystem;
use virtu::detect::usb::UsbDeviceClass;
use virtu::detect::{
    audio, cpu, display_manager, gpu, initramfs, iommu, memory, monitors, readiness, storage, usb,
};

fn fixture(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(path)
}

#[test]
fn parses_cpuinfo_with_repeated_core_ids_across_sockets() {
    let cpuinfo = std::fs::read_to_string(fixture("proc/cpuinfo-intel-2socket-ht")).unwrap();
    let parsed = cpu::parse_cpuinfo(&cpuinfo, true, Vec::new());

    assert_eq!(parsed.vendor, "GenuineIntel");
    assert_eq!(parsed.model_name, "Intel(R) Xeon(R) Test CPU");
    assert_eq!(parsed.physical_cores, 4);
    assert_eq!(parsed.logical_cores, 8);
    assert!(parsed.has_hyperthreading);
    assert!(parsed.iommu_capable);
    assert!(parsed.iommu_enabled);
    assert_eq!(parsed.core_to_threads.get(&0), Some(&vec![0, 4]));
    assert_eq!(parsed.core_to_threads.get(&3), Some(&vec![3, 7]));
}

#[test]
fn parses_meminfo_without_sysfs_hugepage_probe() {
    let meminfo = std::fs::read_to_string(fixture("proc/meminfo-basic")).unwrap();
    let parsed = memory::parse_meminfo(&meminfo);

    assert_eq!(parsed.total_kb, 32_768_000);
    assert_eq!(parsed.available_kb, 20_480_000);
    assert_eq!(parsed.hugepage_size_kb, 2048);
    assert_eq!(parsed.hugepages_total, 64);
    assert_eq!(parsed.hugepages_free, 32);
    assert_eq!(parsed.recommended_vm_ram_mb(), 16_000);
}

#[test]
fn classifies_distro_families_from_os_release() {
    let ubuntu = std::fs::read_to_string(fixture("etc/os-release-ubuntu")).unwrap();
    let ubuntu = distro::parse_distro_info(&ubuntu);
    assert_eq!(ubuntu.family, DistroFamily::Ubuntu);
    assert_eq!(ubuntu.package_manager, PackageManager::Apt);
    assert_eq!(ubuntu.id_like, vec!["debian"]);

    let opensuse = std::fs::read_to_string(fixture("etc/os-release-opensuse")).unwrap();
    let opensuse = distro::parse_distro_info(&opensuse);
    assert_eq!(opensuse.family, DistroFamily::OpenSuse);
    assert_eq!(opensuse.package_manager, PackageManager::Zypper);

    let arch = std::fs::read_to_string(fixture("etc/os-release-arch")).unwrap();
    let arch = distro::parse_distro_info(&arch);
    assert_eq!(arch.family, DistroFamily::Arch);
    assert_eq!(arch.package_manager, PackageManager::Pacman);

    let fedora = std::fs::read_to_string(fixture("etc/os-release-fedora")).unwrap();
    let fedora = distro::parse_distro_info(&fedora);
    assert_eq!(fedora.family, DistroFamily::Fedora);
    assert_eq!(fedora.package_manager, PackageManager::Dnf);

    let pop = std::fs::read_to_string(fixture("etc/os-release-pop")).unwrap();
    let pop = distro::parse_distro_info(&pop);
    assert_eq!(pop.family, DistroFamily::Ubuntu);
    assert_eq!(pop.package_manager, PackageManager::Apt);

    let leap = std::fs::read_to_string(fixture("etc/os-release-opensuse-leap")).unwrap();
    let leap = distro::parse_distro_info(&leap);
    assert_eq!(leap.family, DistroFamily::OpenSuse);
}

#[tokio::test]
async fn detects_grub_from_fixture_root() {
    let root = fixture("bootloaders/grub");
    let info = bootloader::detect_from_root(&root, true).await.unwrap();

    assert_eq!(info.kind, BootloaderKind::Grub2);
    assert_eq!(info.config_path, Some(root.join("etc/default/grub")));
    assert_eq!(
        info.update_command.as_deref(),
        Some("grub-mkconfig -o /boot/grub/grub.cfg")
    );
    assert!(info.is_uefi);
}

#[tokio::test]
async fn detects_systemd_boot_default_and_entries_from_fixture_root() {
    let root = fixture("bootloaders/systemd");
    let info = bootloader::detect_from_root(&root, true).await.unwrap();

    assert_eq!(info.kind, BootloaderKind::SystemdBoot);
    assert_eq!(info.active_entry.as_deref(), Some("arch.conf"));
    assert_eq!(info.entry_paths.len(), 1);
    assert_eq!(info.config_path, Some(root.join("boot/loader/loader.conf")));
}

#[tokio::test]
async fn parses_iommu_groups_from_windows_friendly_sysfs_fixture() {
    let root = fixture("sysfs");
    let groups = iommu::detect_groups_from_sysfs_root(&root).await.unwrap();

    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].id, 15);
    assert!(groups[0].is_isolated_for_gpu);
    assert_eq!(groups[0].devices[0].pci_slot, "0000:01:00.0");
    assert_eq!(iommu::group_for_pci_slot(&groups, "0000:01:00.0"), Some(15));
    assert!(iommu::is_gpu_isolated(&groups, "0000:01:00.0"));

    assert_eq!(groups[1].id, 16);
    assert!(!groups[1].is_isolated_for_gpu);
}

#[tokio::test]
async fn detects_gpus_and_companion_audio_from_sysfs_fixture() {
    let root = fixture("sysfs");
    let gpus = gpu::detect_all_from_sysfs_root(&root).await.unwrap();

    assert_eq!(gpus.len(), 2);

    let igpu = &gpus[0];
    assert_eq!(igpu.pci_slot, "0000:00:02.0");
    assert_eq!(igpu.vendor, GpuVendor::Intel);
    assert_eq!(igpu.gpu_type, GpuType::Integrated);
    assert_eq!(igpu.model_name, "Intel UHD Graphics 770");
    assert_eq!(igpu.current_driver.as_deref(), Some("i915"));
    assert!(igpu.is_boot_vga);
    assert!(igpu.companion_audio.is_none());

    let dgpu = &gpus[1];
    assert_eq!(dgpu.pci_slot, "0000:01:00.0");
    assert_eq!(dgpu.vendor, GpuVendor::Nvidia);
    assert_eq!(dgpu.gpu_type, GpuType::Discrete);
    assert_eq!(dgpu.current_driver.as_deref(), Some("nvidia"));
    assert!(dgpu.rom_accessible);

    let audio = dgpu.companion_audio.as_ref().unwrap();
    assert_eq!(audio.pci_slot, "0000:01:00.1");
    assert_eq!(audio.current_driver.as_deref(), Some("snd_hda_intel"));
}

#[tokio::test]
async fn detects_monitors_and_maps_them_to_gpu_pci_slots() {
    let drm_root = fixture("sysfs/class/drm");
    let monitors = monitors::detect_from_drm_root(&drm_root).await.unwrap();

    assert_eq!(monitors.len(), 3);
    assert_eq!(monitors[0].connector_name, "DP-1");
    assert!(monitors[0].connected);
    assert_eq!(monitors[0].current_mode.as_deref(), Some("2560x1440"));
    assert_eq!(monitors[0].gpu_pci_slot.as_deref(), Some("0000:01:00.0"));
    assert!(!monitors[0].is_internal);

    assert_eq!(monitors[1].connector_name, "eDP-1");
    assert!(monitors[1].connected);
    assert_eq!(monitors[1].gpu_pci_slot.as_deref(), Some("0000:00:02.0"));
    assert!(monitors[1].is_internal);

    assert_eq!(monitors[2].connector_name, "HDMI-A-1");
    assert!(!monitors[2].connected);
}

#[test]
fn parses_secure_boot_and_virsh_output() {
    assert!(readiness::parse_secure_boot_state(&[0, 0, 0, 0, 1]));
    assert!(!readiness::parse_secure_boot_state(&[0, 0, 0, 0, 0]));
    assert!(readiness::parse_secure_boot_state(b"1"));

    let domains = readiness::parse_virsh_list_all(
        r#"
 Id   Name              State
----------------------------------
 -    win11-gaming      shut off
 2    linux-test        running
"#,
    );

    assert_eq!(domains.len(), 2);
    assert_eq!(domains[0].id, None);
    assert_eq!(domains[0].name, "win11-gaming");
    assert_eq!(domains[0].state, "shut off");
    assert_eq!(domains[1].id.as_deref(), Some("2"));
    assert_eq!(domains[1].state, "running");
}

#[tokio::test]
async fn detects_system_readiness_from_fixture_root() {
    let root = fixture("readiness");
    let info = readiness::detect_from_root(&root).await.unwrap();

    assert_eq!(info.kernel_version, "6.8.9-arch1-1");
    assert!(info
        .kernel_cmdline_params
        .iter()
        .any(|param| param == "intel_iommu=on"));
    assert!(info
        .loaded_modules
        .iter()
        .any(|module| module == "vfio_pci"));
    assert!(info.kernel_headers.present);
    assert_eq!(
        info.kernel_headers.path,
        Some(root.join("usr/lib/modules/6.8.9-arch1-1/build"))
    );
    assert!(info.secure_boot);
    assert!(info.ovmf.available());
    assert_eq!(info.ovmf.code_paths.len(), 1);
    assert!(info.user_access.in_libvirt_group);
    assert!(info.user_access.in_kvm_group);
    assert_eq!(info.user_access.username.as_deref(), Some("alice"));
    assert_eq!(info.libvirt_domains.len(), 2);
}

#[tokio::test]
async fn detects_initramfs_variants_from_fixture_roots() {
    let arch = initramfs::detect_from_root(fixture("initramfs/arch"), false)
        .await
        .unwrap();
    assert_eq!(arch, InitramfsSystem::Mkinitcpio);

    let fedora = initramfs::detect_from_root(fixture("initramfs/fedora"), false)
        .await
        .unwrap();
    assert_eq!(fedora, InitramfsSystem::Dracut);

    let debian = initramfs::detect_from_root(fixture("initramfs/debian"), false)
        .await
        .unwrap();
    assert_eq!(debian, InitramfsSystem::UpdateInitramfs);
}

#[tokio::test]
async fn detects_display_manager_from_fixture_root() {
    let sddm = display_manager::detect_from_root(fixture("display/sddm"))
        .await
        .unwrap();
    assert_eq!(sddm, DisplayManager::Sddm);

    let greetd = display_manager::detect_from_root(fixture("display/greetd"))
        .await
        .unwrap();
    assert_eq!(greetd, DisplayManager::Greetd);

    assert_eq!(
        display_manager::parse_display_manager_service("lightdm.service"),
        DisplayManager::LightDm
    );
}

#[tokio::test]
async fn detects_audio_stack_from_fixture_root() {
    let pipewire = audio::detect_from_root(fixture("audio/pipewire"))
        .await
        .unwrap();
    assert_eq!(pipewire, AudioSystem::PipeWire);

    let pulse = audio::detect_from_root(fixture("audio/pulseaudio"))
        .await
        .unwrap();
    assert_eq!(pulse, AudioSystem::PulseAudio);

    let alsa = audio::detect_from_root(fixture("audio/alsa"))
        .await
        .unwrap();
    assert_eq!(alsa, AudioSystem::Alsa);
}

#[tokio::test]
async fn detects_usb_input_devices_from_fixture_root() {
    let devices = usb::detect_input_devices_from_root(fixture("usb"))
        .await
        .unwrap();

    assert_eq!(devices.len(), 3);
    assert_eq!(devices[0].device_class, UsbDeviceClass::Keyboard);
    assert_eq!(devices[0].vendor_id, "046d");
    assert_eq!(devices[0].product_id, "c31c");
    assert_eq!(devices[1].device_class, UsbDeviceClass::Mouse);
    assert_eq!(devices[2].device_class, UsbDeviceClass::Gamepad);
}

#[tokio::test]
async fn detects_storage_free_space_from_fixture_root() {
    let info = storage::detect_from_root(fixture("storage"), false)
        .await
        .unwrap();

    assert_eq!(info.available_bytes, 214_748_364_800);
    assert_eq!(info.available_gb(), 200);
    assert_eq!(
        storage::parse_df_available_bytes("Avail\n107374182400\n"),
        107_374_182_400
    );
}

#[tokio::test]
async fn detects_readiness_variant_paths_and_libvirt_states() {
    let info = readiness::detect_from_root(fixture("readiness-fedora"))
        .await
        .unwrap();

    assert_eq!(info.kernel_version, "6.9.7-200.fc40.x86_64");
    assert!(!info.secure_boot);
    assert!(info.ovmf.available());
    assert!(info
        .ovmf
        .code_paths
        .iter()
        .any(|path| path.ends_with("usr/share/edk2/ovmf/OVMF_CODE.fd")));
    assert!(info
        .loaded_modules
        .iter()
        .any(|module| module == "vfio_iommu_type1"));
    assert_eq!(info.libvirt_domains.len(), 3);
    assert_eq!(info.libvirt_domains[2].state, "paused");
}
