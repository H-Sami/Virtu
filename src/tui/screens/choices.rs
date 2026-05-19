//! Choice screen (Milestone 10, slice 10.3).
//!
//! Walks the user through the major `PassthroughConfig` decisions that
//! drive the planner: GPU mode, role assignments, monitor plan, RAM,
//! and vCPU count. Each field is one row; `j`/`k` (or arrow keys)
//! navigate between fields; `Left`/`Right` cycle option values for
//! enum fields and step numeric fields.
//!
//! The screen owns its own `ChoiceState` and exposes a pure
//! `apply_to(&mut PassthroughConfig)` so the choices can flow back
//! into the wider `App` without the screen knowing anything about
//! the larger wizard state machine.
//!
//! Text-input fields (vm_name, ISO path, custom disk path) are not
//! covered by this slice; ratatui's text-input widgets need a more
//! involved input handler that's worth a follow-up slice. For now
//! those fields stay at their `recommended_defaults` values and the
//! plan-preview screen surfaces them so the user can confirm.

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use crate::detect::SystemProfile;
use crate::vm::{
    GpuPassthroughMode, GpuRole, MonitorPlan, PassthroughConfig, SingleMonitorStrategy,
};

/// One step the user can take inside the choice screen. Mapped from
/// `tui::WizardEvent` by the dispatcher in `tui::mod`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoiceAction {
    PrevField,
    NextField,
    DecrementValue,
    IncrementValue,
}

/// Mutable state for the choice screen. Built from a
/// `PassthroughConfig` (typically `recommended_defaults` plus the
/// detected `SystemProfile`) and edited in-place via `apply`. When the
/// user advances to the plan preview the wizard calls `to_config` to
/// re-materialise the edited config.
#[derive(Debug, Clone)]
pub struct ChoiceState {
    /// Index of the highlighted field (0..fields.len()).
    pub selected: usize,
    /// All fields displayed on screen, in render order.
    pub fields: Vec<ChoiceField>,
    /// Cached helper text. Refreshed when the selected field changes.
    pub helper_text: String,
}

/// One row of the choice screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChoiceField {
    GpuMode {
        current: GpuPassthroughMode,
        available: Vec<GpuPassthroughMode>,
    },
    MonitorPlan {
        current: MonitorPlanChoice,
        available: Vec<MonitorPlanChoice>,
    },
    /// VM RAM in MiB. Step size is 1 GiB (1024 MiB).
    RamMb { current: u64, host_total_kb: u64 },
    /// VM vCPU count. Step size is 1.
    VcpuCount { current: u32, host_threads: u32 },
}

/// Compact monitor-plan variant used by the choice screen. The full
/// `MonitorPlan` enum carries connector strings that we currently do
/// not let the user edit in the TUI; this enum lists the high-level
/// shapes so cycling is a clean enum step. Mapping back to the full
/// plan happens in `apply_choices`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorPlanChoice {
    OneMonitorHookHandoff,
    OneMonitorSwitchInputs,
    TwoMonitorsHostAndVm,
}

impl std::fmt::Display for MonitorPlanChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            MonitorPlanChoice::OneMonitorHookHandoff => "One monitor, hook hand-off (single GPU)",
            MonitorPlanChoice::OneMonitorSwitchInputs => "One monitor, switch inputs manually",
            MonitorPlanChoice::TwoMonitorsHostAndVm => "Two monitors, one each",
        };
        write!(f, "{s}")
    }
}

