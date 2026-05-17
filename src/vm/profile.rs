use crate::detect::{GpuInfo, MonitorInfo, UsbDevice};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The complete set of user-chosen VM configuration options.
/// This is built up through the TUI wizard and then passed to the XML builder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmProfile {
    // ─── Identity ────────────────────────────────────────────────────────────
    pub vm_name: String,
    pub guest_os: GuestOs,

    // ─── Resources ───────────────────────────────────────────────────────────
    /// RAM for the VM in megabytes
    pub ram_mb: u64,
    /// Number of vCPUs
    pub vcpu_count: u32,
    /// Disk image path (created or existing)
    pub disk_path: PathBuf,
    /// Disk size in GB (used only when creating a new image)
    pub disk_size_gb: u64,
    /// Whether the disk image already exists (vs create new)
    pub disk_exists: bool,

    // ─── GPU Passthrough ─────────────────────────────────────────────────────
    /// The GPU being passed through to the VM
    pub passthrough_gpu: GpuInfo,
    /// The GPU remaining on the host
    pub host_gpu: GpuInfo,
    /// Passthrough mode selected
    pub passthrough_mode: PassthroughMode,

    // ─── ISO / Installation ──────────────────────────────────────────────────
    /// Path to the installation ISO (if selected)
    pub iso_path: Option<PathBuf>,

    // ─── Display ─────────────────────────────────────────────────────────────
    pub looking_glass: LookingGlassConfig,
    /// Which monitor the VM will output to (if not using Looking Glass)
    pub vm_monitor: Option<MonitorInfo>,
    /// The monitor the host Linux desktop will use
    pub host_monitor: Option<MonitorInfo>,

    // ─── Input Devices ───────────────────────────────────────────────────────
    pub evdev_keyboard: Option<UsbDevice>,
    pub evdev_mouse: Option<UsbDevice>,
    pub additional_evdev: Vec<UsbDevice>,

    // ─── Performance ─────────────────────────────────────────────────────────
    pub use_hugepages: bool,
    pub use_cpu_pinning: bool,
    pub use_iothreads: bool,

    // ─── Guest OS Extras ─────────────────────────────────────────────────────
    pub enable_tpm: bool,    // Auto-set for Windows 11
    pub enable_hyperv: bool, // Auto-set for Windows guests
    pub enable_secure_boot: bool,

    // ─── Audio ───────────────────────────────────────────────────────────────
    pub audio_passthrough: AudioPassthroughMethod,

    // ─── Network ─────────────────────────────────────────────────────────────
    pub network_type: NetworkType,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GuestOs {
    Windows7,
    Windows10,
    Windows11,
    Linux,
    Other,
}

impl GuestOs {
    pub fn requires_tpm(&self) -> bool {
        matches!(self, GuestOs::Windows11)
    }

    pub fn benefits_from_hyperv(&self) -> bool {
        matches!(
            self,
            GuestOs::Windows7 | GuestOs::Windows10 | GuestOs::Windows11
        )
    }

    pub fn display_name(&self) -> &str {
        match self {
            GuestOs::Windows7 => "Windows 7",
            GuestOs::Windows10 => "Windows 10",
            GuestOs::Windows11 => "Windows 11",
            GuestOs::Linux => "Linux",
            GuestOs::Other => "Other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PassthroughMode {
    /// Two GPUs: one for VM, one stays on host permanently
    DualGpu,
    /// One GPU: hooks needed to hand off between host and VM
    SingleGpu,
    /// iGPU stays on host, dGPU goes to VM
    IgpuHost,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LookingGlassConfig {
    pub enabled: bool,
    /// Whether to auto-download and compile the LG client
    pub auto_compile: bool,
    /// Target resolution for buffer size calculation
    pub target_width: u32,
    pub target_height: u32,
    /// Calculated IVSHMEM buffer size in MB (power of 2)
    pub buffer_size_mb: u64,
}

impl LookingGlassConfig {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            auto_compile: false,
            target_width: 1920,
            target_height: 1080,
            buffer_size_mb: 0,
        }
    }

    pub fn calculate_buffer_size(width: u32, height: u32) -> u64 {
        // Raw bytes: width * height * 4 bytes (BGRA) * 2 (double buffer)
        let raw = width as u64 * height as u64 * 4 * 2;
        let with_overhead = raw + (2 * 1024 * 1024); // 2 MB overhead
                                                     // Round up to next power of 2, minimum 32 MB
        let pow2 = with_overhead.next_power_of_two();
        let min = 32 * 1024 * 1024u64;
        // Convert to MB
        pow2.max(min) / (1024 * 1024)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AudioPassthroughMethod {
    /// PipeWire/PulseAudio backend (best quality)
    HostAudio,
    /// Scream virtual network audio (alternative)
    Scream,
    /// No audio passthrough
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NetworkType {
    /// NAT via virbr0 — simplest, works out of the box
    Nat,
    /// Bridge to physical interface — VM gets its own IP
    Bridge { interface: String },
}

impl VmProfile {
    /// Apply automatic settings based on guest OS selection.
    pub fn apply_os_defaults(&mut self) {
        self.enable_tpm = self.guest_os.requires_tpm();
        self.enable_hyperv = self.guest_os.benefits_from_hyperv();
        self.enable_secure_boot = matches!(self.guest_os, GuestOs::Windows11);
    }
}
