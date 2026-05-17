pub mod schema;

use crate::detect::distro::{DistroFamily, DistroInfo};
use schema::{DistroPaths, ErrorPattern, GpuQuirk};

#[derive(Debug, Clone)]
pub struct KnowledgeBase {
    generic_paths: DistroPaths,
    arch_paths: DistroPaths,
    debian_paths: DistroPaths,
    fedora_paths: DistroPaths,
    opensuse_paths: DistroPaths,
    gpu_quirks: Vec<GpuQuirk>,
    error_patterns: Vec<ErrorPattern>,
}

impl Default for KnowledgeBase {
    fn default() -> Self {
        Self {
            generic_paths: DistroPaths::generic(),
            arch_paths: DistroPaths {
                ovmf_code: Some("/usr/share/edk2/x64/OVMF_CODE.fd".to_string()),
                ovmf_vars: Some("/usr/share/edk2/x64/OVMF_VARS.fd".to_string()),
                qemu_binary: "/usr/bin/qemu-system-x86_64".to_string(),
                libvirt_images_dir: "/var/lib/libvirt/images".to_string(),
            },
            debian_paths: DistroPaths {
                ovmf_code: Some("/usr/share/OVMF/OVMF_CODE.fd".to_string()),
                ovmf_vars: Some("/usr/share/OVMF/OVMF_VARS.fd".to_string()),
                qemu_binary: "/usr/bin/qemu-system-x86_64".to_string(),
                libvirt_images_dir: "/var/lib/libvirt/images".to_string(),
            },
            fedora_paths: DistroPaths {
                ovmf_code: Some("/usr/share/edk2/ovmf/OVMF_CODE.fd".to_string()),
                ovmf_vars: Some("/usr/share/edk2/ovmf/OVMF_VARS.fd".to_string()),
                qemu_binary: "/usr/bin/qemu-system-x86_64".to_string(),
                libvirt_images_dir: "/var/lib/libvirt/images".to_string(),
            },
            opensuse_paths: DistroPaths {
                ovmf_code: Some("/usr/share/qemu/ovmf-x86_64-code.bin".to_string()),
                ovmf_vars: Some("/usr/share/qemu/ovmf-x86_64-vars.bin".to_string()),
                qemu_binary: "/usr/bin/qemu-system-x86_64".to_string(),
                libvirt_images_dir: "/var/lib/libvirt/images".to_string(),
            },
            gpu_quirks: Vec::new(),
            error_patterns: Vec::new(),
        }
    }
}

impl KnowledgeBase {
    pub fn bundled() -> Self {
        Self::default()
    }

    pub fn paths_for_distro(&self, distro: &DistroInfo) -> &DistroPaths {
        match distro.family {
            DistroFamily::Arch => &self.arch_paths,
            DistroFamily::Debian | DistroFamily::Ubuntu => &self.debian_paths,
            DistroFamily::Fedora | DistroFamily::Rhel => &self.fedora_paths,
            DistroFamily::OpenSuse => &self.opensuse_paths,
            _ => &self.generic_paths,
        }
    }

    pub fn quirks_for_gpu(&self, vendor_id: &str, device_id: &str) -> Vec<&GpuQuirk> {
        self.gpu_quirks
            .iter()
            .filter(|quirk| {
                quirk.vendor_id.eq_ignore_ascii_case(vendor_id)
                    && (quirk.device_id_pattern == "*"
                        || quirk.device_id_pattern.eq_ignore_ascii_case(device_id))
            })
            .collect()
    }

    pub fn error_patterns(&self) -> &[ErrorPattern] {
        &self.error_patterns
    }
}
