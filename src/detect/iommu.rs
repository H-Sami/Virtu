use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::debug;

/// PCI class code prefix for PCI bridges. Bridges can share a GPU group
/// without making the group unsafe for passthrough by themselves.
const PCI_BRIDGE_CLASS_PREFIX: &str = "0x06";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IommuGroup {
    pub id: u32,
    pub devices: Vec<IommuDevice>,
    /// True if the only non-bridge devices in this group are GPU + GPU audio.
    pub is_isolated_for_gpu: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IommuDevice {
    pub pci_slot: String,
    pub class: String,
    pub vendor_id: String,
    pub device_id: String,
    pub is_bridge: bool,
}

/// Parse all IOMMU groups from the live sysfs tree.
pub async fn detect_groups() -> Result<Vec<IommuGroup>> {
    detect_groups_from_sysfs_root(Path::new("/sys")).await
}

/// Parse IOMMU groups from a sysfs root.
///
/// This accepts either real sysfs symlinks under
/// `kernel/iommu_groups/<id>/devices/` or fixture directories named by PCI
/// slot. That keeps live detection realistic while making parser tests cheap.
pub async fn detect_groups_from_sysfs_root(
    sysfs_root: impl AsRef<Path>,
) -> Result<Vec<IommuGroup>> {
    debug!("Parsing IOMMU groups");

    let sysfs_root = sysfs_root.as_ref();
    let groups_path = sysfs_root.join("kernel/iommu_groups");
    if !groups_path.exists() {
        debug!("No IOMMU groups found - IOMMU is not active");
        return Ok(Vec::new());
    }

    let mut dir = fs::read_dir(groups_path).await?;
    let mut groups = Vec::new();

    while let Some(entry) = dir.next_entry().await? {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let group_id: u32 = match name_str.parse() {
            Ok(id) => id,
            Err(_) => continue,
        };

        let devices_path = entry.path().join("devices");
        if !devices_path.exists() {
            continue;
        }

        let devices = read_group_devices(sysfs_root, &devices_path).await;
        let is_isolated_for_gpu = check_gpu_isolation(&devices);

        groups.push(IommuGroup {
            id: group_id,
            devices,
            is_isolated_for_gpu,
        });
    }

    groups.sort_by_key(|g| g.id);
    debug!("Found {} IOMMU groups", groups.len());
    Ok(groups)
}

async fn read_group_devices(sysfs_root: &Path, devices_path: &Path) -> Vec<IommuDevice> {
    let mut dir = match fs::read_dir(devices_path).await {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };

    let mut devices = Vec::new();

    while let Some(entry) = dir.next_entry().await.unwrap_or(None) {
        let pci_slot = pci_slot_from_entry_name(&entry.file_name().to_string_lossy());
        let device_path = resolve_iommu_device_path(sysfs_root, &entry.path(), &pci_slot).await;

        let class = read_sysfs_str(&device_path.join("class"))
            .await
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        let vendor_id = normalize_hex_id(
            &read_sysfs_str(&device_path.join("vendor"))
                .await
                .unwrap_or_default(),
        );
        let device_id = normalize_hex_id(
            &read_sysfs_str(&device_path.join("device"))
                .await
                .unwrap_or_default(),
        );

        let is_bridge = is_bridge_class(&class);

        devices.push(IommuDevice {
            pci_slot,
            class,
            vendor_id,
            device_id,
            is_bridge,
        });
    }

    devices.sort_by(|a, b| a.pci_slot.cmp(&b.pci_slot));
    devices
}

fn pci_slot_from_entry_name(entry_name: &str) -> String {
    if entry_name.contains(':') {
        return entry_name.to_string();
    }

    let mut parts = entry_name.splitn(3, '_');
    let Some(domain) = parts.next() else {
        return entry_name.to_string();
    };
    let Some(bus) = parts.next() else {
        return entry_name.to_string();
    };
    let Some(device_function) = parts.next() else {
        return entry_name.to_string();
    };

    if domain.len() == 4 && bus.len() == 2 && device_function.len() >= 4 {
        format!("{domain}:{bus}:{device_function}")
    } else {
        entry_name.to_string()
    }
}

async fn resolve_iommu_device_path(
    sysfs_root: &Path,
    entry_path: &Path,
    pci_slot: &str,
) -> PathBuf {
    if entry_path.join("class").exists() {
        return entry_path.to_path_buf();
    }

    if let Ok(target) = fs::read_link(entry_path).await {
        if target.is_absolute() {
            if let Ok(relative_to_sys) = target.strip_prefix("/sys") {
                return sysfs_root.join(relative_to_sys);
            }
            return target;
        }

        let parent = entry_path.parent().unwrap_or(entry_path);
        let joined = parent.join(&target);
        if joined.join("class").exists() {
            return joined;
        }
    }

    sysfs_root.join("bus/pci/devices").join(pci_slot)
}

fn normalize_hex_id(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X")
        .to_lowercase()
}

fn is_bridge_class(class: &str) -> bool {
    class
        .trim()
        .to_lowercase()
        .starts_with(PCI_BRIDGE_CLASS_PREFIX)
}

/// A group is safe for GPU passthrough if, after removing PCI bridges, the only
/// remaining devices are display-class and audio-class devices.
pub fn check_gpu_isolation(devices: &[IommuDevice]) -> bool {
    let non_bridge: Vec<&IommuDevice> = devices.iter().filter(|d| !d.is_bridge).collect();

    if non_bridge.is_empty() {
        return false;
    }

    non_bridge.iter().all(|d| {
        let class = d.class.to_lowercase();
        class.starts_with("0x03") || class.starts_with("0x0401") || class.starts_with("0x0403")
    })
}

/// Return true if the GPU at the given PCI slot is isolated in its IOMMU group.
pub fn is_gpu_isolated(groups: &[IommuGroup], pci_slot: &str) -> bool {
    groups
        .iter()
        .any(|g| g.devices.iter().any(|d| d.pci_slot == pci_slot) && g.is_isolated_for_gpu)
}

/// Return the IOMMU group ID for a given PCI slot.
pub fn group_for_pci_slot(groups: &[IommuGroup], pci_slot: &str) -> Option<u32> {
    groups
        .iter()
        .find(|g| g.devices.iter().any(|d| d.pci_slot == pci_slot))
        .map(|g| g.id)
}

async fn read_sysfs_str(path: &Path) -> Option<String> {
    fs::read_to_string(path).await.ok()
}
