pub mod compatibility;
pub mod diagnostics;
pub mod executor;
pub mod planner;
pub mod step;

pub use compatibility::{
    build_compatibility_report, CompatibilityFinding, CompatibilityReport, CompatibilityStatus,
    FindingSeverity, FixAutomation, FixOption,
};
pub use executor::{execute_plan, execute_snapshot_step};
pub use planner::{build_plan, plan, Plan, PlanError, PlanSummary};
pub use step::{PlannedStep, PrivilegeNeed, StepKind, StepRisk, StepState};
