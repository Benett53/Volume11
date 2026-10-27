//! Volume11 — per-application volume management for Windows.
//!
//! The binary is a thin shell around this library, which also lets the examples
//! and tests exercise the audio layer without duplicating module trees.

pub mod audio;
pub mod autostart;
pub mod config;
pub mod instance;
pub mod tray;
pub mod ui;
pub mod update;