impl ChoiceField {
    fn label(&self) -> &'static str {
        match self {
            ChoiceField::GpuMode { .. } => "GPU mode",
            ChoiceField::MonitorPlan { .. } => "Monitor plan",
            ChoiceField::RamMb { .. } => "VM RAM (MiB)",
            ChoiceField::VcpuCount { .. } => "VM vCPUs",
        }
    }

    fn current_value_text(&self) -> String {
        match self {
            ChoiceField::GpuMode { current, .. } => current.to_string(),
            ChoiceField::MonitorPlan { current, .. } => current.to_string(),
            ChoiceField::RamMb { current, .. } => {
                format!("{current} MiB ({:.1} GiB)", *current as f64 / 1024.0)
            }
            ChoiceField::VcpuCount { current, .. } => current.to_string(),
        }
    }

    fn helper_for(&self) -> &'static str {
        match self {
            ChoiceField::GpuMode { .. } => {
                "Left / Right cycle through GPU modes. SingleGpu carries higher risk because the host display is torn down when the VM starts."
            }
            ChoiceField::MonitorPlan { .. } => {
                "Left / Right cycle through monitor plans. Hook hand-off requires SingleGpu mode; the validator surfaces the mismatch on the plan preview if the choices are inconsistent."
            }
            ChoiceField::RamMb { .. } => {
                "Left / Right step in 1 GiB increments. Virtu reserves at least 4 GiB for the host."
            }
            ChoiceField::VcpuCount { .. } => {
                "Left / Right add or remove one vCPU. Virtu always reserves at least one host thread."
            }
        }
    }

    fn decrement(&mut self) {
        match self {
            ChoiceField::GpuMode { current, available } => {
                let idx = available.iter().position(|m| m == current).unwrap_or(0);
                let next = if idx == 0 {
                    available.len() - 1
                } else {
                    idx - 1
                };
                if let Some(value) = available.get(next).copied() {
                    *current = value;
                }
            }
            ChoiceField::MonitorPlan { current, available } => {
                let idx = available.iter().position(|m| m == current).unwrap_or(0);
                let next = if idx == 0 {
                    available.len() - 1
                } else {
                    idx - 1
                };
                if let Some(value) = available.get(next).copied() {
                    *current = value;
                }
            }
            ChoiceField::RamMb {
                current,
                host_total_kb,
            } => {
                let host_mb = *host_total_kb / 1024;
                let host_reserve_mb: u64 = 4 * 1024;
                let min_vm_ram_mb: u64 = 2 * 1024;
                let new_value = current.saturating_sub(1024);
                // Floor at min_vm_ram_mb but never above (host_total -
                // host_reserve). The recommended_defaults builder already
                // gave us a value below the host cap, so a decrement
                // never violates it; the floor is what we care about.
                let max_allowed = host_mb.saturating_sub(host_reserve_mb);
                *current = new_value.clamp(min_vm_ram_mb, max_allowed.max(min_vm_ram_mb));
            }
            ChoiceField::VcpuCount {
                current,
                host_threads,
            } => {
                let new_value = current.saturating_sub(1).max(1);
                let max_allowed = host_threads.saturating_sub(1).max(1);
                *current = new_value.clamp(1, max_allowed);
            }
        }
    }

    fn increment(&mut self) {
        match self {
            ChoiceField::GpuMode { current, available } => {
                let idx = available.iter().position(|m| m == current).unwrap_or(0);
                let next = (idx + 1) % available.len();
                if let Some(value) = available.get(next).copied() {
                    *current = value;
                }
            }
            ChoiceField::MonitorPlan { current, available } => {
                let idx = available.iter().position(|m| m == current).unwrap_or(0);
                let next = (idx + 1) % available.len();
                if let Some(value) = available.get(next).copied() {
                    *current = value;
                }
            }
            ChoiceField::RamMb {
                current,
                host_total_kb,
            } => {
                let host_mb = *host_total_kb / 1024;
                let host_reserve_mb: u64 = 4 * 1024;
                let min_vm_ram_mb: u64 = 2 * 1024;
                let max_allowed = host_mb.saturating_sub(host_reserve_mb);
                let new_value = current.saturating_add(1024);
                *current = new_value.clamp(min_vm_ram_mb, max_allowed.max(min_vm_ram_mb));
            }
            ChoiceField::VcpuCount {
                current,
                host_threads,
            } => {
                let max_allowed = host_threads.saturating_sub(1).max(1);
                *current = current.saturating_add(1).clamp(1, max_allowed);
            }
        }
    }
}

