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
use crate::vm::PassthroughConfig;

/// Top-level wizard state machine. Each screen is one variant. Slice
/// 10.2 ships only `Detection`; slice 10.3 adds `Choices`, slice 10.4
/// adds `PlanPreview`, and a closing `Confirmed` will signal "user
/// approved, hand off to the planner".
#[derive(Debug, Clone, PartialEq, Eq)]
enum Screen {
    /// Read-only detection summary with the compatibility report.
    Detection,
    /// Interactive choice flow: GPU mode, monitor plan, RAM, vCPU.
    Choices,
    /// User pressed quit; the event loop exits at the top of the next
    /// iteration. This variant exists so the loop can distinguish
    /// "user wants out" from "render and wait again".
    Exiting,
}

/// Aggregate UI state. Built once when the wizard starts and read by
/// every screen on every frame. Future slices add fields here for the
/// final plan-preview and confirmation step.
struct App {
    screen: Screen,
    profile: SystemProfile,
    report: CompatibilityReport,
    config: PassthroughConfig,
    choices: ChoiceState,
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
    let result = run_event_loop(&mut terminal, App::new(profile, report, config));
    // Always restore the terminal, even if the loop errored. The user
    // should never have to type `reset` because Virtu crashed.
    let restore = restore_terminal(&mut terminal);
    result.and(restore)
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
        }
    }
}

fn run_event_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, mut app: App) -> Result<()> {
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
            Screen::Exiting => break,
        }

        match next_event()? {
            Some(WizardEvent::Quit) => app.screen = Screen::Exiting,
            Some(WizardEvent::Continue) => match app.screen {
                Screen::Detection => app.screen = Screen::Choices,
                // The plan-preview screen lands in slice 10.4. For now,
                // pressing Enter on the choice screen flushes the
                // current choices into `app.config` and noops; the
                // user can still see the live preview by leaving the
                // wizard via `q` and running `virtu plan`.
                Screen::Choices => {
                    app.choices.apply_to(&app.profile, &mut app.config);
                }
                Screen::Exiting => {}
            },
            Some(WizardEvent::ChoiceMove(action)) if app.screen == Screen::Choices => {
                app.choices.apply(action);
                // Mirror the user's edits into the working config
                // immediately so the preview screen (slice 10.4)
                // always sees current data without an extra commit
                // step. Costs a few clones per keypress; the
                // config struct is small.
                app.choices.apply_to(&app.profile, &mut app.config);
            }
            Some(WizardEvent::ChoiceMove(_)) => {
                // ChoiceMove on a non-Choices screen: ignore. Slice
                // 10.4 may bind these to the plan-preview scroll
                // controls; today they're a no-op.
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

fn map_key(code: KeyCode) -> WizardEvent {
    match code {
        KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => WizardEvent::Quit,
        KeyCode::Enter => WizardEvent::Continue,
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
