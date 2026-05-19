//! Interactive Virtu wizard (Milestone 10, slice 10.2 scaffold).
//!
//! Runs against `ratatui` + `crossterm`. The wizard owns one terminal
//! lifecycle (`enter_alt_screen` plus `enable_raw_mode` followed later
//! by `disable_raw_mode` plus `leave_alt_screen`), drives an event loop
//! that polls keys with a short timeout so the UI stays responsive,
//! and dispatches to the current screen's draw routine.
//!
//! Slice 10.2 ships only the detection screen. Quitting (`q` or `Esc`)
//! exits cleanly and restores the terminal. `Enter` is reserved for the
//! choice flow that lands in slice 10.3; today it is a no-op.
//!
//! The `App` state machine is small on purpose: each future slice adds
//! one variant and one transition. Keeping the logic out of the screen
//! modules means a future maintainer can change the flow (e.g. allow
//! "back" navigation) without touching every screen renderer.

pub mod screens;

use std::io::{self, Stdout};
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use crate::detect::{self, SystemProfile};
use crate::engine::{build_compatibility_report, CompatibilityReport};
use crate::tui::screens::choices::{self, ChoiceAction, ChoiceState};
use crate::tui::screens::detection::{self, DetectionView};
use crate::tui::screens::preview::{self, PreviewAction, PreviewState};
use crate::vm::PassthroughConfig;

/// Top-level wizard state machine. Each screen is one variant. Slice
/// 10.2 added `Detection`, 10.3 added `Choices`, and 10.4 closes the
/// loop with `Preview`. After confirmation the loop exits and the CLI
/// prints the chosen plan to stdout.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Screen {
    /// Read-only detection summary with the compatibility report.
    Detection,
    /// Interactive choice flow: GPU mode, monitor plan, RAM, vCPU.
    Choices,
    /// Plan preview + confirmation. The user reviews every step the
    /// planner emitted and either confirms or backs out.
    Preview,
    /// User pressed quit; the event loop exits at the top of the next
    /// iteration. This variant exists so the loop can distinguish
    /// "user wants out" from "render and wait again".
    Exiting,
}

/// Aggregate UI state. Built once when the wizard starts and read by
/// every screen on every frame.
struct App {
    screen: Screen,
    profile: SystemProfile,
    report: CompatibilityReport,
    config: PassthroughConfig,
    choices: ChoiceState,
    /// Lazily built when the user first advances to the preview
    /// screen. Rebuilt every time they move from Choices → Preview so
    /// late edits propagate.
    preview: Option<PreviewState>,
}

/// Entry point invoked from `virtu wizard` (and the default `virtu`
/// command). Handles the entire terminal lifecycle: detection runs
/// before raw mode is enabled so a long scan with a panic does not
/// leave the user's terminal stuck.
pub async fn run_wizard() -> Result<()> {
    if !is_terminal_attached() {
        // Running under a redirect or in a CI context. Print a sensible
        // fallback so `virtu` (no subcommand) still does something.
        return run_wizard_text_fallback().await;
    }

    let profile = detect::scan_system()
        .await
        .context("scanning host before launching the wizard")?;
    let report = build_compatibility_report(&profile);
    let config = PassthroughConfig::recommended_defaults(&profile).unwrap_or_else(|| {
        // Profile has no GPUs; recommended_defaults returns None. The
        // wizard still runs (the user can read the detection summary)
        // but the choice flow has nothing to edit.
        synthetic_empty_config()
    });

    let mut terminal = setup_terminal().context("entering the alternate-screen TUI")?;
    let mut app = App::new(profile, report, config);
    let result = run_event_loop(&mut terminal, &mut app);
    // Always restore the terminal, even if the loop errored. The user
    // should never have to type `reset` because Virtu crashed.
    let restore = restore_terminal(&mut terminal);
    result.and(restore)?;

    if app.was_confirmed() {
        print_confirmed_plan(&app);
    } else {
        println!("Wizard exited without confirming a plan.");
    }
    Ok(())
}

