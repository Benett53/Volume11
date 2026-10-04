//! Audio layer.
//!
//! All COM/WASAPI work happens on one dedicated thread owned by [`AudioHandle`].
//! The UI never touches COM: it sends [`Command`]s and receives [`Event`]s.
//!
//! Two design points carried over deliberately from reviewing the old WPF version:
//!
//! 1. Session objects are **cached** in a map keyed by process id. Moving a slider
//!    is an O(1) lookup plus a single COM call, not a full session re-enumeration.
//! 2. Changes are **event driven** (`IAudioSessionNotification`, `IAudioSessionEvents`,
//!    `IMMNotificationClient`). There is no polling timer, so switching the default
//!    playback device or changing a level elsewhere is reflected immediately.

mod callbacks;
mod engine;
mod policy;
pub mod process;

pub use engine::{AudioHandle, spawn};
pub use policy::set_default_device;

/// Identity of the master (endpoint) volume in configuration and commands.
pub const MASTER_KEY: &str = "@master";
/// Identity of the Windows system sounds session.
pub const SYSTEM_KEY: &str = "@system";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKind {
    Master,
    System,
    App,
}

impl SessionKind {
    /// Sort rank: master first, system sounds second, applications after.
    pub fn rank(self) -> u8 {
        match self {
            SessionKind::Master => 0,
            SessionKind::System => 1,
            SessionKind::App => 2,
        }
    }
}

/// An immutable view of one audio session, safe to send to the UI thread.
#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    /// Stable identity used as the configuration key (`firefox.exe`, `@master`, ...).
    pub key: String,
    /// What the user sees.
    pub label: String,
    pub pid: u32,
    /// 0..=100.
    pub volume: u8,
    pub muted: bool,
    pub kind: SessionKind,
    /// Full path of the backing executable, used to load the application icon.
    pub executable: Option<String>,
}

/// An active playback device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Endpoint id. Stable across restarts and reboots for the same hardware,
    /// which is what makes it usable as the key for per-device saved volumes.
    pub id: String,
    pub name: String,
}

/// Sent from the UI to the audio thread.
#[derive(Debug, Clone)]
pub enum Command {
    SetVolume {
        key: String,
        volume: u8,
    },
    SetMute {
        key: String,
        muted: bool,
    },
    /// Apply saved levels to everything currently playing ("sync").
    ApplyAll(Vec<(String, u8, bool)>),
    /// Re-enumerate sessions from scratch.
    Refresh,
    /// Make this endpoint the Windows default playback device.
    SetDefaultDevice(String),
    Shutdown,
}

/// Sent from the audio thread to the UI.
#[derive(Debug, Clone)]
pub enum Event {
    /// Full snapshot; sent whenever the set of sessions changes.
    Sessions(Vec<SessionSnapshot>),
    /// A single session changed, usually because something outside this app moved it.
    Changed {
        key: String,
        volume: u8,
        muted: bool,
    },
    /// The default playback device changed.
    DeviceChanged { id: String, name: String },
    /// The playback devices that can be switched to, and which one is current.
    /// Sent at startup and whenever a device is plugged in, removed or enabled.
    Devices {
        list: Vec<DeviceInfo>,
        current: String,
    },
    /// A session that was not present before appeared. Sent only for genuinely
    /// new sessions, so re-applying saved levels cannot clobber adjustments the
    /// user made by hand during this run.
    SessionAdded { key: String, label: String },
    /// Something went wrong that the application recovers from on its own, such
    /// as a failed device switch or no playback device being present. Shown to
    /// the user without replacing the mixer.
    Problem(String),
    /// The audio thread could not start; the UI shows this instead of silently failing.
    Fatal(String),
}

/// Convert a WASAPI scalar (0.0..=1.0) to the integer percentage used everywhere else.
pub fn scalar_to_percent(scalar: f32) -> u8 {
    (scalar.clamp(0.0, 1.0) * 100.0).round() as u8
}

/// Inverse of [`scalar_to_percent`].
pub fn percent_to_scalar(percent: u8) -> f32 {
    f32::from(percent.min(100)) / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_round_trips() {
        for percent in 0u8..=100 {
            assert_eq!(scalar_to_percent(percent_to_scalar(percent)), percent);
        }
    }

    #[test]
    fn scalars_are_clamped() {
        assert_eq!(scalar_to_percent(-1.0), 0);
        assert_eq!(scalar_to_percent(2.0), 100);
        assert_eq!(percent_to_scalar(200), 1.0);
    }

    #[test]
    fn master_sorts_before_system_and_apps() {
        assert!(SessionKind::Master.rank() < SessionKind::System.rank());
        assert!(SessionKind::System.rank() < SessionKind::App.rank());
    }
}
