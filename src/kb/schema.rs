use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistroPaths {
    pub ovmf_code: Option<String>,
    pub ovmf_vars: Option<String>,
    pub qemu_binary: String,
    pub libvirt_images_dir: String,
}

impl DistroPaths {
    pub fn generic() -> Self {
        Self {
            ovmf_code: Some("/usr/share/OVMF/OVMF_CODE.fd".to_string()),
            ovmf_vars: Some("/usr/share/OVMF/OVMF_VARS.fd".to_string()),
            qemu_binary: "/usr/bin/qemu-system-x86_64".to_string(),
            libvirt_images_dir: "/var/lib/libvirt/images".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuQuirk {
    pub issue_id: String,
    pub vendor_id: String,
    pub device_id_pattern: String,
    pub description: String,
    pub fixes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorPattern {
    pub id: String,
    pub regex: String,
    pub cause: String,
    pub fix_options: Vec<String>,
}

/// Wrapper TOML schema for the bundled `gpu_quirks.toml` file. Kept
/// separate from [`GpuQuirk`] so the file format is forward-compatible:
/// adding new top-level keys (`schema_version`, etc.) does not require
/// rewriting every quirk.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GpuQuirksFile {
    #[serde(default)]
    pub quirks: Vec<GpuQuirk>,
}

/// Wrapper TOML schema for the bundled `error_patterns.toml` file. See
/// [`GpuQuirksFile`] for the rationale.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ErrorPatternsFile {
    #[serde(default)]
    pub patterns: Vec<ErrorPattern>,
}
