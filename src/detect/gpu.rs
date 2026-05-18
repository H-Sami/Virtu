use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::debug;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Unknown(String),
}

impl std::fmt::Display for GpuVendor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GpuVendor::Nvidia => write!(f, "NVIDIA"),
            GpuVendor::Amd => write!(f, "AMD"),
            GpuVendor::Intel => write!(f, "Intel"),
            GpuVendor::Unknown(v) => write!(f, "Unknown({v})"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GpuType {
    Discrete,
    Integrated,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuInfo {
    /// PCI domain:bus:device.function, e.g. `0000:01:00.0`.
    pub pci_slot: String,
    pub vendor: GpuVendor,
    pub gpu_type: GpuType,
    /// Human-readable model name from PCI ID database or sysfs.
    pub model_name: String,
    /// 4-hex-digit vendor ID, e.g. `10de`.
    pub vendor_id: String,
    /// 4-hex-digit device ID, e.g. `2684`.
    pub device_id: String,
    pub subsystem_vendor_id: String,
    pub subsystem_device_id: String,
    /// Currently bound kernel driver, if any.
    pub current_driver: Option<String>,
    /// IOMMU group number, filled in after IOMMU detection.
    pub iommu_group_id: Option<u32>,
    /// Whether this GPU is isolated enough for GPU passthrough.
    pub iommu_isolated: bool,
    /// Whether the GPU ROM sysfs node exists.
    pub rom_accessible: bool,
    /// Companion HDMI/DP audio device on another function of the same slot.
    pub companion_audio: Option<CompanionDevice>,
    /// True if firmware marked this as the boot VGA device.
    pub is_boot_vga: bool,
    /// Computed after IOMMU groups and driver state are known.
    pub vfio_compatible: bool,
    /// Known quirks from the local knowledge base.
    pub quirks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompanionDevice {
    pub pci_slot: String,
    pub vendor_id: String,
    pub device_id: String,
    pub class: String,
    pub current_driver: Option<String>,
}

/// Detect all GPU devices in the system by scanning live sysfs.
pub async fn detect_all() -> Result<Vec<GpuInfo>> {
    detect_all_from_sysfs_root(Path::new("/sys")).await
}

/// Detect GPU devices from a sysfs root.
///
/// Fixture roots may use Windows-friendly PCI directory names such as
/// `0000_01_00.0`; these are normalized back to Linux PCI slot IDs.
pub async fn detect_all_from_sysfs_root(sysfs_root: impl AsRef<Path>) -> Result<Vec<GpuInfo>> {
    debug!("Scanning PCI devices for GPUs");

    let sysfs_root = sysfs_root.as_ref();
    let devices_path = sysfs_root.join("bus/pci/devices");
    let mut dir = fs::read_dir(&devices_path).await.with_context(|| {
        format!(
            "Cannot read PCI device directory {}",
            devices_path.display()
        )
    })?;

    let mut gpus = Vec::new();

    while let Some(entry) = dir.next_entry().await? {
        let pci_slot = normalize_pci_slot_name(&entry.file_name().to_string_lossy());
        let device_path = entry.path();

        let Some(class) = read_sysfs_str(&device_path.join("class")).await else {
            continue;
        };

        if is_gpu_class(&class) {
            if let Some(gpu) = parse_gpu_device(sysfs_root, &pci_slot, &device_path).await {
                gpus.push(gpu);
            }
        }
    }

    gpus.sort_by(|a, b| a.pci_slot.cmp(&b.pci_slot));

    debug!("Found {} GPU(s)", gpus.len());
    Ok(gpus)
}

async fn parse_gpu_device(
    sysfs_root: &Path,
    pci_slot: &str,
    device_path: &Path,
) -> Option<GpuInfo> {
    let vendor_id = normalize_hex_id(&read_sysfs_str(&device_path.join("vendor")).await?);
    let device_id = normalize_hex_id(&read_sysfs_str(&device_path.join("device")).await?);

    let subsystem_vendor_id = normalize_hex_id(
        &read_sysfs_str(&device_path.join("subsystem_vendor"))
            .await
            .unwrap_or_default(),
    );
    let subsystem_device_id = normalize_hex_id(
        &read_sysfs_str(&device_path.join("subsystem_device"))
            .await
            .unwrap_or_default(),
    );

    let vendor = vendor_id_to_vendor(&vendor_id);
    let gpu_type = determine_gpu_type(&vendor, pci_slot);
    let model_name = detect_model_name(device_path, &vendor, &vendor_id, &device_id).await;
    let current_driver = detect_driver(device_path).await;
    let companion_audio = detect_companion_audio(sysfs_root, pci_slot).await;
    let is_boot_vga = read_sysfs_str(&device_path.join("boot_vga"))
        .await
        .map(|s| s.trim() == "1")
        .unwrap_or(false);
    let rom_accessible = device_path.join("rom").exists();

    Some(GpuInfo {
        pci_slot: pci_slot.to_string(),
        vendor,
        gpu_type,
        model_name,
        vendor_id,
        device_id,
        subsystem_vendor_id,
        subsystem_device_id,
        current_driver,
        iommu_group_id: None,
        iommu_isolated: false,
        rom_accessible,
        companion_audio,
        is_boot_vga,
        vfio_compatible: false,
        quirks: Vec::new(),
    })
}

fn is_gpu_class(class: &str) -> bool {
    class.trim().to_lowercase().starts_with("0x03")
}

pub fn vendor_id_to_vendor(vendor_id: &str) -> GpuVendor {
    match vendor_id {
        "10de" => GpuVendor::Nvidia,
        "1002" => GpuVendor::Amd,
        "8086" => GpuVendor::Intel,
        other => GpuVendor::Unknown(other.to_string()),
    }
}

pub fn determine_gpu_type(vendor: &GpuVendor, pci_slot: &str) -> GpuType {
    match vendor {
        GpuVendor::Intel => GpuType::Integrated,
        GpuVendor::Amd => {
            let Some(bus_str) = pci_slot.split(':').nth(1) else {
                return GpuType::Unknown;
            };
            match u8::from_str_radix(bus_str, 16) {
                Ok(0x00 | 0x06) => GpuType::Integrated,
                Ok(_) => GpuType::Discrete,
                Err(_) => GpuType::Unknown,
            }
        }
        GpuVendor::Nvidia => GpuType::Discrete,
        GpuVendor::Unknown(_) => GpuType::Unknown,
    }
}

async fn detect_model_name(
    device_path: &Path,
    vendor: &GpuVendor,
    vendor_id: &str,
    device_id: &str,
) -> String {
    if let Some(label) = read_sysfs_str(&device_path.join("label")).await {
        if !label.trim().is_empty() {
            return label.trim().to_string();
        }
    }

    let vendor_str = match vendor {
        GpuVendor::Nvidia => "NVIDIA",
        GpuVendor::Amd => "AMD",
        GpuVendor::Intel => "Intel",
        GpuVendor::Unknown(v) => v.as_str(),
    };

    format!("{vendor_str} GPU [{vendor_id}:{device_id}]")
}

async fn detect_driver(device_path: &Path) -> Option<String> {
    let fixture_driver_name = device_path.join("driver_name");
    if let Some(driver) = read_sysfs_str(&fixture_driver_name).await {
        let driver = driver.trim();
        if !driver.is_empty() {
            return Some(driver.to_string());
        }
    }

    let driver_link = device_path.join("driver");
    if !driver_link.exists() {
        return None;
    }

    if let Ok(target) = fs::read_link(&driver_link).await {
        return target.file_name().map(|n| n.to_string_lossy().to_string());
    }

    read_sysfs_str(&driver_link)
        .await
        .map(|driver| driver.trim().to_string())
        .filter(|driver| !driver.is_empty())
}

async fn detect_companion_audio(sysfs_root: &Path, gpu_slot: &str) -> Option<CompanionDevice> {
    let base = gpu_slot.rsplit_once('.')?.0;

    for function in 1..=7u8 {
        let candidate_slot = format!("{base}.{function}");
        let candidate_path = pci_device_path(sysfs_root, &candidate_slot);

        if !candidate_path.exists() {
            continue;
        }

        let Some(class) = read_sysfs_str(&candidate_path.join("class")).await else {
            // A sibling function exists but has no class file. That can
            // happen on partial fixture roots; keep scanning instead of
            // bailing out early.
            continue;
        };
        let class = class.trim().to_lowercase();
        if !(class.starts_with("0x0401") || class.starts_with("0x0403")) {
            continue;
        }

        let vendor_id = normalize_hex_id(
            &read_sysfs_str(&candidate_path.join("vendor"))
                .await
                .unwrap_or_default(),
        );
        let device_id = normalize_hex_id(
            &read_sysfs_str(&candidate_path.join("device"))
                .await
                .unwrap_or_default(),
        );
        let current_driver = detect_driver(&candidate_path).await;

        return Some(CompanionDevice {
            pci_slot: candidate_slot,
            vendor_id,
            device_id,
            class,
            current_driver,
        });
    }

    None
}

fn pci_device_path(sysfs_root: &Path, pci_slot: &str) -> PathBuf {
    let devices_root = sysfs_root.join("bus/pci/devices");
    let native = devices_root.join(pci_slot);
    if native.exists() {
        return native;
    }
    devices_root.join(pci_slot.replace(':', "_"))
}

fn normalize_pci_slot_name(name: &str) -> String {
    if name.contains(':') {
        return name.to_string();
    }

    let mut parts = name.splitn(3, '_');
    let Some(domain) = parts.next() else {
        return name.to_string();
    };
    let Some(bus) = parts.next() else {
        return name.to_string();
    };
    let Some(device_function) = parts.next() else {
        return name.to_string();
    };

    if domain.len() == 4 && bus.len() == 2 && device_function.len() >= 4 {
        format!("{domain}:{bus}:{device_function}")
    } else {
        name.to_string()
    }
}

fn normalize_hex_id(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X")
        .to_lowercase()
}

async fn read_sysfs_str(path: &Path) -> Option<String> {
    fs::read_to_string(path).await.ok()
}
