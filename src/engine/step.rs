use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepKind {
    Detect,
    Snapshot,
    BootloaderWrite,
    InitramfsWrite,
    VfioConfig,
    HookInstall,
    VmXmlGenerate,
    VmRegister,
    Verify,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepRisk {
    ReadOnly,
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedStep {
    pub kind: StepKind,
    pub title: String,
    pub summary: String,
    pub risk: StepRisk,
    pub touches: Vec<String>,
    pub requires_reboot: bool,
}