/// Print the plan the user just confirmed in plain text after the
/// alternate-screen has been torn down. The CLI does not (yet) drop
/// directly into `virtu apply`; the user reviews the printed plan and
/// runs the apply command themselves. Wiring auto-apply lives in a
/// follow-up slice so the wizard can stay non-mutating until then.
fn print_confirmed_plan(app: &App) {
    use crate::engine::plan;
    println!("=== VIRTU WIZARD: CONFIRMED PLAN ===");
    println!(
        "VM name:       {}\nGuest OS:      {:?}\nGPU mode:      {}\nMonitor plan:  {:?}",
        app.config.vm_name, app.config.guest_os, app.config.gpu_mode, app.config.monitor_plan,
    );
    println!(
        "VM resources:  {} MiB RAM, {} vCPUs, disk {:?}",
        app.config.resources.ram_mb, app.config.resources.vcpu_count, app.config.resources.disk
    );
    println!();
    match plan(&app.profile, &app.report, &app.config) {
        Ok(plan) => {
            println!("{} step(s):", plan.steps.len());
            for (idx, step) in plan.steps.iter().enumerate() {
                println!("  {:>2}. [{}] {}", idx + 1, step.risk, step.title);
            }
            println!(
                "\nMax risk: {}    Reboot required: {}    Confirmation required: {}",
                plan.summary.max_risk,
                plan.summary.requires_reboot,
                plan.summary.requires_confirmation,
            );
            println!(
                "\nReview, then run:\n  virtu apply --phase a --confirm   # to start the host edits\n  virtu rollback --to <id>          # to undo, after a snapshot exists"
            );
        }
        Err(err) => {
            println!("Plan generation failed after wizard confirmation: {err}");
        }
    }
}

/// Stand-in `PassthroughConfig` used when the host has no GPUs at all.
/// `recommended_defaults` returns `None` in that case; the wizard still
/// renders the detection summary (which surfaces the no-GPUs finding)
/// but the choice flow has nothing meaningful to edit. Building a
/// minimal config here keeps the type system happy without wiring an
/// `Option<PassthroughConfig>` through every screen.
fn synthetic_empty_config() -> PassthroughConfig {
    use crate::vm::{
        AudioChoice, DiskChoice, DiskFormat, GpuPassthroughMode, GuestOs, InputChoice,
        LookingGlassChoice, MonitorPlan, NetworkChoice, SingleMonitorStrategy, VmResources,
    };
    PassthroughConfig {
        vm_name: "virtu-windows".to_string(),
        guest_os: GuestOs::Windows11,
        gpu_mode: GpuPassthroughMode::SingleGpu,
        gpu_roles: Vec::new(),
        monitor_plan: MonitorPlan::OneMonitor {
            strategy: SingleMonitorStrategy::SwitchInputs,
        },
        looking_glass: LookingGlassChoice::Disabled,
        iso_path: None,
        resources: VmResources {
            ram_mb: 8 * 1024,
            vcpu_count: 4,
            disk: DiskChoice::Create {
                path: std::path::PathBuf::from("/var/lib/libvirt/images/virtu-windows.qcow2"),
                size_gb: 100,
                format: DiskFormat::Qcow2,
            },
        },
        network: NetworkChoice::Nat,
        audio: AudioChoice::None,
        input: InputChoice::default(),
    }
}

impl App {
    fn new(profile: SystemProfile, report: CompatibilityReport, config: PassthroughConfig) -> Self {
        let choices = ChoiceState::new(&profile, &config);
        Self {
            screen: Screen::Detection,
            profile,
            report,
            config,
            choices,
            preview: None,
        }
    }

    fn enter_preview(&mut self) {
        self.preview = Some(PreviewState::new(&self.profile, &self.report, &self.config));
        self.screen = Screen::Preview;
    }

    /// True when the wizard finished successfully with a confirmed
    /// plan. The CLI checks this after the event loop returns and
    /// prints the plan to stdout once the alternate-screen has been
    /// torn down.
    fn was_confirmed(&self) -> bool {
        matches!(&self.preview, Some(state) if state.confirmed)
    }
}

