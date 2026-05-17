use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::debug;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DistroFamily {
    Arch,
    Debian,
    Ubuntu,
    Fedora,
    Rhel,
    OpenSuse,
    Gentoo,
    Unknown,
}

impl std::fmt::Display for DistroFamily {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            DistroFamily::Arch => "Arch",
            DistroFamily::Debian => "Debian",
            DistroFamily::Ubuntu => "Ubuntu",
            DistroFamily::Fedora => "Fedora",
            DistroFamily::Rhel => "RHEL",
            DistroFamily::OpenSuse => "OpenSUSE",
            DistroFamily::Gentoo => "Gentoo",
            DistroFamily::Unknown => "Unknown",
        };
        write!(f, "{s}")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistroInfo {
    pub id: String,           // e.g. "arch", "ubuntu", "fedora"
    pub id_like: Vec<String>, // e.g. ["debian"] for Ubuntu
    pub pretty_name: String,  // e.g. "Ubuntu 22.04.3 LTS"
    pub version_id: String,   // e.g. "22.04"
    pub family: DistroFamily,
    pub package_manager: PackageManager,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PackageManager {
    Pacman,  // Arch
    Apt,     // Debian/Ubuntu
    Dnf,     // Fedora/RHEL
    Zypper,  // OpenSUSE
    Portage, // Gentoo
    Unknown,
}

impl PackageManager {
    pub fn install_command(&self) -> &str {
        match self {
            PackageManager::Pacman => "pacman -S --noconfirm",
            PackageManager::Apt => "apt-get install -y",
            PackageManager::Dnf => "dnf install -y",
            PackageManager::Zypper => "zypper install -y",
            PackageManager::Portage => "emerge",
            PackageManager::Unknown => "unknown",
        }
    }
}

pub async fn detect() -> Result<DistroInfo> {
    debug!("Detecting Linux distribution");

    let content = tokio::fs::read_to_string("/etc/os-release")
        .await
        .context("Cannot read /etc/os-release")?;

    Ok(parse_distro_info(&content))
}

pub fn parse_distro_info(content: &str) -> DistroInfo {
    let fields = parse_os_release(content);

    let id = fields
        .get("ID")
        .cloned()
        .unwrap_or_else(|| "unknown".to_string());
    let id_like: Vec<String> = fields
        .get("ID_LIKE")
        .map(|s| s.split_whitespace().map(String::from).collect())
        .unwrap_or_default();
    let pretty_name = fields
        .get("PRETTY_NAME")
        .cloned()
        .unwrap_or_else(|| id.clone());
    let version_id = fields.get("VERSION_ID").cloned().unwrap_or_default();

    let family = classify_family(&id, &id_like);
    let package_manager = detect_package_manager(&family);

    DistroInfo {
        id,
        id_like,
        pretty_name,
        version_id,
        family,
        package_manager,
    }
}

pub fn parse_os_release(content: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let value = unquote_os_release_value(value.trim());
            map.insert(key.trim().to_string(), value);
        }
    }
    map
}

fn unquote_os_release_value(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        return value[1..value.len() - 1].to_string();
    }
    value.to_string()
}

pub fn classify_family(id: &str, id_like: &[String]) -> DistroFamily {
    let all_ids: Vec<&str> = std::iter::once(id)
        .chain(id_like.iter().map(String::as_str))
        .collect();

    for candidate in &all_ids {
        match *candidate {
            "arch" | "endeavouros" | "manjaro" | "garuda" | "artix" => {
                return DistroFamily::Arch;
            }
            "ubuntu" | "linuxmint" | "pop" | "elementary" | "zorin" => {
                return DistroFamily::Ubuntu;
            }
            "debian" => {
                return DistroFamily::Debian;
            }
            "fedora" => {
                return DistroFamily::Fedora;
            }
            "rhel" | "centos" | "almalinux" | "rocky" => {
                return DistroFamily::Rhel;
            }
            "opensuse" | "opensuse-leap" | "opensuse-tumbleweed" | "sled" | "sles" => {
                return DistroFamily::OpenSuse;
            }
            "gentoo" => {
                return DistroFamily::Gentoo;
            }
            other if other.starts_with("opensuse-") => {
                return DistroFamily::OpenSuse;
            }
            _ => {}
        }
    }
    DistroFamily::Unknown
}

pub fn detect_package_manager(family: &DistroFamily) -> PackageManager {
    match family {
        DistroFamily::Arch => PackageManager::Pacman,
        DistroFamily::Ubuntu => PackageManager::Apt,
        DistroFamily::Debian => PackageManager::Apt,
        DistroFamily::Fedora => PackageManager::Dnf,
        DistroFamily::Rhel => PackageManager::Dnf,
        DistroFamily::OpenSuse => PackageManager::Zypper,
        DistroFamily::Gentoo => PackageManager::Portage,
        DistroFamily::Unknown => PackageManager::Unknown,
    }
}
