// detect/initramfs.rs
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InitramfsSystem {
    Mkinitcpio,      // Arch Linux
    Dracut,          // Fedora, RHEL, OpenSUSE
    UpdateInitramfs, // Debian, Ubuntu
    Unknown,
}

impl InitramfsSystem {
    pub fn name(&self) -> &str {
        match self {
            InitramfsSystem::Mkinitcpio => "mkinitcpio (Arch)",
            InitramfsSystem::Dracut => "dracut (Fedora/RHEL/OpenSUSE)",
            InitramfsSystem::UpdateInitramfs => "update-initramfs (Debian/Ubuntu)",
            InitramfsSystem::Unknown => "Unknown",
        }
    }

    pub fn rebuild_command(&self) -> &str {
        match self {
            InitramfsSystem::Mkinitcpio => "mkinitcpio -P",
            InitramfsSystem::Dracut => "dracut --force",
            InitramfsSystem::UpdateInitramfs => "update-initramfs -u -k all",
            InitramfsSystem::Unknown => "",
        }
    }
}

pub async fn detect() -> Result<InitramfsSystem> {
    detect_from_root(Path::new("/"), true).await
}

pub async fn detect_from_root(
    root: impl AsRef<Path>,
    allow_command_lookup: bool,
) -> Result<InitramfsSystem> {
    let root = root.as_ref();

    if rooted(root, "/etc/mkinitcpio.conf").exists() {
        return Ok(InitramfsSystem::Mkinitcpio);
    }
    if rooted(root, "/etc/dracut.conf").exists()
        || rooted(root, "/etc/dracut.conf.d").exists()
        || (allow_command_lookup && which::which("dracut").is_ok())
    {
        return Ok(InitramfsSystem::Dracut);
    }
    if rooted(root, "/etc/initramfs-tools").exists() {
        return Ok(InitramfsSystem::UpdateInitramfs);
    }
    Ok(InitramfsSystem::Unknown)
}

fn rooted(root: &Path, absolute_path: &str) -> std::path::PathBuf {
    root.join(absolute_path.trim_start_matches('/'))
}