fn run_event_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    // The detection view is rebuilt on Refresh events (resize) so the
    // pre-rendered text stays in sync with `app.profile` / `app.report`.
    let mut detection_view = DetectionView::new(&app.profile, &app.report);

    while app.screen != Screen::Exiting {
        match app.screen {
            Screen::Detection => {
                terminal
                    .draw(|frame| {
                        let area = frame.size();
                        detection::render(frame, area, &detection_view);
                    })
                    .context("drawing the detection screen")?;
            }
            Screen::Choices => {
                terminal
                    .draw(|frame| {
                        let area = frame.size();
                        choices::render(frame, area, &app.choices);
                    })
                    .context("drawing the choices screen")?;
            }
            Screen::Preview => {
                let state = app
                    .preview
                    .as_ref()
                    .expect("preview state must be built before entering the preview screen");
                terminal
                    .draw(|frame| {
                        let area = frame.size();
                        preview::render(frame, area, state);
                    })
                    .context("drawing the preview screen")?;
            }
            Screen::Exiting => break,
        }

        match next_event()? {
            Some(WizardEvent::Quit) => match app.screen {
                // From the preview screen, `q`/`Esc` go back to
                // choices so the user can amend without losing the
                // wizard. From any other screen quit closes the
                // whole wizard.
                Screen::Preview => {
                    app.screen = Screen::Choices;
                    app.preview = None;
                }
                _ => app.screen = Screen::Exiting,
            },
            Some(WizardEvent::Continue) => match app.screen {
                Screen::Detection => app.screen = Screen::Choices,
                Screen::Choices => {
                    // Commit any pending edits and build a fresh
                    // preview from the current config.
                    app.choices.apply_to(&app.profile, &mut app.config);
                    app.enter_preview();
                }
                Screen::Preview => {} // Confirm goes through PreviewMove
                Screen::Exiting => {}
            },
            Some(WizardEvent::ChoiceMove(action)) if app.screen == Screen::Choices => {
                app.choices.apply(action);
                // Mirror the user's edits into the working config
                // immediately so the preview screen always sees
                // current data without an extra commit step. Costs a
                // few clones per keypress; the config struct is
                // small.
                app.choices.apply_to(&app.profile, &mut app.config);
            }
            Some(WizardEvent::ChoiceMove(action)) if app.screen == Screen::Preview => {
                // On the preview screen, j/k/arrow translate to
                // scroll. h/l are ignored (no horizontal layout).
                let preview_action = match action {
                    ChoiceAction::PrevField => Some(PreviewAction::ScrollUp),
                    ChoiceAction::NextField => Some(PreviewAction::ScrollDown),
                    _ => None,
                };
                if let (Some(preview_action), Some(state)) = (preview_action, app.preview.as_mut())
                {
                    state.apply(preview_action);
                }
            }
            Some(WizardEvent::ChoiceMove(_)) => {
                // ChoiceMove on the detection screen: ignore.
            }
            Some(WizardEvent::PreviewMove(action)) if app.screen == Screen::Preview => {
                if let Some(state) = app.preview.as_mut() {
                    state.apply(action);
                    if state.confirmed {
                        app.screen = Screen::Exiting;
                    }
                }
            }
            Some(WizardEvent::PreviewMove(_)) => {
                // PreviewMove on a non-Preview screen: ignore.
            }
            Some(WizardEvent::Refresh) => {
                detection_view = DetectionView::new(&app.profile, &app.report);
            }
            None => {} // tick timeout; redraw on next loop
        }
    }

    Ok(())
}

/// Discrete events the wizard cares about. Mapping `crossterm` events
/// here keeps the screen modules from depending on `crossterm` types
/// directly, which makes them trivial to test with synthetic input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WizardEvent {
    Quit,
    Continue,
    /// One step inside the choice screen. Slice 10.3 surfaces these as
    /// arrow / hjkl keys; the choice screen interprets them.
    ChoiceMove(ChoiceAction),
    /// One step inside the preview screen. Slice 10.4 surfaces these
    /// as scroll / confirm keys.
    PreviewMove(PreviewAction),
    Refresh,
}

fn next_event() -> Result<Option<WizardEvent>> {
    // Poll with a short timeout so the loop stays responsive (terminal
    // resizes, future periodic refresh) without burning CPU.
    if !event::poll(Duration::from_millis(250))? {
        return Ok(None);
    }
    match event::read()? {
        Event::Key(key) if key.kind == KeyEventKind::Press => Ok(Some(map_key(key.code))),
        Event::Resize(_, _) => Ok(Some(WizardEvent::Refresh)),
        _ => Ok(None),
    }
}