impl ChoiceState {
    /// Build the initial state from the user's `recommended_defaults`
    /// config plus the detected host. The available enum values for
    /// each field are computed from the host shape; for example,
    /// `IgpuHost` is only listed when the host actually has an iGPU.
    pub fn new(profile: &SystemProfile, config: &PassthroughConfig) -> Self {
        let mut available_modes = vec![GpuPassthroughMode::SingleGpu];
        let has_igpu = profile
            .gpus
            .iter()
            .any(|gpu| gpu.gpu_type == crate::detect::gpu::GpuType::Integrated);
        let has_multiple = profile.gpus.len() >= 2;
        if has_igpu && has_multiple {
            available_modes.insert(0, GpuPassthroughMode::IgpuHost);
        }
        if has_multiple {
            // DualGpu is the generic discrete + discrete fallback. Slot
            // it before SingleGpu so the cycle order matches the
            // recommended-defaults ordering (most-preferred first).
            let pos = if has_igpu { 1 } else { 0 };
            available_modes.insert(pos, GpuPassthroughMode::DualGpu);
        }

        let monitor_choices = vec![
            MonitorPlanChoice::TwoMonitorsHostAndVm,
            MonitorPlanChoice::OneMonitorHookHandoff,
            MonitorPlanChoice::OneMonitorSwitchInputs,
        ];
        let initial_monitor = monitor_choice_from(&config.monitor_plan);

        let fields = vec![
            ChoiceField::GpuMode {
                current: config.gpu_mode,
                available: available_modes,
            },
            ChoiceField::MonitorPlan {
                current: initial_monitor,
                available: monitor_choices,
            },
            ChoiceField::RamMb {
                current: config.resources.ram_mb,
                host_total_kb: profile.ram.total_kb,
            },
            ChoiceField::VcpuCount {
                current: config.resources.vcpu_count,
                host_threads: profile.cpu.logical_cores,
            },
        ];

        let helper_text = fields[0].helper_for().to_string();

        Self {
            selected: 0,
            fields,
            helper_text,
        }
    }

    pub fn apply(&mut self, action: ChoiceAction) {
        match action {
            ChoiceAction::PrevField => {
                if self.selected == 0 {
                    self.selected = self.fields.len().saturating_sub(1);
                } else {
                    self.selected -= 1;
                }
            }
            ChoiceAction::NextField => {
                if !self.fields.is_empty() {
                    self.selected = (self.selected + 1) % self.fields.len();
                }
            }
            ChoiceAction::DecrementValue => {
                if let Some(field) = self.fields.get_mut(self.selected) {
                    field.decrement();
                }
            }
            ChoiceAction::IncrementValue => {
                if let Some(field) = self.fields.get_mut(self.selected) {
                    field.increment();
                }
            }
        }
        if let Some(field) = self.fields.get(self.selected) {
            self.helper_text = field.helper_for().to_string();
        }
    }

    /// Project the in-progress field values back into the supplied
    /// `PassthroughConfig`. Pure: nothing about the host is touched.
    /// The caller (the wizard `App`) holds a mutable config and
    /// re-validates after this returns.
    pub fn apply_to(&self, profile: &SystemProfile, config: &mut PassthroughConfig) {
        for field in &self.fields {
            match field {
                ChoiceField::GpuMode { current, .. } => {
                    config.gpu_mode = *current;
                    align_gpu_roles_to_mode(profile, config, *current);
                }
                ChoiceField::MonitorPlan { current, .. } => {
                    config.monitor_plan = monitor_plan_from_choice(profile, *current);
                }
                ChoiceField::RamMb { current, .. } => {
                    config.resources.ram_mb = *current;
                }
                ChoiceField::VcpuCount { current, .. } => {
                    config.resources.vcpu_count = *current;
                }
            }
        }
    }
}

fn monitor_choice_from(plan: &MonitorPlan) -> MonitorPlanChoice {
    match plan {
        MonitorPlan::OneMonitor {
            strategy: SingleMonitorStrategy::HookHandoff,
        } => MonitorPlanChoice::OneMonitorHookHandoff,
        MonitorPlan::OneMonitor {
            strategy: SingleMonitorStrategy::SwitchInputs,
        } => MonitorPlanChoice::OneMonitorSwitchInputs,
        MonitorPlan::OneMonitor {
            strategy: SingleMonitorStrategy::LookingGlassOnly,
        } => MonitorPlanChoice::OneMonitorSwitchInputs,
        MonitorPlan::TwoMonitors { .. } => MonitorPlanChoice::TwoMonitorsHostAndVm,
    }
}

fn monitor_plan_from_choice(profile: &SystemProfile, choice: MonitorPlanChoice) -> MonitorPlan {
    match choice {
        MonitorPlanChoice::OneMonitorHookHandoff => MonitorPlan::OneMonitor {
            strategy: SingleMonitorStrategy::HookHandoff,
        },
        MonitorPlanChoice::OneMonitorSwitchInputs => MonitorPlan::OneMonitor {
            strategy: SingleMonitorStrategy::SwitchInputs,
        },
        MonitorPlanChoice::TwoMonitorsHostAndVm => {
            // Pick the first two connected DRM connectors as defaults.
            // The user can refine these later via the CLI flag work
            // queued for a follow-up slice; today the wizard does not
            // edit connector strings directly.
            let mut connectors = profile
                .monitors
                .iter()
                .filter(|m| m.connected)
                .map(|m| m.connector_name.clone());
            let host_connector = connectors.next().unwrap_or_else(|| "DP-1".to_string());
            let vm_connector = connectors.next().unwrap_or_else(|| "DP-2".to_string());
            MonitorPlan::TwoMonitors {
                host_connector,
                vm_connector,
            }
        }
    }
}

