use crate::detect::bootloader::BootloaderInfo;

#[derive(Debug, Clone)]
pub struct KernelParamPlan {
    pub params: Vec<String>,
    pub target: Option<String>,
}

pub fn plan_iommu_params(bootloader: &BootloaderInfo, cpu_vendor: &str) -> KernelParamPlan {
    let iommu_param = if cpu_vendor.contains("AMD") || cpu_vendor == "AuthenticAMD" {
        "amd_iommu=on"
    } else {
        "intel_iommu=on"
    };

    KernelParamPlan {
        params: vec![iommu_param.to_string(), "iommu=pt".to_string()],
        target: bootloader
            .config_path
            .as_ref()
            .map(|path| path.display().to_string()),
    }
}
