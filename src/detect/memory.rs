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

    /// Recommended VM RAM: half of total, min 4GB, max (total - 4GB)
    pub fn recommended_vm_ram_mb(&self) -> u64 {
        let total_mb = self.total_kb / 1024;
        let half = total_mb / 2;
        let min = 4096u64;
        let max = total_mb.saturating_sub(4096);
        half.clamp(min, max)
    }

    /// Calculate hugepages needed for a given VM RAM in MB
    pub fn hugepages_for_vm_ram(&self, vm_ram_mb: u64) -> u64 {
        if self.hugepage_size_kb == 0 {
            return 0;
        }
        let hugepage_mb = self.hugepage_size_kb / 1024;
        if hugepage_mb == 0 {
            return 0;
        }
        (vm_ram_mb + hugepage_mb - 1) / hugepage_mb + 1 // +1 for overhead
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