/// Map a `crossterm` keycode onto a `WizardEvent`. The mapping is
/// shared across screens; the event loop dispatches based on the
/// current screen so the same key can be a `ChoiceMove` on one
/// screen and a `PreviewMove` on another. Confirm (`c`) is a
/// dedicated keybind so users do not accidentally hit it from the
/// choice screen.
fn map_key(code: KeyCode) -> WizardEvent {
    match code {
        KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => WizardEvent::Quit,
        KeyCode::Enter => WizardEvent::Continue,
        KeyCode::Char('c') | KeyCode::Char('C') => WizardEvent::PreviewMove(PreviewAction::Confirm),
        KeyCode::PageUp => WizardEvent::PreviewMove(PreviewAction::PageUp),
        KeyCode::PageDown => WizardEvent::PreviewMove(PreviewAction::PageDown),
        KeyCode::Up | KeyCode::Char('k') => WizardEvent::ChoiceMove(ChoiceAction::PrevField),
        KeyCode::Down | KeyCode::Char('j') => WizardEvent::ChoiceMove(ChoiceAction::NextField),
        KeyCode::Left | KeyCode::Char('h') => WizardEvent::ChoiceMove(ChoiceAction::DecrementValue),
        KeyCode::Right | KeyCode::Char('l') => {
            WizardEvent::ChoiceMove(ChoiceAction::IncrementValue)
        }
        _ => WizardEvent::Refresh,
    }
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode().context("enabling raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .context("switching to the alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend).context("constructing the ratatui Terminal")
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode().context("disabling raw mode")?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )
    .context("leaving the alternate screen")?;
    terminal
        .show_cursor()
        .context("restoring cursor visibility")?;
    Ok(())
}

/// True when stdin/stdout look like a real interactive terminal.
/// Refuses to enter raw mode when run under a redirect (`virtu | tee`)
/// or a CI runner; the fallback prints a usable text summary instead.
fn is_terminal_attached() -> bool {
    use std::io::IsTerminal;
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

/// Plain-text fallback the wizard runs when no TTY is attached (CI,
/// `virtu | head`, etc.). Print enough that piping the output is still
/// useful while we explain why the interactive flow was skipped.
async fn run_wizard_text_fallback() -> Result<()> {
    println!("Virtu wizard cannot launch: stdin or stdout is not a TTY.");
    println!("Run `virtu scan` for the detection summary, or invoke `virtu wizard`");
    println!("from an interactive terminal.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quit_keys_map_to_quit_event() {
        assert_eq!(map_key(KeyCode::Char('q')), WizardEvent::Quit);
        assert_eq!(map_key(KeyCode::Char('Q')), WizardEvent::Quit);
        assert_eq!(map_key(KeyCode::Esc), WizardEvent::Quit);
    }

    #[test]
    fn enter_maps_to_continue_event() {
        assert_eq!(map_key(KeyCode::Enter), WizardEvent::Continue);
    }

    #[test]
    fn navigation_keys_map_to_choice_move_actions() {
        // hjkl + arrow keys both work, mirroring vim and the terminal
        // standard. Anything that isn't bound returns Refresh so the
        // screen redraws cleanly.
        assert_eq!(
            map_key(KeyCode::Up),
            WizardEvent::ChoiceMove(ChoiceAction::PrevField)
        );
        assert_eq!(
            map_key(KeyCode::Char('k')),
            WizardEvent::ChoiceMove(ChoiceAction::PrevField)
        );
        assert_eq!(
            map_key(KeyCode::Down),
            WizardEvent::ChoiceMove(ChoiceAction::NextField)
        );
        assert_eq!(
            map_key(KeyCode::Char('j')),
            WizardEvent::ChoiceMove(ChoiceAction::NextField)
        );
        assert_eq!(
            map_key(KeyCode::Left),
            WizardEvent::ChoiceMove(ChoiceAction::DecrementValue)
        );
        assert_eq!(
            map_key(KeyCode::Char('h')),
            WizardEvent::ChoiceMove(ChoiceAction::DecrementValue)
        );
        assert_eq!(
            map_key(KeyCode::Right),
            WizardEvent::ChoiceMove(ChoiceAction::IncrementValue)
        );
        assert_eq!(
            map_key(KeyCode::Char('l')),
            WizardEvent::ChoiceMove(ChoiceAction::IncrementValue)
        );
    }

    #[test]
    fn unbound_keys_request_refresh_so_the_screen_stays_responsive() {
        // Pressing space or any other key the wizard hasn't claimed
        // shouldn't quit, advance, or move. We map it to Refresh so
        // screens that scroll (slice 10.4's plan preview) can react
        // later.
        assert_eq!(map_key(KeyCode::Char(' ')), WizardEvent::Refresh);
        assert_eq!(map_key(KeyCode::Char('?')), WizardEvent::Refresh);
    }
}
