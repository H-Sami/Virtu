use crate::detect::CpuInfo;

/// Result of CPU pinning calculation
#[derive(Debug)]
pub struct PinningPlan {
    /// Vec of (vcpu_index, cpuset_string)
    pub vcpu_pins: Vec<(u32, String)>,
    /// cpuset string for emulator and iothreads
    pub emulator_cpuset: String,
    /// Physical cores assigned to the VM
    pub vm_cores: Vec<u32>,
    /// Physical cores reserved for host/emulator
    pub host_cores: Vec<u32>,
}

/// Calculate CPU pinning based on the physical topology and requested vCPU count.
///
/// Strategy:
/// - Assign whole physical cores (both HT threads) to the VM
/// - Reserve at least 1-2 physical cores for the host (emulator + OS)
/// - Never split a physical core between VM and emulator
///
/// Example: 8-core/16-thread CPU, 6 vCPUs requested (3 physical cores × 2 threads)
///   VM gets: cores 0,1,2 → threads 0,8,1,9,2,10
///   Host gets: cores 6,7 → threads 6,14,7,15
pub fn calculate_pinning(cpu: &CpuInfo, vcpu_count: u32) -> PinningPlan {
    let threads_per_core = if cpu.has_hyperthreading { 2 } else { 1 };
    let physical_cores_for_vm = (vcpu_count / threads_per_core).max(1) as usize;

    // Sort physical core IDs for deterministic assignment
    let mut all_core_ids: Vec<u32> = cpu.core_to_threads.keys().copied().collect();
    all_core_ids.sort_unstable();

    // Reserve the last 1-2 physical cores for the host
    let host_reserve = if all_core_ids.len() >= 8 { 2 } else { 1 };
    let vm_core_count = all_core_ids
        .len()
        .saturating_sub(host_reserve)
        .min(physical_cores_for_vm);

    let vm_cores: Vec<u32> = all_core_ids[..vm_core_count].to_vec();
    let host_cores: Vec<u32> = all_core_ids[vm_core_count..].to_vec();

    // Build vcpu pins
    let mut vcpu_pins = Vec::new();
    let mut vcpu_idx = 0u32;

    for &core_id in &vm_cores {
        if let Some(threads) = cpu.core_to_threads.get(&core_id) {
            for &thread_id in threads {
                if vcpu_idx < vcpu_count {
                    vcpu_pins.push((vcpu_idx, thread_id.to_string()));
                    vcpu_idx += 1;
                }
            }
        }
    }

    // Build emulator cpuset from host cores
    let host_thread_ids: Vec<String> = host_cores
        .iter()
        .flat_map(|&core_id| {
            cpu.core_to_threads
                .get(&core_id)
                .cloned()
                .unwrap_or_default()
        })
        .map(|t| t.to_string())
        .collect();

    let emulator_cpuset = if host_thread_ids.is_empty() {
        // Fallback: use all threads (shouldn't happen)
        format!("0-{}", cpu.logical_cores.saturating_sub(1))
    } else {
        compress_cpu_list(&host_thread_ids)
    };

    PinningPlan {
        vcpu_pins,
        emulator_cpuset,
        vm_cores,
        host_cores,
    }
}

/// Convert a list of CPU ids into a compact range string.
/// e.g. ["6", "7", "14", "15"] → "6-7,14-15"
fn compress_cpu_list(ids: &[String]) -> String {
    let mut nums: Vec<u32> = ids.iter().filter_map(|s| s.parse().ok()).collect();
    nums.sort_unstable();
    nums.dedup();

    if nums.is_empty() {
        return String::new();
    }

    let mut ranges = Vec::new();
    let mut start = nums[0];
    let mut end = nums[0];

    for &n in &nums[1..] {
        if n == end + 1 {
            end = n;
        } else {
            ranges.push(if start == end {
                format!("{start}")
            } else {
                format!("{start}-{end}")
            });
            start = n;
            end = n;
        }
    }
    ranges.push(if start == end {
        format!("{start}")
    } else {
        format!("{start}-{end}")
    });

    ranges.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_cpu(cores: u32, ht: bool) -> CpuInfo {
        let mut core_to_threads = HashMap::new();
        for c in 0..cores {
            let threads = if ht { vec![c, c + cores] } else { vec![c] };
            core_to_threads.insert(c, threads);
        }
        CpuInfo {
            vendor: "GenuineIntel".to_string(),
            model_name: "Test CPU".to_string(),
            physical_cores: cores,
            logical_cores: cores * if ht { 2 } else { 1 },
            numa_nodes: Vec::new(),
            iommu_capable: true,
            iommu_enabled: true,
            has_hyperthreading: ht,
            core_to_threads,
        }
    }

    #[test]
    fn test_8core_ht_6vcpu() {
        let cpu = make_cpu(8, true);
        let plan = calculate_pinning(&cpu, 6);
        assert_eq!(plan.vcpu_pins.len(), 6);
        // Should use 3 physical cores (0,1,2)
        assert_eq!(plan.vm_cores.len(), 3);
        // Emulator should have host cores
        assert!(!plan.emulator_cpuset.is_empty());
    }

    #[test]
    fn test_compress_cpu_list_ranges() {
        let ids: Vec<String> = ["6", "7", "14", "15"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(compress_cpu_list(&ids), "6-7,14-15");
    }

    #[test]
    fn test_compress_single() {
        let ids: Vec<String> = ["4"].iter().map(|s| s.to_string()).collect();
        assert_eq!(compress_cpu_list(&ids), "4");
    }
}
