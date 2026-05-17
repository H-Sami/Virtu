use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::debug;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum UsbDeviceClass {
    Keyboard,
    Mouse,
    Gamepad,
    UsbHub,
    Other,
}

impl std::fmt::Display for UsbDeviceClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UsbDeviceClass::Keyboard => write!(f, "Keyboard"),
            UsbDeviceClass::Mouse => write!(f, "Mouse"),
            UsbDeviceClass::Gamepad => write!(f, "Gamepad"),
            UsbDeviceClass::UsbHub => write!(f, "USB Hub"),
            UsbDeviceClass::Other => write!(f, "Other"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsbDevice {
    /// Device name from `/dev/input/by-id/` or product name.
    pub name: String,
    /// `/dev/input/by-id/` path for evdev passthrough.
    pub evdev_path: Option<PathBuf>,
    /// Vendor ID as lowercase hex.
    pub vendor_id: String,
    /// Product ID as lowercase hex.
    pub product_id: String,
    pub device_class: UsbDeviceClass,
    /// USB bus path for USB controller passthrough alternative.
    pub usb_bus_path: Option<String>,
}

/// Detect input devices for evdev passthrough.
pub async fn detect_input_devices() -> Result<Vec<UsbDevice>> {
    detect_input_devices_from_root(Path::new("/")).await
}

/// Detect input devices from a fixture or alternate filesystem root.
pub async fn detect_input_devices_from_root(root: impl AsRef<Path>) -> Result<Vec<UsbDevice>> {
    debug!("Detecting USB input devices");

    let root = root.as_ref();
    let by_id_path = rooted(root, "/dev/input/by-id");
    if !by_id_path.exists() {
        return Ok(Vec::new());
    }

    let mut devices = Vec::new();
    let mut dir = tokio::fs::read_dir(by_id_path).await?;

    while let Some(entry) = dir.next_entry().await? {
        let name = entry.file_name().to_string_lossy().to_string();
        let device_class = classify_input_device(&name);

        if !matches!(
            device_class,
            UsbDeviceClass::Keyboard | UsbDeviceClass::Mouse | UsbDeviceClass::Gamepad
        ) {
            continue;
        }

        let friendly = friendly_input_name(&name);
        let (vendor_id, product_id) = extract_usb_ids(root, &entry.path()).await;

        devices.push(UsbDevice {
            name: friendly,
            evdev_path: Some(entry.path()),
            vendor_id,
            product_id,
            device_class,
            usb_bus_path: None,
        });
    }

    devices.sort_by(|a, b| {
        let order_a = device_class_order(&a.device_class);
        let order_b = device_class_order(&b.device_class);
        order_a.cmp(&order_b).then_with(|| a.name.cmp(&b.name))
    });

    debug!("Found {} input devices", devices.len());
    Ok(devices)
}

pub fn classify_input_device(name: &str) -> UsbDeviceClass {
    let lower = name.to_lowercase();
    if lower.ends_with("-event-kbd") || lower.contains("keyboard") {
        UsbDeviceClass::Keyboard
    } else if lower.ends_with("-event-mouse") || lower.contains("mouse") {
        UsbDeviceClass::Mouse
    } else if lower.ends_with("-event-joystick")
        || lower.contains("gamepad")
        || lower.contains("controller")
    {
        UsbDeviceClass::Gamepad
    } else if lower.contains("hub") {
        UsbDeviceClass::UsbHub
    } else {
        UsbDeviceClass::Other
    }
}

pub fn friendly_input_name(name: &str) -> String {
    name.trim_start_matches("usb-")
        .trim_start_matches("hid-")
        .replace("-event-kbd", "")
        .replace("-event-mouse", "")
        .replace("-event-joystick", "")
        .replace("-if00", "")
        .replace("-if01", "")
        .replace('_', " ")
}

fn device_class_order(class: &UsbDeviceClass) -> u8 {
    match class {
        UsbDeviceClass::Keyboard => 0,
        UsbDeviceClass::Mouse => 1,
        UsbDeviceClass::Gamepad => 2,
        UsbDeviceClass::UsbHub => 3,
        UsbDeviceClass::Other => 4,
    }
}

async fn extract_usb_ids(root: &Path, entry_path: &Path) -> (String, String) {
    let event_name = match resolve_event_name(entry_path).await {
        Some(event_name) => event_name,
        None => return ("0000".to_string(), "0000".to_string()),
    };

    let id_path = rooted(root, &format!("/sys/class/input/{event_name}/device/id"));
    let vendor = read_trimmed(id_path.join("vendor"))
        .await
        .unwrap_or_else(|| "0000".to_string());
    let product = read_trimmed(id_path.join("product"))
        .await
        .unwrap_or_else(|| "0000".to_string());

    (normalize_hex(&vendor), normalize_hex(&product))
}

async fn resolve_event_name(entry_path: &Path) -> Option<String> {
    if let Ok(real) = tokio::fs::read_link(entry_path).await {
        return real
            .file_name()
            .map(|name| name.to_string_lossy().to_string());
    }

    if let Ok(content) = tokio::fs::read_to_string(entry_path).await {
        let trimmed = content.trim();
        if !trimmed.is_empty() {
            return Path::new(trimmed)
                .file_name()
                .map(|name| name.to_string_lossy().to_string());
        }
    }

    None
}

async fn read_trimmed(path: PathBuf) -> Option<String> {
    tokio::fs::read_to_string(path)
        .await
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn normalize_hex(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X")
        .to_lowercase()
}

fn rooted(root: &Path, absolute_path: &str) -> PathBuf {
    root.join(absolute_path.trim_start_matches('/'))
}
