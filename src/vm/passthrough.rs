//! User-choice model for GPU passthrough planning.
//!
//! [`PassthroughConfig`] sits between the immutable [`SystemProfile`](crate::detect::SystemProfile)
//! / [`CompatibilityReport`](crate::engine::CompatibilityReport) layer and the
//! later VM/XML/planner layers. It captures every choice the user makes in the
//! wizard before any plan is generated, so validation can reason about user
//! intent without touching the host.
//!
//! This module is intentionally read-only. It must not perform I/O or mutate
//! system state.

use crate::detect::audio::AudioSystem;
use crate::detect::gpu::{GpuInfo, GpuType};
use crate::detect::SystemProfile;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Complete set of user choices that drive a Virtu passthrough plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PassthroughConfig {
    /// libvirt domain name. Used as the registered VM name and as the
    /// basename for generated artifacts (`<vm_name>.xml`,
    /// `<vm_name>.qcow2`). Must be unique across the host's existing
    /// libvirt domains and must match libvirt's accepted character set.
    pub vm_name: String,

    /// Guest operating system target. The current default is Windows 11
    /// because Virtu v1 is optimized for Windows gaming/workstation guests,
    /// but later wizard/CLI work can expose this directly to the user.
    #[serde(default = "default_guest_os")]
    pub guest_os: GuestOs,

    /// Overall passthrough strategy. The `gpu_roles` list must be consistent
    /// with this mode. See [`PassthroughConfig::derived_mode`] for the
    /// implied mode based on `gpu_roles` alone.
    pub gpu_mode: GpuPassthroughMode,

    /// Per-GPU role assignments. There is one entry per detected GPU. GPUs
    /// the user wants Virtu to ignore are recorded with [`GpuRole::Ignored`]
    /// and must not be touched by any later plan.
    pub gpu_roles: Vec<GpuRoleAssignment>,

    /// Monitor workflow plan.
    pub monitor_plan: MonitorPlan,

    /// Looking Glass selection.
    pub looking_glass: LookingGlassChoice,

    /// Optional path to an installation ISO (for example a Windows ISO).
    pub iso_path: Option<PathBuf>,

    /// VM resource selections (RAM, vCPUs, disk).
    pub resources: VmResources,

    /// Networking choice.
    pub network: NetworkChoice,

    /// Audio backend choice.
    pub audio: AudioChoice,

    /// Evdev keyboard/mouse passthrough selections.
    pub input: InputChoice,
}

/// Guest OS target for generated libvirt XML.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GuestOs {
    Windows7,
    Windows10,
    Windows11,
    Linux,
    Other,
}

impl GuestOs {
    pub fn requires_tpm(self) -> bool {
        matches!(self, GuestOs::Windows11)
    }

    pub fn benefits_from_hyperv(self) -> bool {
        matches!(
            self,
            GuestOs::Windows7 | GuestOs::Windows10 | GuestOs::Windows11
        )
    }

    pub fn enables_secure_boot_by_default(self) -> bool {
        matches!(self, GuestOs::Windows11)
    }

    pub fn display_name(self) -> &'static str {
        match self {
            GuestOs::Windows7 => "Windows 7",
            GuestOs::Windows10 => "Windows 10",
            GuestOs::Windows11 => "Windows 11",
            GuestOs::Linux => "Linux",
            GuestOs::Other => "Other",
        }
    }
}

fn default_guest_os() -> GuestOs {
    GuestOs::Windows11
}

/// Overall GPU passthrough strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GpuPassthroughMode {
    /// Two or more GPUs: one stays on the host, one is dedicated to the VM.
    DualGpu,
    /// iGPU drives the host display, dGPU is dedicated to the VM.
    IgpuHost,
    /// Only one GPU is available (or the user chose single-GPU even with
    /// other GPUs present). Requires display-manager-aware hooks.
    SingleGpu,
    /// More than one GPU is being passed through to the VM. Reserved for a
    /// future multi-GPU automation path.
    MultiGpu,
}

impl std::fmt::Display for GpuPassthroughMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            GpuPassthroughMode::DualGpu => "dual GPU",
            GpuPassthroughMode::IgpuHost => "iGPU host + dGPU passthrough",
            GpuPassthroughMode::SingleGpu => "single GPU",
            GpuPassthroughMode::MultiGpu => "multi-GPU passthrough",
        };
        write!(f, "{s}")
    }
}

