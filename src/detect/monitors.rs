use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tracing::debug;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorInfo {
    /// DRM connector name, e.g. `HDMI-A-1`, `DP-1`, `eDP-1`.
    pub connector_name: String,
    /// Whether a monitor is physically connected.
    pub connected: bool,
    /// Current resolution if active.
    pub current_mode: Option<String>,
    /// Associated DRM card name.
    pub card: String,
    /// PCI slot of the GPU owning this DRM card, when detectable.
    #[serde(default)]
    pub gpu_pci_slot: Option<String>,
    /// Whether this is a laptop internal display.
    pub is_internal: bool,
}

/// Detect monitors via live DRM sysfs.
pub async fn detect() -> Result<Vec<MonitorInfo>> {
    detect_from_drm_root(Path::new("/sys/class/drm")).await
}

/// Detect monitors from a DRM sysfs root.
pub async fn detect_from_drm_root(drm_root: impl AsRef<Path>) -> Result<Vec<MonitorInfo>> {
    debug!("Detecting connected monitors");

    let drm_root = drm_root.as_ref();
    if !drm_root.exists() {
        return Ok(Vec::new());
    }

    let mut monitors = Vec::new();
    let mut dir = tokio::fs::read_dir(drm_root).await?;

    while let Some(entry) = dir.next_entry().await? {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some((card, connector_name)) = parse_connector_entry_name(&name) else {
            continue;
        };

        let status = tokio::fs::read_to_string(entry.path().join("status"))
            .await
            .unwrap_or_default();
        let connected = status.trim() == "connected";

        let current_mode = tokio::fs::read_to_string(entry.path().join("modes"))
            .await
            .ok()
            .and_then(|s| s.lines().next().map(String::from));

        let gpu_pci_slot = detect_card_pci_slot(drm_root, &card).await;
        let is_internal = connector_name.starts_with("eDP") || connector_name.starts_with("LVDS");

        monitors.push(MonitorInfo {
            connector_name,
            connected,
            current_mode,
            card,
            gpu_pci_slot,
            is_internal,
        });
    }

    monitors.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then_with(|| a.is_internal.cmp(&b.is_internal))
            .then_with(|| a.card.cmp(&b.card))
            .then_with(|| a.connector_name.cmp(&b.connector_name))
    });

    debug!(
        "Found {} monitors ({} connected)",
        monitors.len(),
        monitors.iter().filter(|m| m.connected).count()
    );

    Ok(monitors)
}

fn parse_connector_entry_name(name: &str) -> Option<(String, String)> {
    let (card, connector) = name.split_once('-')?;
    if card.starts_with("card") && !connector.is_empty() {
        Some((card.to_string(), connector.to_string()))
    } else {
        None
    }
}

async fn detect_card_pci_slot(drm_root: &Path, card: &str) -> Option<String> {
    let device_path = drm_root.join(card).join("device");

    if let Ok(text) = tokio::fs::read_to_string(&device_path).await {
        let slot = normalize_pci_slot_name(text.trim());
        if looks_like_pci_slot(&slot) {
            return Some(slot);
        }
    }

    if let Ok(target) = tokio::fs::read_link(&device_path).await {
        let name = target.file_name()?.to_string_lossy();
        let slot = normalize_pci_slot_name(&name);
        if looks_like_pci_slot(&slot) {
            return Some(slot);
        }
    }

    None
}

fn looks_like_pci_slot(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 12
        && bytes.get(4) == Some(&b':')
        && bytes.get(7) == Some(&b':')
        && bytes.contains(&b'.')
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
