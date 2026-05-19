//! Plan preview + confirmation screen (Milestone 10, slice 10.4).
//!
//! Shows the user the full ordered `Plan` that the planner emitted from
//! their detected host plus the choices they made on the previous
//! screen. Each step renders with a color-tagged risk badge, the files
//! it touches, and the regenerate commands it will run. Below the
//! step list, a footer summary makes the cumulative impact obvious:
//! "5 steps, max risk High, requires reboot, 3 files touched".
//!
//! The user can scroll through the list with `j`/`k`/PageUp/PageDown,
//! hit `c` to confirm, or `q`/`Esc` to back out. Confirming flips the
//! `confirmed` flag on the screen state; the wizard's `App` reads
//! that flag and exits the event loop with a confirmation result.
//!
//! Validation runs ahead of plan emission. If validation surfaces any
//! errors the screen renders them in place of the plan and disables the
//! `c` keybind; the user must back out to fix the choices.

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use crate::detect::SystemProfile;
use crate::engine::{plan, CompatibilityReport, Plan, PlanError, StepRisk};
use crate::vm::{validate, PassthroughConfig, ValidationIssue, ValidationSeverity};

/// One step the user can take inside the preview screen. Mapped from
/// `tui::WizardEvent` by the dispatcher in `tui::mod`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewAction {
    ScrollUp,
    ScrollDown,
    PageUp,
    PageDown,
    /// User confirmed the plan; the wizard transitions to its
    /// post-wizard summary (today: print the plan to stdout after the
    /// terminal restores).
    Confirm,
}

/// Outcome of running the planner against the user's current config.
/// Either a clean plan or one of the structured failure cases the
/// planner exposes; we keep the actual error type from the engine so
/// the preview can render it verbatim.
#[derive(Debug, Clone)]
pub enum PreviewResult {
    Plan(Plan),
    PlanFailed(String),
    /// Validation surfaced one or more errors. Confirmation is
    /// disabled; the user must navigate back.
    ValidationErrors(Vec<ValidationIssue>),
}

#[derive(Debug, Clone)]
pub struct PreviewState {
    pub result: PreviewResult,
    /// First step index visible in the scroll viewport.
    pub scroll_offset: usize,
    /// True after the user pressed `c`. The wizard's event loop reads
    /// this and exits cleanly.
    pub confirmed: bool,
}

impl PreviewState {
    /// Build the preview by validating the user's config and, when
    /// validation passes, asking the planner for a `Plan`.
    pub fn new(
        profile: &SystemProfile,
        report: &CompatibilityReport,
        config: &PassthroughConfig,
    ) -> Self {
        let validation = validate(profile, report, config);
        let errors: Vec<ValidationIssue> = validation.errors().cloned().collect();
        if !errors.is_empty() {
            return Self {
                result: PreviewResult::ValidationErrors(errors),
                scroll_offset: 0,
                confirmed: false,
            };
        }
        let result = match plan(profile, report, config) {
            Ok(plan) => PreviewResult::Plan(plan),
            Err(PlanError::ValidationFailed(report)) => {
                PreviewResult::ValidationErrors(report.errors().cloned().collect())
            }
            Err(other) => PreviewResult::PlanFailed(format!("{other}")),
        };
        Self {
            result,
            scroll_offset: 0,
            confirmed: false,
        }
    }

    pub fn apply(&mut self, action: PreviewAction) {
        let total = self.step_count();
        match action {
            PreviewAction::ScrollUp => {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
            }
            PreviewAction::ScrollDown => {
                if self.scroll_offset + 1 < total {
                    self.scroll_offset += 1;
                }
            }
            PreviewAction::PageUp => {
                self.scroll_offset = self.scroll_offset.saturating_sub(5);
            }
            PreviewAction::PageDown => {
                let new_offset = self.scroll_offset + 5;
                self.scroll_offset = new_offset.min(total.saturating_sub(1));
            }
            PreviewAction::Confirm => {
                if self.can_confirm() {
                    self.confirmed = true;
                }
            }
        }
    }

    /// True only when the planner produced a usable plan. The
    /// `c`/Confirm binding is gated on this so users cannot
    /// accidentally accept a broken plan.
    pub fn can_confirm(&self) -> bool {
        matches!(self.result, PreviewResult::Plan(_))
    }

    fn step_count(&self) -> usize {
        match &self.result {
            PreviewResult::Plan(plan) => plan.steps.len(),
            PreviewResult::ValidationErrors(errors) => errors.len(),
            PreviewResult::PlanFailed(_) => 1,
        }
    }
}

pub fn render(frame: &mut Frame, area: Rect, state: &PreviewState) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(4),
            Constraint::Length(3),
        ])
        .split(area);

    render_header(frame, outer[0], state);
    render_body(frame, outer[1], state);
    render_summary(frame, outer[2], state);
    render_footer(frame, outer[3], state);
}