/// Role of a single detected GPU.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuRoleAssignment {
    /// PCI slot of the GPU, e.g. `0000:01:00.0`.
    pub pci_slot: String,
    /// Role assigned by the user.
    pub role: GpuRole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GpuRole {
    /// Stays bound to the host driver. The host renders the desktop with it.
    Host,
    /// Goes to the VM through `vfio-pci`.
    Passthrough,
    /// User explicitly told Virtu not to touch this GPU. It must remain on
    /// its current driver and must not appear in any plan.
    Ignored,
}

impl std::fmt::Display for GpuRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            GpuRole::Host => "host",
            GpuRole::Passthrough => "passthrough",
            GpuRole::Ignored => "ignored",
        };
        write!(f, "{s}")
    }
}

/// Monitor workflow plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MonitorPlan {
    /// One physical monitor.
    OneMonitor { strategy: SingleMonitorStrategy },
    /// Two physical monitors. `host_connector` keeps the Linux desktop;
    /// `vm_connector` is owned by the passthrough GPU's outputs.
    TwoMonitors {
        host_connector: String,
        vm_connector: String,
    },
}

/// How a single physical monitor will be shared between host and guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SingleMonitorStrategy {
    /// Host keeps the monitor; the VM is viewed through Looking Glass.
    LookingGlassOnly,
    /// User switches monitor inputs manually (KVM switch or display input
    /// switch). Virtu does not automate this.
    SwitchInputs,
    /// Display-manager-aware hand-off. High risk; requires single-GPU mode.
    HookHandoff,
}

/// Looking Glass selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LookingGlassChoice {
    Disabled,
    Enabled {
        install_mode: LookingGlassInstallMode,
        target_resolution: Resolution,
    },
}

impl LookingGlassChoice {
    pub fn is_enabled(&self) -> bool {
        matches!(self, LookingGlassChoice::Enabled { .. })
    }
}

/// How Virtu should obtain the Looking Glass client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LookingGlassInstallMode {
    /// User installs the Looking Glass client themselves.
    Manual,
    /// Virtu downloads the stable source and compiles it locally after
    /// explicit user consent. The consent decision is captured at plan time,
    /// not at choice time.
    AutoBuild,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

impl Resolution {
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

/// VM resource selections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmResources {
    pub ram_mb: u64,
    pub vcpu_count: u32,
    pub disk: DiskChoice,
}

