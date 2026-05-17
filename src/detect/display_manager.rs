// detect/display_manager.rs
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DisplayManager {
    Gdm,
    Sddm,
    LightDm,
    Greetd,
    Ly,
    Lxdm,
    None, // TTY only
    Unknown,
}

impl DisplayManager {
    pub fn service_name(&self) -> Option<&str> {
        match self {
            DisplayManager::Gdm => Some("gdm"),
            DisplayManager::Sddm => Some("sddm"),
            DisplayManager::LightDm => Some("lightdm"),
            DisplayManager::Greetd => Some("greetd"),
            DisplayManager::Ly => Some("ly"),
            DisplayManager::Lxdm => Some("lxdm"),
            _ => None,
        }
    }
}

impl std::fmt::Display for DisplayManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            DisplayManager::Gdm => "GDM",
            DisplayManager::Sddm => "SDDM",
            DisplayManager::LightDm => "LightDM",
            DisplayManager::Greetd => "greetd",
            DisplayManager::Ly => "ly",
            DisplayManager::Lxdm => "LXDM",
            DisplayManager::None => "None (TTY)",
            DisplayManager::Unknown => "Unknown",
        };
        write!(f, "{s}")
    }
}

pub async fn detect() -> Result<DisplayManager> {
    if let Ok(dm) = detect_from_root(Path::new("/")).await {
        if dm != DisplayManager::Unknown {
            return Ok(dm);
        }
    }

    let services = [
        ("gdm", DisplayManager::Gdm),
        ("sddm", DisplayManager::Sddm),
        ("lightdm", DisplayManager::LightDm),
        ("greetd", DisplayManager::Greetd),
        ("ly", DisplayManager::Ly),
        ("lxdm", DisplayManager::Lxdm),
    ];

    for (service, dm) in &services {
        let output = tokio::process::Command::new("systemctl")
            .args(["is-active", "--quiet", service])
            .output()
            .await;
        if let Ok(o) = output {
            if o.status.success() {
                return Ok(dm.clone());
            }
        }
    }

    Ok(DisplayManager::Unknown)
}

pub async fn detect_from_root(root: impl AsRef<Path>) -> Result<DisplayManager> {
    let root = root.as_ref();
    let display_manager_service = rooted(root, "/etc/systemd/system/display-manager.service");

    if display_manager_service.exists() {
        if let Ok(target) = tokio::fs::read_link(&display_manager_service).await {
            if let Some(service) = target.file_name().map(|name| name.to_string_lossy()) {
                return Ok(parse_display_manager_service(&service));
            }
        }

        if let Ok(content) = tokio::fs::read_to_string(&display_manager_service).await {
            return Ok(parse_display_manager_service(&content));
        }
    }

    for (path, dm) in [
        ("/etc/systemd/system/gdm.service", DisplayManager::Gdm),
        ("/etc/systemd/system/sddm.service", DisplayManager::Sddm),
        (
            "/etc/systemd/system/lightdm.service",
            DisplayManager::LightDm,
        ),
        ("/etc/systemd/system/greetd.service", DisplayManager::Greetd),
        ("/etc/systemd/system/ly.service", DisplayManager::Ly),
        ("/etc/systemd/system/lxdm.service", DisplayManager::Lxdm),
    ] {
        if rooted(root, path).exists() {
            return Ok(dm);
        }
    }

    Ok(DisplayManager::Unknown)
}

pub fn parse_display_manager_service(service: &str) -> DisplayManager {
    let service = service.to_lowercase();
    if service.contains("gdm") {
        DisplayManager::Gdm
    } else if service.contains("sddm") {
        DisplayManager::Sddm
    } else if service.contains("lightdm") {
        DisplayManager::LightDm
    } else if service.contains("greetd") {
        DisplayManager::Greetd
    } else if service.contains("ly.service") || service.trim() == "ly" {
        DisplayManager::Ly
    } else if service.contains("lxdm") {
        DisplayManager::Lxdm
    } else {
        DisplayManager::Unknown
    }
}

fn rooted(root: &Path, absolute_path: &str) -> std::path::PathBuf {
    root.join(absolute_path.trim_start_matches('/'))
}
