pub mod cpu_topology;
pub mod passthrough;
pub mod profile;
pub mod validation;
pub mod xml;

pub use passthrough::{
    AudioChoice, DiskChoice, DiskFormat, GpuPassthroughMode, GpuRole, GpuRoleAssignment, GuestOs,
    InputChoice, LookingGlassChoice, LookingGlassInstallMode, MonitorPlan, NetworkChoice,
    PassthroughConfig, Resolution, SingleMonitorStrategy, VmResources,
};
pub use profile::{vm_view, VmDiskView, VmInputView, VmView, VmViewError};
pub use validation::{
    validate, ValidationIssue, ValidationIssueId, ValidationReport, ValidationSeverity,
};