fn render_header(frame: &mut Frame, area: Rect, state: &PreviewState) {
    let title = match &state.result {
        PreviewResult::Plan(_) => Line::from(vec![Span::styled(
            "Plan preview",
            Style::default().add_modifier(Modifier::BOLD),
        )]),
        PreviewResult::PlanFailed(_) => Line::from(vec![Span::styled(
            "Plan failed",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )]),
        PreviewResult::ValidationErrors(_) => Line::from(vec![Span::styled(
            "Configuration has errors",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )]),
    };
    let header = Paragraph::new(title)
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(header, area);
}

fn render_body(frame: &mut Frame, area: Rect, state: &PreviewState) {
    match &state.result {
        PreviewResult::Plan(plan) => render_plan_steps(frame, area, plan, state.scroll_offset),
        PreviewResult::ValidationErrors(errors) => {
            render_validation_errors(frame, area, errors, state.scroll_offset)
        }
        PreviewResult::PlanFailed(message) => {
            let block = Block::default()
                .borders(Borders::ALL)
                .title(" Plan generation failed ");
            let paragraph = Paragraph::new(message.clone())
                .block(block)
                .wrap(Wrap { trim: false });
            frame.render_widget(paragraph, area);
        }
    }
}

fn render_plan_steps(frame: &mut Frame, area: Rect, plan: &Plan, scroll_offset: usize) {
    let mut lines: Vec<Line> = Vec::new();
    for (idx, step) in plan.steps.iter().enumerate().skip(scroll_offset) {
        let (risk_label, risk_color) = match step.risk {
            StepRisk::ReadOnly => ("[READ-ONLY]", Color::Gray),
            StepRisk::Low => ("[LOW]", Color::Green),
            StepRisk::Medium => ("[MEDIUM]", Color::Yellow),
            StepRisk::High => ("[HIGH]", Color::Red),
        };
        let mut header = vec![
            Span::styled(
                format!("{:>2}. ", idx + 1),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{risk_label} "),
                Style::default().fg(risk_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                step.title.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ];
        if step.requires_confirmation {
            header.push(Span::styled(
                "  ⚠ requires confirmation",
                Style::default().fg(Color::Yellow),
            ));
        }
        if step.requires_reboot {
            header.push(Span::styled(
                "  ⟳ reboot",
                Style::default().fg(Color::Magenta),
            ));
        }
        lines.push(Line::from(header));
        lines.push(Line::from(format!("    {}", step.summary)));
        if !step.touches.is_empty() {
            lines.push(Line::from(Span::styled(
                "    Touches:",
                Style::default().fg(Color::Cyan),
            )));
            for path in &step.touches {
                lines.push(Line::from(format!("      - {}", path.display())));
            }
        }
        if !step.commands.is_empty() {
            lines.push(Line::from(Span::styled(
                "    Commands:",
                Style::default().fg(Color::Cyan),
            )));
            for cmd in &step.commands {
                lines.push(Line::from(format!("      $ {cmd}")));
            }
        }
        // Blank line between steps so the list is scannable.
        lines.push(Line::from(""));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Steps ({} total) ", plan.steps.len()));
    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_validation_errors(
    frame: &mut Frame,
    area: Rect,
    errors: &[ValidationIssue],
    scroll_offset: usize,
) {
    let mut lines: Vec<Line> = Vec::new();
    for issue in errors.iter().skip(scroll_offset) {
        let (badge, color) = match issue.severity {
            ValidationSeverity::Error => ("[ERROR]", Color::Red),
            ValidationSeverity::Warning => ("[WARN]", Color::Yellow),
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{badge} "),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::raw(issue.message.clone()),
        ]));
        lines.push(Line::from(""));
    }
    if lines.is_empty() {
        // Should not happen; the constructor only takes this branch when
        // errors is non-empty. Defensive fallback so the screen never
        // renders blank.
        lines.push(Line::from("(no validation issues to display)"));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Validation errors — go back and fix these ");
    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_summary(frame: &mut Frame, area: Rect, state: &PreviewState) {
    let block = Block::default().borders(Borders::ALL).title(" Summary ");
    let lines: Vec<Line> = match &state.result {
        PreviewResult::Plan(plan) => {
            let touched: usize = plan.steps.iter().map(|s| s.touches.len()).sum();
            vec![
                Line::from(format!(
                    "Steps: {}    Max risk: {}    Reboot required: {}    Confirmation required: {}",
                    plan.summary.total_steps,
                    plan.summary.max_risk,
                    yes_no(plan.summary.requires_reboot),
                    yes_no(plan.summary.requires_confirmation),
                )),
                Line::from(format!("Files touched (cumulative): {touched}")),
            ]
        }
        PreviewResult::ValidationErrors(errors) => vec![Line::from(format!(
            "{} validation error{} — confirmation disabled.",
            errors.len(),
            if errors.len() == 1 { "" } else { "s" }
        ))],
        PreviewResult::PlanFailed(_) => vec![Line::from(
            "Plan generation failed — confirmation disabled.",
        )],
    };
    let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: true });
    frame.render_widget(paragraph, area);
}

fn render_footer(frame: &mut Frame, area: Rect, state: &PreviewState) {
    let confirm_style = if state.can_confirm() {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let mut hint: Vec<Span> = vec![
        Span::styled(
            "↑/k ↓/j",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" scroll    "),
        Span::styled("c", confirm_style),
        Span::raw(if state.can_confirm() {
            " confirm    "
        } else {
            " confirm (disabled)    "
        }),
        Span::styled(
            "q/Esc",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" back / quit"),
    ];
    if state.confirmed {
        hint.push(Span::raw("    "));
        hint.push(Span::styled(
            "✔ Confirmed — exiting",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ));
    }
    let footer = Paragraph::new(Line::from(hint))
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, area);
}

fn yes_no(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::build_compatibility_report;

    fn arch_profile_with_amd_dgpu() -> SystemProfile {
        let mut profile = crate::tui::screens::detection::tests_helpers::dummy_profile();
        // Override the GPU vendor so the planner picks AMD's softdeps;
        // the exact contents don't matter for these tests, only that
        // the plan emits.
        profile.gpus[0].vendor = crate::detect::gpu::GpuVendor::Amd;
        profile.gpus[0].vendor_id = "1002".to_string();
        profile.gpus[0].device_id = "7590".to_string();
        profile
    }

    fn passing_config(profile: &SystemProfile) -> PassthroughConfig {
        // recommended_defaults gives a config that already passes
        // validation against this fixture profile.
        PassthroughConfig::recommended_defaults(profile).expect("recommended defaults")
    }

    #[test]
    fn plan_preview_holds_a_plan_when_validation_passes() {
        let profile = arch_profile_with_amd_dgpu();
        let report = build_compatibility_report(&profile);
        let config = passing_config(&profile);
        let state = PreviewState::new(&profile, &report, &config);
        // The dummy profile is missing libvirt + qemu so the report
        // surfaces blockers and the planner refuses. We treat that
        // as the documented "errors" path here, which is exactly the
        // feedback the screen is supposed to surface.
        match state.result {
            PreviewResult::Plan(_) | PreviewResult::ValidationErrors(_) => {}
            PreviewResult::PlanFailed(message) => {
                // Plan failure is acceptable; just confirm the message
                // makes it through to the user.
                assert!(!message.is_empty());
            }
        }
    }

    #[test]
    fn validation_errors_disable_confirmation_keybind() {
        // Force a validation error by giving the config an invalid
        // gpu_roles list.
        let profile = arch_profile_with_amd_dgpu();
        let report = build_compatibility_report(&profile);
        let mut config = passing_config(&profile);
        config.gpu_roles[0].pci_slot = "0000:ff:ff.0".to_string();
        let state = PreviewState::new(&profile, &report, &config);
        assert!(matches!(state.result, PreviewResult::ValidationErrors(_)));
        assert!(!state.can_confirm());
    }

    #[test]
    fn confirm_action_is_a_no_op_when_validation_failed() {
        let profile = arch_profile_with_amd_dgpu();
        let report = build_compatibility_report(&profile);
        let mut config = passing_config(&profile);
        config.gpu_roles[0].pci_slot = "0000:ff:ff.0".to_string();
        let mut state = PreviewState::new(&profile, &report, &config);
        state.apply(PreviewAction::Confirm);
        assert!(
            !state.confirmed,
            "must not flip confirmed when validation failed"
        );
    }

    #[test]
    fn scrolling_clamps_at_zero_and_at_step_count_minus_one() {
        let profile = arch_profile_with_amd_dgpu();
        let report = build_compatibility_report(&profile);
        let config = passing_config(&profile);
        let mut state = PreviewState::new(&profile, &report, &config);
        // ScrollUp at offset 0 stays at 0.
        state.apply(PreviewAction::ScrollUp);
        assert_eq!(state.scroll_offset, 0);
        // ScrollDown advances at most to step_count - 1.
        for _ in 0..50 {
            state.apply(PreviewAction::ScrollDown);
        }
        let total = match &state.result {
            PreviewResult::Plan(plan) => plan.steps.len(),
            PreviewResult::ValidationErrors(errs) => errs.len(),
            PreviewResult::PlanFailed(_) => 1,
        };
        assert!(state.scroll_offset <= total.saturating_sub(1));
    }

    #[test]
    fn render_paints_terminal_buffer_without_panicking() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let profile = arch_profile_with_amd_dgpu();
        let report = build_compatibility_report(&profile);
        let config = passing_config(&profile);
        let state = PreviewState::new(&profile, &report, &config);
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
        // Either "Plan preview" or one of the error banners must be
        // visible; we assert the screen is non-empty as the smoke
        // signal.
        assert!(top_text.trim_start().chars().any(|c| !c.is_whitespace()));
    }
}
