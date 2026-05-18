use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::debug;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BootloaderKind {
    Grub2,
    SystemdBoot,
    Refind,
    Syslinux,
    Efistub,
    Unknown,
}

impl std::fmt::Display for BootloaderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            BootloaderKind::Grub2 => "GRUB2",
            BootloaderKind::SystemdBoot => "systemd-boot",
            BootloaderKind::Refind => "rEFInd",
            BootloaderKind::Syslinux => "Syslinux/EXTLINUX",
            BootloaderKind::Efistub => "EFISTUB (efibootmgr)",
            BootloaderKind::Unknown => "Unknown",
        };
        write!(f, "{s}")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootloaderInfo {
    pub kind: BootloaderKind,
    /// The primary config file to modify.
    pub config_path: Option<PathBuf>,
    /// Additional entry files, such as systemd-boot loader entries.
    pub entry_paths: Vec<PathBuf>,
    /// The active entry for bootloaders that expose one in config.
    pub active_entry: Option<String>,
    /// Command to regenerate/update the bootloader after modifying config.
    pub update_command: Option<String>,
    /// Whether the system is UEFI rather than legacy BIOS.
    pub is_uefi: bool,
}

pub async fn detect() -> Result<BootloaderInfo> {
    debug!("Detecting bootloader");

    let is_uefi = tokio::fs::metadata("/sys/firmware/efi").await.is_ok();
    let info = detect_from_root_internal(Path::new("/"), is_uefi, true).await?;

    if info.kind == BootloaderKind::Unknown && is_uefi {
        if let Some(info) = detect_efistub().await {
            debug!("Detected EFISTUB");
            return Ok(info);
        }
    }

    Ok(info)
}

/// Detect a bootloader from a fixture or alternate filesystem root.
///
/// This does not run host commands such as `efibootmgr`, so tests stay local
/// and deterministic.
pub async fn detect_from_root(root: impl AsRef<Path>, is_uefi: bool) -> Result<BootloaderInfo> {
    detect_from_root_internal(root.as_ref(), is_uefi, false).await
}

async fn detect_from_root_internal(
    root: &Path,
    is_uefi: bool,
    allow_host_command_lookup: bool,
) -> Result<BootloaderInfo> {
    if let Some(info) = detect_grub2(root, is_uefi, allow_host_command_lookup).await {
        debug!("Detected GRUB2");
        return Ok(info);
    }
    if let Some(info) = detect_systemd_boot(root, is_uefi).await {
        debug!("Detected systemd-boot");
        return Ok(info);
    }
    if let Some(info) = detect_refind(root, is_uefi).await {
        debug!("Detected rEFInd");
        return Ok(info);
    }
    if let Some(info) = detect_syslinux(root, is_uefi).await {
        debug!("Detected Syslinux");
        return Ok(info);
    }

    Ok(BootloaderInfo {
        kind: BootloaderKind::Unknown,
        config_path: None,
        entry_paths: Vec::new(),
        active_entry: None,
        update_command: None,
        is_uefi,
    })
}

async fn detect_grub2(
    root: &Path,
    is_uefi: bool,
    allow_host_command_lookup: bool,
) -> Option<BootloaderInfo> {
    let config_path = rooted(root, "/etc/default/grub");
    if !config_path.exists() {
        return None;
    }

    Some(BootloaderInfo {
        kind: BootloaderKind::Grub2,
        config_path: Some(config_path),
        entry_paths: Vec::new(),
        active_entry: None,
        update_command: Some(grub_update_command(root, allow_host_command_lookup)),
        is_uefi,
    })
}

fn grub_update_command(root: &Path, allow_host_command_lookup: bool) -> String {
    if rooted(root, "/boot/grub2/grub.cfg").exists() {
        return "grub2-mkconfig -o /boot/grub2/grub.cfg".to_string();
    }

    if allow_host_command_lookup && which::which("update-grub").is_ok() {
        return "update-grub".to_string();
    }

    if allow_host_command_lookup && which::which("grub2-mkconfig").is_ok() {
        return "grub2-mkconfig -o /boot/grub2/grub.cfg".to_string();
    }

    "grub-mkconfig -o /boot/grub/grub.cfg".to_string()
}

async fn detect_systemd_boot(root: &Path, is_uefi: bool) -> Option<BootloaderInfo> {
    if !is_uefi {
        return None;
    }

    let loader_conf = [
        "/boot/loader/loader.conf",
        "/efi/loader/loader.conf",
        "/boot/efi/loader/loader.conf",
    ]
    .into_iter()
    .map(|path| rooted(root, path))
    .find(|path| path.exists())?;

    let entries_dir = loader_conf.parent()?.join("entries");
    let mut entry_paths = Vec::new();
    if let Ok(mut dir) = tokio::fs::read_dir(&entries_dir).await {
        while let Ok(Some(entry)) = dir.next_entry().await {
            if entry
                .path()
                .extension()
                .map(|e| e.to_string_lossy() == "conf")
                .unwrap_or(false)
            {
                entry_paths.push(entry.path());
            }
        }
    }
    entry_paths.sort();

    let active_entry = tokio::fs::read_to_string(&loader_conf)
        .await
        .ok()
        .and_then(|content| parse_systemd_boot_default(&content));

    Some(BootloaderInfo {
        kind: BootloaderKind::SystemdBoot,
        config_path: Some(loader_conf),
        entry_paths,
        active_entry,
        update_command: None,
        is_uefi,
    })
}

pub fn parse_systemd_boot_default(loader_conf: &str) -> Option<String> {
    for line in loader_conf.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let mut parts = line.split_whitespace();
        if parts.next() == Some("default") {
            return parts.next().map(str::to_string);
        }
    }
    None
}

async fn detect_refind(root: &Path, is_uefi: bool) -> Option<BootloaderInfo> {
    if !is_uefi {
        return None;
    }

    let candidates = [
        "/boot/EFI/refind/refind.conf",
        "/boot/efi/EFI/refind/refind.conf",
        "/efi/EFI/refind/refind.conf",
    ];

    for candidate in candidates {
        let path = rooted(root, candidate);
        if path.exists() {
            return Some(BootloaderInfo {
                kind: BootloaderKind::Refind,
                config_path: Some(path),
                entry_paths: Vec::new(),
                active_entry: None,
                update_command: None,
                is_uefi,
            });
        }
    }
    None
}

async fn detect_syslinux(root: &Path, is_uefi: bool) -> Option<BootloaderInfo> {
    let candidates = [
        "/boot/syslinux/syslinux.cfg",
        "/boot/extlinux/extlinux.conf",
        "/syslinux/syslinux.cfg",
    ];

    for candidate in candidates {
        let path = rooted(root, candidate);
        if path.exists() {
            return Some(BootloaderInfo {
                kind: BootloaderKind::Syslinux,
                config_path: Some(path),
                entry_paths: Vec::new(),
                active_entry: None,
                update_command: None,
                is_uefi,
            });
        }
    }
    None
}

async fn detect_efistub() -> Option<BootloaderInfo> {
    let output = tokio::process::Command::new("efibootmgr")
        .arg("-v")
        .output()
        .await
        .ok()?;

    if output.status.success() {
        Some(BootloaderInfo {
            kind: BootloaderKind::Efistub,
            config_path: None,
            entry_paths: Vec::new(),
            active_entry: None,
            update_command: Some("efibootmgr".to_string()),
            is_uefi: true,
        })
    } else {
        None
    }
}

fn rooted(root: &Path, absolute_path: &str) -> PathBuf {
    root.join(absolute_path.trim_start_matches('/'))
}