/// Mutate `config.gpu_roles` so the role assignments line up with
/// the freshly chosen mode. This is what `validate` would otherwise
/// catch: e.g. if the user picks SingleGpu but had two `Host`
/// assignments left over from `recommended_defaults`, the validator
/// surfaces a `GpuModeMismatch` error. Doing the alignment here means
/// the plan-preview screen sees a consistent config.
fn align_gpu_roles_to_mode(
    profile: &SystemProfile,
    config: &mut PassthroughConfig,
    mode: GpuPassthroughMode,
) {
    use crate::detect::gpu::GpuType;
    match mode {
        GpuPassthroughMode::SingleGpu => {
            // Pick the first discrete GPU as passthrough; ignore the
            // rest. Falls back to whatever GPU the user originally had
            // as Passthrough.
            let primary = config
                .gpu_roles
                .iter()
                .find(|r| r.role == GpuRole::Passthrough)
                .map(|r| r.pci_slot.clone())
                .or_else(|| {
                    profile
                        .gpus
                        .iter()
                        .find(|g| g.gpu_type == GpuType::Discrete)
                        .map(|g| g.pci_slot.clone())
                });
            for role in &mut config.gpu_roles {
                if Some(&role.pci_slot) == primary.as_ref() {
                    role.role = GpuRole::Passthrough;
                } else {
                    role.role = GpuRole::Ignored;
                }
            }
        }
        GpuPassthroughMode::IgpuHost => {
            // iGPU = host, first discrete = passthrough, rest ignored.
            for role in &mut config.gpu_roles {
                let gpu = profile.gpus.iter().find(|g| g.pci_slot == role.pci_slot);
                role.role = match gpu.map(|g| g.gpu_type.clone()) {
                    Some(GpuType::Integrated) => GpuRole::Host,
                    Some(GpuType::Discrete) => GpuRole::Passthrough,
                    _ => GpuRole::Ignored,
                };
            }
            // If multiple discretes are present, ensure only the first
            // one is Passthrough.
            let mut first_pass_seen = false;
            for role in &mut config.gpu_roles {
                if role.role == GpuRole::Passthrough {
                    if first_pass_seen {
                        role.role = GpuRole::Ignored;
                    } else {
                        first_pass_seen = true;
                    }
                }
            }
        }
        GpuPassthroughMode::DualGpu => {
            // Two discretes: first = host, second = passthrough.
            let mut first_host = false;
            let mut first_pass = false;
            for role in &mut config.gpu_roles {
                let gpu = profile.gpus.iter().find(|g| g.pci_slot == role.pci_slot);
                if gpu.map(|g| g.gpu_type.clone()) != Some(GpuType::Discrete) {
                    role.role = GpuRole::Ignored;
                    continue;
                }
                if !first_host {
                    role.role = GpuRole::Host;
                    first_host = true;
                } else if !first_pass {
                    role.role = GpuRole::Passthrough;
                    first_pass = true;
                } else {
                    role.role = GpuRole::Ignored;
                }
            }
        }
        GpuPassthroughMode::MultiGpu => {
            // The validator already blocks this for v1.0; preserve the
            // user's existing roles so the validator can surface the
            // refusal cleanly.
        }
    }
}

/// Render the choice screen.
pub fn render(frame: &mut Frame, area: Rect, state: &ChoiceState) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(5),
            Constraint::Length(3),
        ])
        .split(area);

    render_header(frame, outer[0]);
    render_fields(frame, outer[1], state);
    render_helper(frame, outer[2], state);
    render_footer(frame, outer[3]);
}

fn render_header(frame: &mut Frame, area: Rect) {
    let title = Line::from(vec![Span::styled(
        "Configure your VM",
        Style::default().add_modifier(Modifier::BOLD),
    )]);
    let header = Paragraph::new(title)
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(header, area);
}

