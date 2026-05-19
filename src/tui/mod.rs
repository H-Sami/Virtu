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
use crate::tui::screens::detection::{self, DetectionView};

/// Top-level wizard state machine. Each screen is one variant. Slice
/// 10.2 ships only `Detection`; slice 10.3 adds `Choices`, slice 10.4
/// adds `PlanPreview`, and a closing `Confirmed` will signal "user
/// approved, hand off to the planner".
#[derive(Debug, Clone, PartialEq, Eq)]
enum Screen {
    /// Read-only detection summary with the compatibility report.
    Detection,
    /// User pressed quit; the event loop exits at the top of the next
    /// iteration. This variant exists so the loop can distinguish
    /// "user wants out" from "render and wait again".
    Exiting,
}

/// Aggregate UI state. Built once when the wizard starts and read by
/// every screen on every frame. Future slices add fields here for the
/// user's in-progress `PassthroughConfig`.
struct App {
    screen: Screen,
    profile: SystemProfile,
    report: CompatibilityReport,
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

    let mut terminal = setup_terminal().context("entering the alternate-screen TUI")?;
    let result = run_event_loop(&mut terminal, App::new(profile, report));
    // Always restore the terminal, even if the loop errored. The user
    // should never have to type `reset` because Virtu crashed.
    let restore = restore_terminal(&mut terminal);
    result.and(restore)
}

impl App {
    fn new(profile: SystemProfile, report: CompatibilityReport) -> Self {
        Self {
            screen: Screen::Detection,
            profile,
            report,
        }
    }
}

fn run_event_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, mut app: App) -> Result<()> {
    // Build the per-screen view once per loop iteration. For slice
    // 10.2 only the detection view exists; future slices will route
    // through a match on `app.screen`.
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
            // Unreachable today — the while-condition filters Exiting
            // before we get here. Keep an explicit arm so the future
            // `Choices` and `PlanPreview` variants slot in cleanly.
            Screen::Exiting => break,
        }

        match next_event()? {
            Some(WizardEvent::Quit) => app.screen = Screen::Exiting,
            // Slice 10.3 will turn `Continue` into the transition to
            // the choice flow. For now it is intentionally a no-op so
            // the keybind is documented but harmless.
            Some(WizardEvent::Continue) => {}
            Some(WizardEvent::Refresh) => {
                // Resize / general redraw. The view is rebuilt from
                // the same data; nothing recomputes detection.
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
    fn arbitrary_keys_request_refresh_so_the_screen_stays_responsive() {
        // Pressing `j` or any other key the wizard hasn't claimed yet
        // shouldn't quit or advance. We map it to Refresh so screens
        // that scroll (slice 10.4's plan preview) can react later.
        assert_eq!(map_key(KeyCode::Char('j')), WizardEvent::Refresh);
        assert_eq!(map_key(KeyCode::Char(' ')), WizardEvent::Refresh);
    }
}
