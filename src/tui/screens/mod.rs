//! Per-screen renderers for the wizard.
//!
//! Each screen owns its draw routine and any keybind handling specific
//! to it. The shared `App` state machine in `tui::mod` orchestrates
//! transitions between screens.
//!
//! Slice 10.2 ships only the detection screen. Slices 10.3 and 10.4
//! add the choice flow and the plan preview.

pub mod detection;