fn render_fields(frame: &mut Frame, area: Rect, state: &ChoiceState) {
    let mut lines: Vec<Line> = Vec::with_capacity(state.fields.len());
    for (idx, field) in state.fields.iter().enumerate() {
        let cursor = if idx == state.selected { "▶ " } else { "  " };
        let label = field.label();
        let value = field.current_value_text();
        let style = if idx == state.selected {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::from(vec![
            Span::styled(cursor, style),
            Span::styled(format!("{label:<14}"), style),
            Span::raw("  "),
            Span::styled(value, style),
        ]));
    }
    let block = Block::default().borders(Borders::ALL).title(" Choices ");
    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_helper(frame: &mut Frame, area: Rect, state: &ChoiceState) {
    let block = Block::default().borders(Borders::ALL).title(" Help ");
    let paragraph = Paragraph::new(state.helper_text.clone())
        .block(block)
        .wrap(Wrap { trim: true });
    frame.render_widget(paragraph, area);
}

fn render_footer(frame: &mut Frame, area: Rect) {
    let hint = Line::from(vec![
        Span::styled(
            "↑/k ↓/j",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" move    "),
        Span::styled(
            "←/h →/l",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" change    "),
        Span::styled(
            "Enter",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" preview plan    "),
        Span::styled(
            "q",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" quit"),
    ]);
    let footer = Paragraph::new(hint)
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::gpu::{GpuType, GpuVendor};
    use crate::vm::{DiskChoice, DiskFormat, GpuRoleAssignment, VmResources};

    fn dual_gpu_profile() -> SystemProfile {
        crate::tui::screens::detection::tests_helpers::dummy_profile_with_extras(vec![
            (
                "0000:00:02.0",
                GpuType::Integrated,
                GpuVendor::Intel,
                "8086",
                "0a16",
            ),
            (
                "0000:01:00.0",
                GpuType::Discrete,
                GpuVendor::Nvidia,
                "10de",
                "1f08",
            ),
        ])
    }

    fn config_for(profile: &SystemProfile) -> PassthroughConfig {
        // Build a representative recommended-style config inline so the
        // test does not need to reach into the real recommended_defaults
        // implementation (which has its own coverage in
        // `tests/passthrough_validation.rs`).
        PassthroughConfig {
            vm_name: "virtu-test".to_string(),
            guest_os: crate::vm::GuestOs::Windows11,
            gpu_mode: GpuPassthroughMode::IgpuHost,
            gpu_roles: profile
                .gpus
                .iter()
                .map(|g| GpuRoleAssignment {
                    pci_slot: g.pci_slot.clone(),
                    role: match g.gpu_type.clone() {
                        GpuType::Integrated => GpuRole::Host,
                        GpuType::Discrete => GpuRole::Passthrough,
                        _ => GpuRole::Ignored,
                    },
                })
                .collect(),
            monitor_plan: MonitorPlan::TwoMonitors {
                host_connector: "DP-1".to_string(),
                vm_connector: "DP-2".to_string(),
            },
            looking_glass: crate::vm::LookingGlassChoice::Disabled,
            iso_path: None,
            resources: VmResources {
                ram_mb: 16 * 1024,
                vcpu_count: 8,
                disk: DiskChoice::Create {
                    path: std::path::PathBuf::from("/var/lib/libvirt/images/virtu-test.qcow2"),
                    size_gb: 100,
                    format: DiskFormat::Qcow2,
                },
            },
            network: crate::vm::NetworkChoice::Nat,
            audio: crate::vm::AudioChoice::HostAudio,
            input: crate::vm::InputChoice::default(),
        }
    }

    #[test]
    fn new_offers_igpu_host_when_profile_has_an_igpu_and_a_dgpu() {
        let profile = dual_gpu_profile();
        let config = config_for(&profile);
        let state = ChoiceState::new(&profile, &config);
        // The first field is GPU mode; available list should include
        // IgpuHost (because there's an iGPU + a discrete GPU).
        match &state.fields[0] {
            ChoiceField::GpuMode { available, .. } => {
                assert!(available.contains(&GpuPassthroughMode::IgpuHost));
                assert!(available.contains(&GpuPassthroughMode::DualGpu));
                assert!(available.contains(&GpuPassthroughMode::SingleGpu));
                // MultiGpu is intentionally excluded because the v1.0
                // validator blocks it.
                assert!(!available.contains(&GpuPassthroughMode::MultiGpu));
            }
            other => panic!("first field must be GpuMode, got {other:?}"),
        }
    }

    #[test]
    fn new_strips_igpu_host_when_no_igpu_is_present() {
        let profile =
            crate::tui::screens::detection::tests_helpers::dummy_profile_with_extras(vec![(
                "0000:01:00.0",
                GpuType::Discrete,
                GpuVendor::Nvidia,
                "10de",
                "1f08",
            )]);
        let mut config = config_for(&profile);
        config.gpu_mode = GpuPassthroughMode::SingleGpu;
        let state = ChoiceState::new(&profile, &config);
        match &state.fields[0] {
            ChoiceField::GpuMode { available, .. } => {
                assert!(!available.contains(&GpuPassthroughMode::IgpuHost));
                assert!(available.contains(&GpuPassthroughMode::SingleGpu));
            }
            other => panic!("first field must be GpuMode, got {other:?}"),
        }
    }

    #[test]
    fn next_field_wraps_around_to_the_first_field() {
        let profile = dual_gpu_profile();
        let mut state = ChoiceState::new(&profile, &config_for(&profile));
        let total = state.fields.len();
        for _ in 0..total {
            state.apply(ChoiceAction::NextField);
        }
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn prev_field_wraps_around_to_the_last_field() {
        let profile = dual_gpu_profile();
        let mut state = ChoiceState::new(&profile, &config_for(&profile));
        state.apply(ChoiceAction::PrevField);
        assert_eq!(state.selected, state.fields.len() - 1);
    }

    #[test]
    fn increment_on_ram_field_steps_in_one_gib_increments_up_to_host_minus_reserve() {
        let profile = dual_gpu_profile();
        let mut state = ChoiceState::new(&profile, &config_for(&profile));
        // RAM field is index 2 in the layout above.
        state.selected = 2;
        // The dummy profile has 32 GiB total, so max VM RAM is
        // 32 - 4 = 28 GiB = 28672 MiB.
        let initial = match state.fields[2] {
            ChoiceField::RamMb { current, .. } => current,
            _ => panic!(),
        };
        for _ in 0..40 {
            state.apply(ChoiceAction::IncrementValue);
        }
        let final_value = match state.fields[2] {
            ChoiceField::RamMb { current, .. } => current,
            _ => panic!(),
        };
        assert!(final_value <= 28 * 1024);
        assert!(final_value >= initial);
    }

    #[test]
    fn decrement_on_ram_field_floors_at_2_gib() {
        let profile = dual_gpu_profile();
        let mut state = ChoiceState::new(&profile, &config_for(&profile));
        state.selected = 2;
        for _ in 0..100 {
            state.apply(ChoiceAction::DecrementValue);
        }
        let final_value = match state.fields[2] {
            ChoiceField::RamMb { current, .. } => current,
            _ => panic!(),
        };
        assert_eq!(final_value, 2 * 1024);
    }

    #[test]
    fn decrement_on_vcpu_field_floors_at_one() {
        let profile = dual_gpu_profile();
        let mut state = ChoiceState::new(&profile, &config_for(&profile));
        state.selected = 3;
        for _ in 0..40 {
            state.apply(ChoiceAction::DecrementValue);
        }
        let final_value = match state.fields[3] {
            ChoiceField::VcpuCount { current, .. } => current,
            _ => panic!(),
        };
        assert_eq!(final_value, 1);
    }

    #[test]
    fn increment_on_vcpu_field_caps_at_host_threads_minus_one() {
        let profile = dual_gpu_profile();
        let mut state = ChoiceState::new(&profile, &config_for(&profile));
        state.selected = 3;
        for _ in 0..40 {
            state.apply(ChoiceAction::IncrementValue);
        }
        let final_value = match state.fields[3] {
            ChoiceField::VcpuCount { current, .. } => current,
            _ => panic!(),
        };
        // The dummy profile has 16 logical cores; cap is 15.
        assert_eq!(final_value, 15);
    }

    #[test]
    fn cycling_gpu_mode_to_single_gpu_aligns_role_assignments_to_one_passthrough_only() {
        let profile = dual_gpu_profile();
        let config = config_for(&profile);
        let mut state = ChoiceState::new(&profile, &config);
        // Cycle the GPU mode field until current == SingleGpu. Cap
        // the loop so a future regression cannot infinite-loop the
        // test.
        for _ in 0..10 {
            if let ChoiceField::GpuMode { current, .. } = &state.fields[0] {
                if *current == GpuPassthroughMode::SingleGpu {
                    break;
                }
            }
            state.apply(ChoiceAction::IncrementValue);
        }

        let mut updated = config.clone();
        state.apply_to(&profile, &mut updated);
        assert_eq!(updated.gpu_mode, GpuPassthroughMode::SingleGpu);
        let pass_count = updated
            .gpu_roles
            .iter()
            .filter(|r| r.role == GpuRole::Passthrough)
            .count();
        assert_eq!(
            pass_count, 1,
            "exactly one Passthrough GPU expected after SingleGpu"
        );
        let host_count = updated
            .gpu_roles
            .iter()
            .filter(|r| r.role == GpuRole::Host)
            .count();
        assert_eq!(host_count, 0, "no Host GPU expected in SingleGpu mode");
    }

    #[test]
    fn cycling_gpu_mode_to_dual_gpu_assigns_first_two_discretes() {
        // Build a host with two discrete GPUs.
        let profile =
            crate::tui::screens::detection::tests_helpers::dummy_profile_with_extras(vec![
                (
                    "0000:01:00.0",
                    GpuType::Discrete,
                    GpuVendor::Nvidia,
                    "10de",
                    "1f08",
                ),
                (
                    "0000:02:00.0",
                    GpuType::Discrete,
                    GpuVendor::Amd,
                    "1002",
                    "7590",
                ),
            ]);
        let mut config = config_for(&profile);
        config.gpu_mode = GpuPassthroughMode::SingleGpu;

        let mut state = ChoiceState::new(&profile, &config);
        for _ in 0..10 {
            if let ChoiceField::GpuMode { current, .. } = &state.fields[0] {
                if *current == GpuPassthroughMode::DualGpu {
                    break;
                }
            }
            state.apply(ChoiceAction::IncrementValue);
        }

        let mut updated = config.clone();
        state.apply_to(&profile, &mut updated);
        assert_eq!(updated.gpu_mode, GpuPassthroughMode::DualGpu);
        let pass = updated
            .gpu_roles
            .iter()
            .filter(|r| r.role == GpuRole::Passthrough)
            .count();
        let host = updated
            .gpu_roles
            .iter()
            .filter(|r| r.role == GpuRole::Host)
            .count();
        assert_eq!(host, 1);
        assert_eq!(pass, 1);
    }

    #[test]
    fn applying_two_monitors_choice_picks_first_two_connected_drm_connectors() {
        let mut profile = dual_gpu_profile();
        profile.monitors = vec![
            crate::detect::monitors::MonitorInfo {
                connector_name: "DP-1".to_string(),
                connected: true,
                current_mode: None,
                card: "card0".to_string(),
                gpu_pci_slot: None,
                is_internal: false,
            },
            crate::detect::monitors::MonitorInfo {
                connector_name: "HDMI-1".to_string(),
                connected: true,
                current_mode: None,
                card: "card0".to_string(),
                gpu_pci_slot: None,
                is_internal: false,
            },
        ];
        let config = config_for(&profile);
        let mut state = ChoiceState::new(&profile, &config);
        state.selected = 1; // monitor plan field
                            // Cycle until current == TwoMonitorsHostAndVm.
        for _ in 0..5 {
            if let ChoiceField::MonitorPlan { current, .. } = &state.fields[1] {
                if *current == MonitorPlanChoice::TwoMonitorsHostAndVm {
                    break;
                }
            }
            state.apply(ChoiceAction::IncrementValue);
        }

        let mut updated = config.clone();
        state.apply_to(&profile, &mut updated);
        match updated.monitor_plan {
            MonitorPlan::TwoMonitors {
                host_connector,
                vm_connector,
            } => {
                assert_eq!(host_connector, "DP-1");
                assert_eq!(vm_connector, "HDMI-1");
            }
            other => panic!("expected TwoMonitors, got {other:?}"),
        }
    }

    #[test]
    fn render_paints_terminal_buffer_without_panicking() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let profile = dual_gpu_profile();
        let state = ChoiceState::new(&profile, &config_for(&profile));
        terminal
            .draw(|f| {
                let area = f.size();
                render(f, area, &state);
            })
            .expect("render must not panic on a 120x40 terminal");

        let buffer = terminal.backend().buffer().clone();
        let mut top_text = String::new();
        for x in 0..buffer.area().width {
            top_text.push_str(buffer.get(x, 1).symbol());
        }
        assert!(top_text.contains("Configure"));
    }
}