/// User's disk choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiskChoice {
    /// Reuse an existing disk image at the given path.
    Existing { path: PathBuf },
    /// Create a new disk image of the given size and format.
    Create {
        path: PathBuf,
        size_gb: u64,
        format: DiskFormat,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiskFormat {
    Qcow2,
    Raw,
}

impl DiskFormat {
    pub fn extension(self) -> &'static str {
        match self {
            DiskFormat::Qcow2 => "qcow2",
            DiskFormat::Raw => "raw",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetworkChoice {
    /// libvirt default NAT network (`virbr0`).
    Nat,
    /// Bridge to a named host interface.
    Bridge { interface: String },
    /// No network device.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioChoice {
    /// Pipe audio to the detected host audio backend.
    HostAudio,
    /// Use the Scream network audio path.
    Scream,
    /// No audio device for the VM.
    None,
}

/// Evdev keyboard/mouse passthrough selections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct InputChoice {
    pub keyboard_evdev: Option<PathBuf>,
    pub mouse_evdev: Option<PathBuf>,
    pub additional_evdev: Vec<PathBuf>,
}

impl InputChoice {
    /// All evdev paths the user wants to grab, in iteration order.
    pub fn all_evdev_paths(&self) -> Vec<&PathBuf> {
        let mut paths: Vec<&PathBuf> = Vec::new();
        if let Some(p) = &self.keyboard_evdev {
            paths.push(p);
        }
        if let Some(p) = &self.mouse_evdev {
            paths.push(p);
        }
        for p in &self.additional_evdev {
            paths.push(p);
        }
        paths
    }
}

impl PassthroughConfig {
    /// Suggested defaults derived from a detected system profile. Returns
    /// `None` when there are no GPUs detected, since no GPU passthrough plan
    /// is meaningful in that case.
    ///
    /// This function does not perform I/O. It does not consult the host
    /// filesystem. Callers may freely override any field afterwards.
    pub fn recommended_defaults(profile: &SystemProfile) -> Option<Self> {
        if profile.gpus.is_empty() {
            return None;
        }

        let (gpu_mode, gpu_roles) = recommend_gpu_assignment(&profile.gpus);
        let monitor_plan = recommend_monitor_plan(profile, &gpu_roles);
        let looking_glass = match monitor_plan {
            MonitorPlan::OneMonitor {
                strategy: SingleMonitorStrategy::LookingGlassOnly,
            } => LookingGlassChoice::Enabled {
                install_mode: LookingGlassInstallMode::Manual,
                target_resolution: recommend_resolution(profile),
            },
            _ => LookingGlassChoice::Disabled,
        };

        let resources = recommend_resources(profile);
        let audio = match profile.audio {
            AudioSystem::PipeWire | AudioSystem::PulseAudio => AudioChoice::HostAudio,
            _ => AudioChoice::None,
        };

        Some(PassthroughConfig {
            vm_name: default_vm_name(profile),
            guest_os: GuestOs::Windows11,
            gpu_mode,
            gpu_roles,
            monitor_plan,
            looking_glass,
            iso_path: None,
            resources,
            network: NetworkChoice::Nat,
            audio,
            input: InputChoice::default(),
        })
    }

    /// Returns the GPU mode implied by the current `gpu_roles` list, ignoring
    /// the explicit `gpu_mode` field. This is what validation uses to detect
    /// inconsistencies between the user's stated mode and the role list.
    pub fn derived_mode(&self, profile: &SystemProfile) -> Option<GpuPassthroughMode> {
        let active: Vec<&GpuRoleAssignment> = self
            .gpu_roles
            .iter()
            .filter(|role| role.role != GpuRole::Ignored)
            .collect();

        let host_count = active
            .iter()
            .filter(|role| role.role == GpuRole::Host)
            .count();
        let pass_count = active
            .iter()
            .filter(|role| role.role == GpuRole::Passthrough)
            .count();

        if pass_count == 0 {
            return None;
        }

        if pass_count >= 2 {
            return Some(GpuPassthroughMode::MultiGpu);
        }

        if host_count == 0 {
            return Some(GpuPassthroughMode::SingleGpu);
        }

        let host_gpu = self.host_gpu(profile)?;
        if host_gpu.gpu_type == GpuType::Integrated {
            Some(GpuPassthroughMode::IgpuHost)
        } else {
            Some(GpuPassthroughMode::DualGpu)
        }
    }

    /// Returns the assignment marked as the passthrough GPU, when there is
    /// exactly one. Multi-GPU mode returns `None` here on purpose; consumers
    /// should iterate `passthrough_gpus` instead.
    pub fn primary_passthrough_gpu<'a>(&self, profile: &'a SystemProfile) -> Option<&'a GpuInfo> {
        let mut iter = self
            .gpu_roles
            .iter()
            .filter(|role| role.role == GpuRole::Passthrough);
        let first = iter.next()?;
        if iter.next().is_some() {
            return None;
        }
        find_gpu(profile, &first.pci_slot)
    }

    /// All passthrough GPUs in user order.
    pub fn passthrough_gpus<'a>(&self, profile: &'a SystemProfile) -> Vec<&'a GpuInfo> {
        self.gpu_roles
            .iter()
            .filter(|role| role.role == GpuRole::Passthrough)
            .filter_map(|role| find_gpu(profile, &role.pci_slot))
            .collect()
    }

    /// Returns the host GPU when exactly one is selected.
    pub fn host_gpu<'a>(&self, profile: &'a SystemProfile) -> Option<&'a GpuInfo> {
        let mut iter = self
            .gpu_roles
            .iter()
            .filter(|role| role.role == GpuRole::Host);
        let first = iter.next()?;
        if iter.next().is_some() {
            return None;
        }
        find_gpu(profile, &first.pci_slot)
    }
}

