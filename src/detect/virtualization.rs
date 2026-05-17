// src/detect/virtualization.rs
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtInfo {
    pub qemu_version: Option<String>,
    pub libvirt_version: Option<String>,
    pub virsh_available: bool,
    pub virt_manager_available: bool,
    pub libvirtd_running: bool,
}

pub async fn detect() -> Result<VirtInfo> {
    let qemu_version = get_command_version("qemu-system-x86_64", &["--version"]).await;
    let libvirt_version = get_command_version("virsh", &["--version"]).await;
    let virsh_available = which::which("virsh").is_ok();
    let virt_manager_available = which::which("virt-manager").is_ok();

    let libvirtd_running = tokio::process::Command::new("systemctl")
        .args(["is-active", "--quiet", "libvirtd"])
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false);

    Ok(VirtInfo {
        qemu_version,
        libvirt_version,
        virsh_available,
        virt_manager_available,
        libvirtd_running,
    })
}

async fn get_command_version(cmd: &str, args: &[&str]) -> Option<String> {
    if which::which(cmd).is_err() {
        return None;
    }
    let output = tokio::process::Command::new(cmd)
        .args(args)
        .output()
        .await
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.lines().next().map(|l| l.trim().to_string())
}
