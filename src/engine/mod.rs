pub mod compatibility;
pub mod diagnostics;
pub mod executor;
pub mod planner;
pub mod resume;
pub mod step;

pub use compatibility::{
    build_compatibility_report, CompatibilityFinding, CompatibilityReport, CompatibilityStatus,
    FindingSeverity, FixAutomation, FixOption,
};
pub use executor::{
    execute_phase_a, execute_plan, execute_snapshot_step, PhaseAError, PhaseAOutcome,
};
pub use planner::{build_plan, plan, Plan, PlanError, PlanSummary};
pub use resume::{verify_phase_a_landed, Divergence, HostMismatch, ResumeReadiness};
pub use step::{PlannedStep, PrivilegeNeed, StepKind, StepRisk, StepState};