fn recommend_gpu_assignment(gpus: &[GpuInfo]) -> (GpuPassthroughMode, Vec<GpuRoleAssignment>) {
    if gpus.len() == 1 {
        let only = &gpus[0];
        return (
            GpuPassthroughMode::SingleGpu,
            vec![GpuRoleAssignment {
                pci_slot: only.pci_slot.clone(),
                role: GpuRole::Passthrough,
            }],
        );
    }

    let igpu = gpus.iter().find(|g| g.gpu_type == GpuType::Integrated);
    let candidate_dgpu = gpus
        .iter()
        .filter(|g| g.gpu_type == GpuType::Discrete)
        .find(|g| g.iommu_isolated)
        .or_else(|| gpus.iter().find(|g| g.gpu_type == GpuType::Discrete));

    if let (Some(igpu), Some(dgpu)) = (igpu, candidate_dgpu) {
        let mut roles: Vec<GpuRoleAssignment> = Vec::with_capacity(gpus.len());
        for gpu in gpus {
            let role = if gpu.pci_slot == igpu.pci_slot {
                GpuRole::Host
            } else if gpu.pci_slot == dgpu.pci_slot {
                GpuRole::Passthrough
            } else {
                GpuRole::Ignored
            };
            roles.push(GpuRoleAssignment {
                pci_slot: gpu.pci_slot.clone(),
                role,
            });
        }
        return (GpuPassthroughMode::IgpuHost, roles);
    }

    // Two or more GPUs of the same kind. Pick the passthrough candidate by
    // priority:
    //   1. An IOMMU-isolated GPU that is NOT the boot VGA. The boot VGA is
    //      where the firmware drew the splash screen and where Linux first
    //      hands display to the user; keeping it on the host avoids losing
    //      the host's display when vfio-pci binds.
    //   2. Any IOMMU-isolated GPU. If only the boot VGA is isolated, use it
    //      and let the user override (they may want NVIDIA-on-host with
    //      AMD passing through, or vice versa).
    //   3. Any non-boot-VGA GPU, isolated or not. Validation will surface
    //      the missing isolation as an error so the user is told why.
    //   4. Fallback to gpus[1] when no other candidate fits, e.g. two
    //      indistinguishable cards.
    //
    // Host candidate is then "anything that isn't the chosen passthrough
    // card", preferring the boot VGA so the host display remains stable.
    let pass_candidate = gpus
        .iter()
        .find(|g| g.iommu_isolated && !g.is_boot_vga)
        .or_else(|| gpus.iter().find(|g| g.iommu_isolated))
        .or_else(|| gpus.iter().find(|g| !g.is_boot_vga))
        .unwrap_or(&gpus[1]);

    let host_candidate = gpus
        .iter()
        .find(|g| g.is_boot_vga && g.pci_slot != pass_candidate.pci_slot)
        .or_else(|| gpus.iter().find(|g| g.pci_slot != pass_candidate.pci_slot))
        .unwrap_or(&gpus[0]);

    let mut roles: Vec<GpuRoleAssignment> = Vec::with_capacity(gpus.len());
    for gpu in gpus {
        let role = if gpu.pci_slot == host_candidate.pci_slot {
            GpuRole::Host
        } else if gpu.pci_slot == pass_candidate.pci_slot {
            GpuRole::Passthrough
        } else {
            GpuRole::Ignored
        };
        roles.push(GpuRoleAssignment {
            pci_slot: gpu.pci_slot.clone(),
            role,
        });
    }

    (GpuPassthroughMode::DualGpu, roles)
}

