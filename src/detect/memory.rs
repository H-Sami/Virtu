// src/detect/memory.rs
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemInfo {
    pub total_kb: u64,
    pub available_kb: u64,
    pub hugepage_size_kb: u64,
    pub hugepages_total: u64,
    pub hugepages_free: u64,
}

impl MemInfo {
    pub fn total_gb(&self) -> u64 {
        self.total_kb / 1_048_576
    }
    pub fn available_gb(&self) -> u64 {
        self.available_kb / 1_048_576
    }
    pub fn hugepage_eligible_gb(&self) -> u64 {
        self.available_kb / 1_048_576
    }

    /// Recommended VM RAM in MiB.
    ///
    /// Aims for half of host RAM, with a 4 GiB floor and a 4 GiB host
    /// reserve. On hosts with too little RAM to satisfy both bounds, the host
    /// reserve wins so the host stays usable; the floor is treated as a
    /// suggestion, not a guarantee. Returns 0 only when total RAM is 0
    /// (e.g. an empty fixture).
    pub fn recommended_vm_ram_mb(&self) -> u64 {
        let total_mb = self.total_kb / 1024;
        if total_mb == 0 {
            return 0;
        }

        let host_reserve_mb = 4096u64;
        let preferred_floor = 4096u64;

        // Always leave at least the host reserve for the host. If the host
        // has less than the reserve, give it everything anyway and return 0.
        let max_for_vm = total_mb.saturating_sub(host_reserve_mb);
        if max_for_vm == 0 {
            return 0;
        }

        let half = total_mb / 2;
        // Prefer half-of-RAM, but never exceed `total - reserve`.
        let target = half.min(max_for_vm);
        // Bring it up toward the floor only when there is room.
        target.max(preferred_floor.min(max_for_vm))
    }
}

pub async fn detect() -> Result<MemInfo> {
    let content = tokio::fs::read_to_string("/proc/meminfo")
        .await
        .context("Cannot read /proc/meminfo")?;

    let mut meminfo = parse_meminfo(&content);

    // Prefer 1GB hugepages when the kernel exposes that size.
    let gb_hp = tokio::fs::read_dir("/sys/kernel/mm/hugepages").await;
    if let Ok(mut dir) = gb_hp {
        while let Ok(Some(entry)) = dir.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.contains("1048576") {
                meminfo.hugepage_size_kb = 1_048_576;
            }
        }
    }

    Ok(meminfo)
}

pub fn parse_meminfo(content: &str) -> MemInfo {
    let mut total_kb = 0u64;
    let mut available_kb = 0u64;
    let mut hugepage_size_kb = 2048u64; // Default 2MB
    let mut hugepages_total = 0u64;
    let mut hugepages_free = 0u64;

    for line in content.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }
        match parts[0] {
            "MemTotal:" => total_kb = parts[1].parse().unwrap_or(0),
            "MemAvailable:" => available_kb = parts[1].parse().unwrap_or(0),
            "Hugepagesize:" => hugepage_size_kb = parts[1].parse().unwrap_or(2048),
            "HugePages_Total:" => hugepages_total = parts[1].parse().unwrap_or(0),
            "HugePages_Free:" => hugepages_free = parts[1].parse().unwrap_or(0),
            _ => {}
        }
    }

    MemInfo {
        total_kb,
        available_kb,
        hugepage_size_kb,
        hugepages_total,
        hugepages_free,
    }
}
