//! Volume11 — per-application volume management for Windows and Linux.
//!
//! The binary is a thin shell around this library, which also lets the examples
//! and tests exercise the audio layer without duplicating module trees.

//!
//! Platform specific modules keep one public surface: the Linux files live
//! next to the Windows ones with a `_linux` suffix and are selected here.

pub mod audio;
#[cfg_attr(target_os = "linux", path = "autostart_linux.rs")]
pub mod autostart;
pub mod config;
#[cfg_attr(target_os = "linux", path = "instance_linux.rs")]
pub mod instance;
#[cfg(target_os = "linux")]
pub mod pngutil;
#[cfg_attr(target_os = "linux", path = "tray_linux.rs")]
pub mod tray;
pub mod ui;
pub mod update;