fn recommend_monitor_plan(profile: &SystemProfile, roles: &[GpuRoleAssignment]) -> MonitorPlan {
    let connected: Vec<_> = profile.monitors.iter().filter(|m| m.connected).collect();
    let host_slot = roles
        .iter()
        .find(|role| role.role == GpuRole::Host)
        .map(|role| role.pci_slot.clone());
    let pass_slot = roles
        .iter()
        .find(|role| role.role == GpuRole::Passthrough)
        .map(|role| role.pci_slot.clone());

    if connected.len() >= 2 {
        let host_monitor: Option<&crate::detect::MonitorInfo> = host_slot
            .as_ref()
            .and_then(|slot| {
                connected
                    .iter()
                    .copied()
                    .find(|m| m.gpu_pci_slot.as_deref() == Some(slot.as_str()))
            })
            .or_else(|| connected.first().copied());

        let vm_monitor: Option<&crate::detect::MonitorInfo> = pass_slot
            .as_ref()
            .and_then(|slot| {
                connected
                    .iter()
                    .copied()
                    .find(|m| m.gpu_pci_slot.as_deref() == Some(slot.as_str()))
            })
            .or_else(|| {
                connected.iter().copied().find(|m| {
                    host_monitor
                        .map(|host| host.connector_name != m.connector_name)
                        .unwrap_or(true)
                })
            });

        if let (Some(host_monitor), Some(vm_monitor)) = (host_monitor, vm_monitor) {
            if host_monitor.connector_name != vm_monitor.connector_name {
                return MonitorPlan::TwoMonitors {
                    host_connector: host_monitor.connector_name.clone(),
                    vm_connector: vm_monitor.connector_name.clone(),
                };
            }
        }
    }

    MonitorPlan::OneMonitor {
        strategy: SingleMonitorStrategy::LookingGlassOnly,
    }
}

fn recommend_resolution(profile: &SystemProfile) -> Resolution {
    let connected = profile.monitors.iter().find(|m| m.connected);
    if let Some(monitor) = connected {
        if let Some(mode) = monitor.current_mode.as_deref() {
            if let Some((w, h)) = parse_mode_string(mode) {
                return Resolution::new(w, h);
            }
        }
    }
    Resolution::new(1920, 1080)
}

fn parse_mode_string(mode: &str) -> Option<(u32, u32)> {
    let (w, h) = mode.trim().split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

fn recommend_resources(profile: &SystemProfile) -> VmResources {
    let ram_mb = profile.ram.recommended_vm_ram_mb();
    let vcpu_count = recommend_vcpu_count(profile);
    let default_dir = profile.storage.default_vm_dir.clone();
    let path = default_dir.join("virtu-windows.qcow2");

    VmResources {
        ram_mb,
        vcpu_count,
        disk: DiskChoice::Create {
            path,
            size_gb: 100,
            format: DiskFormat::Qcow2,
        },
    }
}

fn recommend_vcpu_count(profile: &SystemProfile) -> u32 {
    let host_threads = profile.cpu.logical_cores.max(1);
    // Always reserve at least one thread for the host so validation
    // (`vcpu_count < host_threads`) cannot reject the recommended default.
    let max_for_vm = host_threads.saturating_sub(1).max(1);

    let physical = profile.cpu.physical_cores.max(1);
    let threads_per_core = if profile.cpu.has_hyperthreading { 2 } else { 1 };
    let suggested_physical = (physical * 3 / 4).max(1);
    let target = suggested_physical * threads_per_core;

    target.min(max_for_vm).max(1)
}

fn find_gpu<'a>(profile: &'a SystemProfile, slot: &str) -> Option<&'a GpuInfo> {
    profile.gpus.iter().find(|gpu| gpu.pci_slot == slot)
}

/// Default VM name. Tries `virtu-windows`, then `virtu-windows-2`, `-3`, ...
/// until it finds one not already registered with libvirt. The user can
/// override this through the upcoming wizard / CLI flags.
pub(crate) fn default_vm_name(profile: &SystemProfile) -> String {
    let base = "virtu-windows";
    let existing: std::collections::HashSet<&str> = profile
        .readiness
        .libvirt_domains
        .iter()
        .map(|d| d.name.as_str())
        .collect();
    if !existing.contains(base) {
        return base.to_string();
    }
    for n in 2..1000 {
        let candidate = format!("{base}-{n}");
        if !existing.contains(candidate.as_str()) {
            return candidate;
        }
    }
    // Pathological host with 1000+ virtu-windows-N domains: fall back to a
    // timestamp-suffixed name. Validation will still re-check uniqueness.
    format!("{base}-{}", chrono::Utc::now().format("%Y%m%d%H%M%S"))
}

