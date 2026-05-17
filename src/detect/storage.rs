// src/detect/storage.rs
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageInfo {
    /// Default directory for VM disk images
    pub default_vm_dir: PathBuf,
    /// Available bytes in the default VM directory
    pub available_bytes: u64,
}

impl StorageInfo {
    pub fn available_gb(&self) -> u64 {
        self.available_bytes / 1_073_741_824
    }
}

pub async fn detect() -> Result<StorageInfo> {
    detect_from_root(Path::new("/"), true).await
}

pub async fn detect_from_root(
    root: impl AsRef<Path>,
    allow_df_command: bool,
) -> Result<StorageInfo> {
    let root = root.as_ref();
    let default_vm_dir = rooted(root, "/var/lib/libvirt/images");

    let fixture_available = default_vm_dir.join(".available-bytes");
    if let Ok(content) = tokio::fs::read_to_string(fixture_available).await {
        let available_bytes = content.trim().parse::<u64>().unwrap_or(0);
        return Ok(StorageInfo {
            default_vm_dir,
            available_bytes,
        });
    }

    let available_bytes = if allow_df_command {
        get_available_bytes(&default_vm_dir).await.unwrap_or(0)
    } else {
        0
    };

    Ok(StorageInfo {
        default_vm_dir,
        available_bytes,
    })
}

pub fn parse_df_available_bytes(stdout: &str) -> u64 {
    stdout
        .lines()
        .skip(1)
        .find_map(|line| line.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

async fn get_available_bytes(path: &Path) -> Result<u64> {
    let path = if path.exists() {
        path.to_path_buf()
    } else {
        PathBuf::from("/var/lib")
    };

    let output = tokio::process::Command::new("df")
        .args(["-B1", "--output=avail", path.to_str().unwrap_or("/")])
        .output()
        .await?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_df_available_bytes(&stdout))
}

fn rooted(root: &Path, absolute_path: &str) -> PathBuf {
    root.join(absolute_path.trim_start_matches('/'))
}
