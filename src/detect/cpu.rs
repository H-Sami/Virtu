use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use tracing::debug;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpuInfo {
    pub vendor: String,
    pub model_name: String,
    pub physical_cores: u32,
    pub logical_cores: u32,
    pub numa_nodes: Vec<NumaNode>,
    pub iommu_capable: bool,
    pub iommu_enabled: bool,
    pub has_hyperthreading: bool,
    /// Map of stable physical core index -> list of logical CPU ids on that core.
    pub core_to_threads: HashMap<u32, Vec<u32>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NumaNode {
    pub id: u32,
    pub cpu_list: Vec<u32>,
    pub memory_mb: u64,
}

pub async fn detect() -> Result<CpuInfo> {
    debug!("Detecting CPU information");

    let cpuinfo = tokio::fs::read_to_string("/proc/cpuinfo")
        .await
        .context("Cannot read /proc/cpuinfo")?;

    let iommu_enabled = check_iommu_active_in_kernel().await;
    let numa_nodes = detect_numa_nodes().await;

    Ok(parse_cpuinfo(&cpuinfo, iommu_enabled, numa_nodes))
}

pub fn parse_cpuinfo(cpuinfo: &str, iommu_enabled: bool, numa_nodes: Vec<NumaNode>) -> CpuInfo {
    let vendor = parse_cpuinfo_field(cpuinfo, "vendor_id")
        .or_else(|| parse_cpuinfo_field(cpuinfo, "CPU implementer"))
        .unwrap_or_else(|| "unknown".to_string());

    let model_name = parse_cpuinfo_field(cpuinfo, "model name")
        .or_else(|| parse_cpuinfo_field(cpuinfo, "Hardware"))
        .or_else(|| parse_cpuinfo_field(cpuinfo, "Processor"))
        .unwrap_or_else(|| "Unknown CPU".to_string());

    let flags = parse_cpuinfo_field(cpuinfo, "flags")
        .or_else(|| parse_cpuinfo_field(cpuinfo, "Features"))
        .unwrap_or_default();

    let iommu_capable = flags
        .split_whitespace()
        .any(|flag| flag == "vmx" || flag == "svm");
    let core_to_threads = build_core_thread_map(cpuinfo);
    let physical_cores = core_to_threads.len() as u32;
    let logical_cores: u32 = core_to_threads.values().map(|v| v.len() as u32).sum();
    let has_hyperthreading = core_to_threads.values().any(|threads| threads.len() > 1);

    CpuInfo {
        vendor,
        model_name,
        physical_cores,
        logical_cores,
        numa_nodes,
        iommu_capable,
        iommu_enabled,
        has_hyperthreading,
        core_to_threads,
    }
}

fn parse_cpuinfo_field(cpuinfo: &str, field: &str) -> Option<String> {
    for line in cpuinfo.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.trim() == field {
            return Some(value.trim().to_string());
        }
    }
    None
}

/// Build a stable map from physical cores to logical CPU thread IDs.
///
/// Linux `core id` repeats across sockets, so this uses `(physical id, core id)`
/// internally and then emits compact stable indexes for downstream VM pinning.
pub fn build_core_thread_map(cpuinfo: &str) -> HashMap<u32, Vec<u32>> {
    let mut physical_cores: BTreeMap<(u32, u32), Vec<u32>> = BTreeMap::new();
    let mut current_processor: Option<u32> = None;
    let mut current_physical_id: Option<u32> = None;
    let mut current_core_id: Option<u32> = None;

    for line in cpuinfo.lines() {
        if line.trim().is_empty() {
            record_cpu_block(
                &mut physical_cores,
                current_processor.take(),
                current_physical_id.take(),
                current_core_id.take(),
            );
        } else if let Some(v) = line.strip_prefix("processor") {
            current_processor = parse_colon_u32(v);
        } else if let Some(v) = line.strip_prefix("physical id") {
            current_physical_id = parse_colon_u32(v);
        } else if let Some(v) = line.strip_prefix("core id") {
            current_core_id = parse_colon_u32(v);
        }
    }

    record_cpu_block(
        &mut physical_cores,
        current_processor,
        current_physical_id,
        current_core_id,
    );

    if physical_cores.is_empty() {
        let mut map = HashMap::new();
        let count = cpuinfo
            .lines()
            .filter(|line| line.trim_start().starts_with("processor"))
            .count() as u32;
        for i in 0..count {
            map.insert(i, vec![i]);
        }
        return map;
    }

    let mut map = HashMap::new();
    for (idx, (_physical_key, mut threads)) in physical_cores.into_iter().enumerate() {
        threads.sort_unstable();
        map.insert(idx as u32, threads);
    }

    map
}

fn record_cpu_block(
    map: &mut BTreeMap<(u32, u32), Vec<u32>>,
    processor: Option<u32>,
    physical_id: Option<u32>,
    core_id: Option<u32>,
) {
    if let (Some(proc), Some(core)) = (processor, core_id) {
        map.entry((physical_id.unwrap_or(0), core))
            .or_default()
            .push(proc);
    }
}

fn parse_colon_u32(s: &str) -> Option<u32> {
    s.split(':').nth(1)?.trim().parse().ok()
}

async fn check_iommu_active_in_kernel() -> bool {
    if let Ok(mut dir) = tokio::fs::read_dir("/sys/kernel/iommu_groups").await {
        return dir.next_entry().await.map(|e| e.is_some()).unwrap_or(false);
    }
    false
}

async fn detect_numa_nodes() -> Vec<NumaNode> {
    let numa_path = "/sys/devices/system/node";
    let mut nodes = Vec::new();

    let dir = match tokio::fs::read_dir(numa_path).await {
        Ok(d) => d,
        Err(_) => return nodes,
    };

    let mut dir = dir;
    while let Ok(Some(entry)) = dir.next_entry().await {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !name_str.starts_with("node") {
            continue;
        }
        let id: u32 = match name_str[4..].parse() {
            Ok(v) => v,
            Err(_) => continue,
        };

        let cpu_list_path = entry.path().join("cpulist");
        let cpu_list_str = tokio::fs::read_to_string(&cpu_list_path)
            .await
            .unwrap_or_default();
        let cpu_list = parse_cpu_list(&cpu_list_str);

        let mem_path = entry.path().join("meminfo");
        let mem_str = tokio::fs::read_to_string(&mem_path)
            .await
            .unwrap_or_default();
        let memory_mb = parse_node_memory_mb(&mem_str);

        nodes.push(NumaNode {
            id,
            cpu_list,
            memory_mb,
        });
    }

    nodes.sort_by_key(|n| n.id);
    nodes
}

/// Parse a CPU list string like `0-3,8-11` into logical CPU ids.
pub fn parse_cpu_list(s: &str) -> Vec<u32> {
    let mut result = Vec::new();
    for part in s.trim().split(',') {
        if part.contains('-') {
            let mut range = part.split('-');
            if let (Some(start), Some(end)) = (range.next(), range.next()) {
                if let (Ok(a), Ok(b)) = (start.trim().parse::<u32>(), end.trim().parse::<u32>()) {
                    result.extend(a..=b);
                }
            }
        } else if let Ok(n) = part.trim().parse::<u32>() {
            result.push(n);
        }
    }
    result
}

fn parse_node_memory_mb(meminfo: &str) -> u64 {
    for line in meminfo.lines() {
        if line.contains("MemTotal") {
            if let Some(kb_str) = line.split_whitespace().rev().nth(1) {
                if let Ok(kb) = kb_str.parse::<u64>() {
                    return kb / 1024;
                }
            }
        }
    }
    0
}