/// Libvirt accepts only a narrow character set in domain names: letters,
/// digits, `_`, `.`, `+`, and `-`. The name must not be empty.
pub(crate) fn is_valid_vm_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '+' | '-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::gpu::{GpuType, GpuVendor};

    fn dgpu(pci_slot: &str, vendor: GpuVendor, isolated: bool, boot_vga: bool) -> GpuInfo {
        GpuInfo {
            pci_slot: pci_slot.to_string(),
            vendor,
            gpu_type: GpuType::Discrete,
            model_name: "test gpu".to_string(),
            vendor_id: "0000".to_string(),
            device_id: "0000".to_string(),
            subsystem_vendor_id: "0000".to_string(),
            subsystem_device_id: "0000".to_string(),
            current_driver: None,
            iommu_group_id: None,
            iommu_isolated: isolated,
            rom_accessible: true,
            companion_audio: None,
            is_boot_vga: boot_vga,
            vfio_compatible: isolated,
            quirks: Vec::new(),
        }
    }

    /// Regression: a host with two dGPUs where the boot VGA is the only
    /// isolated card (e.g. AMD on CPU PCIe + NVIDIA on chipset PCIe) must
    /// pick the isolated card for passthrough, not the non-isolated one.
    /// The previous logic preferred "not the boot VGA" first and produced a
    /// non-isolated passthrough recommendation, which the validator then
    /// correctly refused.
    #[test]
    fn dual_dgpu_picks_isolated_card_for_passthrough_even_when_boot_vga() {
        let amd_isolated_boot_vga = dgpu("0000:2d:00.0", GpuVendor::Amd, true, true);
        let nvidia_chipset_routed = dgpu("0000:04:00.0", GpuVendor::Nvidia, false, false);
        let gpus = vec![amd_isolated_boot_vga.clone(), nvidia_chipset_routed.clone()];

        let (mode, roles) = recommend_gpu_assignment(&gpus);
        assert_eq!(mode, GpuPassthroughMode::DualGpu);

        let pass_role = roles
            .iter()
            .find(|r| r.role == GpuRole::Passthrough)
            .expect("a passthrough role must be assigned");
        assert_eq!(
            pass_role.pci_slot, amd_isolated_boot_vga.pci_slot,
            "the isolated GPU must be picked for passthrough"
        );

        let host_role = roles
            .iter()
            .find(|r| r.role == GpuRole::Host)
            .expect("a host role must be assigned");
        assert_eq!(
            host_role.pci_slot, nvidia_chipset_routed.pci_slot,
            "the non-isolated GPU must be host"
        );
    }

    /// Standard dual-dGPU layout where the isolated card is NOT the boot
    /// VGA still picks the isolated card for passthrough.
    #[test]
    fn dual_dgpu_prefers_isolated_non_boot_vga_when_available() {
        let host_card = dgpu("0000:01:00.0", GpuVendor::Nvidia, false, true);
        let pass_card = dgpu("0000:0a:00.0", GpuVendor::Amd, true, false);
        let gpus = vec![host_card.clone(), pass_card.clone()];

        let (_, roles) = recommend_gpu_assignment(&gpus);
        let pass_role = roles
            .iter()
            .find(|r| r.role == GpuRole::Passthrough)
            .expect("a passthrough role must be assigned");
        assert_eq!(pass_role.pci_slot, pass_card.pci_slot);
    }

    /// Two non-isolated dGPUs is a degenerate case: the recommendation
    /// still produces a plan (the user may want to enable ACS override or
    /// just see the plan) but it falls back to "non-boot-VGA as
    /// passthrough". Validation will then fail loudly because neither card
    /// is isolated, which is the correct outcome.
    #[test]
    fn dual_dgpu_with_no_isolation_falls_back_to_non_boot_vga() {
        let boot_vga_card = dgpu("0000:01:00.0", GpuVendor::Nvidia, false, true);
        let other_card = dgpu("0000:0a:00.0", GpuVendor::Amd, false, false);
        let gpus = vec![boot_vga_card.clone(), other_card.clone()];

        let (_, roles) = recommend_gpu_assignment(&gpus);
        let pass_role = roles
            .iter()
            .find(|r| r.role == GpuRole::Passthrough)
            .expect("a passthrough role must be assigned");
        assert_eq!(pass_role.pci_slot, other_card.pci_slot);
    }
}
