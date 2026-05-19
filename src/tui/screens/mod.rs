//! Per-screen renderers for the wizard.
//!
//! Each screen owns its draw routine and any keybind handling specific
//! to it. The shared `App` state machine in `tui::mod` orchestrates
//! transitions between screens.
//!
//! Slice 10.2 ships only the detection screen. Slice 10.3 adds the
//! choice flow; slice 10.4 adds the plan preview.

pub mod choices;
pub mod detection;
pub mod preview;
