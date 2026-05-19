pub mod compatibility;
pub mod diagnostics;
pub mod executor;
pub mod planner;
pub mod resume;
pub mod step;
pub mod vm_xml;

pub use compatibility::{
    build_compatibility_report, CompatibilityFinding, CompatibilityReport, CompatibilityStatus,
    FindingSeverity, FixAutomation, FixOption,
};
pub use executor::{
    execute_phase_a, execute_phase_b, execute_plan, execute_snapshot_step, HostCommandMode,
    PhaseAError, PhaseAOutcome, PhaseBError, PhaseBOutcome, RegenerateMode,
};
pub use planner::{build_plan, plan, Plan, PlanError, PlanSummary};
pub use resume::{
    verify_hook_install, verify_phase_a_landed, Divergence, HostMismatch, ResumeReadiness,
};
pub use step::{PlannedStep, PrivilegeNeed, StepKind, StepRisk, StepState};
pub use vm_xml::generate_vm_xml;
