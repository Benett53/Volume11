//! Persisted configuration.
//!
//! The old version used EF Core + SQLite for what amounts to a few dozen key/value
//! pairs, which cost a native dependency and a noticeable chunk of startup time.
//! This is a plain JSON file, written atomically.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Bumped only when the on-disk shape changes incompatibly.
const CURRENT_VERSION: u32 = 2;

/// What happens to an application that starts playing and has no saved entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UnknownAppPolicy {
    /// Leave it alone. The old version silently forced every new app to 20 %,
    /// which was surprising; this is the sane default.
    #[default]
    LeaveAlone,
    /// Apply [`Settings::default_volume`].
    ApplyDefault,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppEntry {
    /// 0..=100.
    pub volume: u8,
    pub muted: bool,
    /// Label last seen for this executable, so the settings list stays readable
    /// even when the application is not running.
    #[serde(default)]
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Re-apply the saved level automatically when an application starts playing.
    pub auto_apply: bool,
    pub unknown_app_policy: UnknownAppPolicy,
    pub default_volume: u8,
    pub always_on_top: bool,
    pub start_with_windows: bool,
    /// Accent colour as `#rrggbb`.
    pub accent: String,
    /// Pure black background for OLED panels.
    pub oled_black: bool,
    /// Ask GitHub for a newer release at most once a day.
    pub check_updates: bool,
    /// Unix time of the last successful update check.
    pub last_update_check: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_apply: true,
            unknown_app_policy: UnknownAppPolicy::default(),
            default_volume: 50,
            always_on_top: false,
            start_with_windows: false,
            accent: "#9AA3AD".to_string(),
            oled_black: true,
            check_updates: true,
            last_update_check: 0,
        }
    }
}

/// Saved levels for one playback device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DeviceEntry {
    /// Name the device had when last seen, so the file stays readable and the
    /// entry can be recognised while the device is unplugged.
    pub label: String,
    /// Keyed by lower-cased executable name (`firefox.exe`) or `@master` / `@system`.
    pub apps: BTreeMap<String, AppEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    pub settings: Settings,
    /// One set of saved levels per playback device, keyed by endpoint id.
    ///
    /// A program is usually wanted at a different level on speakers than on a
    /// headset, and Windows itself keeps per-application volume per device, so
    /// storing one level per program across all devices would fight Windows
    /// every time the output changed. `BTreeMap` keeps the file diff-friendly.
    pub devices: BTreeMap<String, DeviceEntry>,
    /// Version 1 stored a single set of levels here. They belong to whichever
    /// device was in use back then, which is not known until the audio engine
    /// reports the current one, so they wait here until `adopt_legacy` moves
    /// them. Never written back once empty.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub apps: BTreeMap<String, AppEntry>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            settings: Settings::default(),
            devices: BTreeMap::new(),
            apps: BTreeMap::new(),
        }
    }
}

impl Config {
    /// Saved levels for one device, if it has any.
    pub fn device(&self, device: &str) -> Option<&DeviceEntry> {
        self.devices.get(device)
    }

    pub fn get(&self, device: &str, key: &str) -> Option<&AppEntry> {
        self.devices.get(device)?.apps.get(key)
    }

    pub fn set(
        &mut self,
        device: &str,
        device_label: &str,
        key: &str,
        volume: u8,
        muted: bool,
        label: &str,
    ) {
        let entry = self.devices.entry(device.to_string()).or_default();
        entry.label = device_label.to_string();
        entry.apps.insert(
            key.to_string(),
            AppEntry {
                volume: volume.min(100),
                muted,
                label: label.to_string(),
            },
        );
    }

    pub fn remove(&mut self, device: &str, key: &str) {
        if let Some(entry) = self.devices.get_mut(device) {
            entry.apps.remove(key);
            if entry.apps.is_empty() {
                self.devices.remove(device);
            }
        }
    }

    /// Everything saved for one device, in the shape `ApplyAll` expects.
    pub fn apply_list(&self, device: &str) -> Vec<(String, u8, bool)> {
        self.devices
            .get(device)
            .map(|entry| {
                entry
                    .apps
                    .iter()
                    .map(|(key, app)| (key.clone(), app.volume, app.muted))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Attach levels saved by version 1 to the device in use now.
    ///
    /// Version 1 kept one set of levels for whatever device was the default,
    /// so the default at the first start after upgrading is the best available
    /// guess. Entries already saved for that device win over legacy ones.
    /// Returns whether anything moved, so the caller knows to save.
    pub fn adopt_legacy(&mut self, device: &str, device_label: &str) -> bool {
        if self.apps.is_empty() || device.is_empty() {
            return false;
        }

        let legacy = std::mem::take(&mut self.apps);
        let entry = self.devices.entry(device.to_string()).or_default();

        if entry.label.is_empty() {
            entry.label = device_label.to_string();
        }
        for (key, app) in legacy {
            entry.apps.entry(key).or_insert(app);
        }

        self.version = CURRENT_VERSION;
        true
    }

    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };

        // Notepad and PowerShell's `Out-File -Encoding utf8` both prepend a byte
        // order mark, which serde_json rejects as a syntax error. Editing the
        // file by hand must not silently cost the user their settings.
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);

        match serde_json::from_str::<Config>(text) {
            Ok(config) => config,
            Err(error) => {
                // A corrupt file must not cost the user their whole setup silently,
                // so keep a copy next to it before starting over.
                let backup = path.with_extension("json.broken");
                let _ = std::fs::rename(path, &backup);
                eprintln!("config unreadable ({error}), moved to {}", backup.display());
                Self::default()
            }
        }
    }

    /// Write via a temporary file and rename, so a crash mid-write cannot leave
    /// a truncated configuration behind.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let text = serde_json::to_string_pretty(self)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;

        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, text)?;
        std::fs::rename(&temporary, path)?;

        Ok(())
    }
}

/// `%APPDATA%\Volume11\config.json`, unless `VOLUME11_CONFIG` names another file.
///
/// The override makes portable installs possible (keep the configuration next to
/// the executable on a stick) and lets tests run against a throwaway file.
///
/// The old version wrote to `%LOCALAPPDATA%\App1\`, a leftover placeholder name.
pub fn config_path() -> PathBuf {
    if let Ok(override_path) = std::env::var("VOLUME11_CONFIG")
        && !override_path.trim().is_empty()
    {
        return PathBuf::from(override_path);
    }

    let base = std::env::var("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));

    base.join("Volume11").join("config.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEAKERS: &str = "{0.0.0.00000000}.{speakers}";
    const HEADSET: &str = "{0.0.0.00000000}.{headset}";

    #[test]
    fn devices_keep_separate_levels() {
        let mut config = Config::default();
        config.set(SPEAKERS, "Speakers", "spotify.exe", 80, false, "Spotify");
        config.set(HEADSET, "Headset", "spotify.exe", 30, false, "Spotify");

        assert_eq!(config.get(SPEAKERS, "spotify.exe").unwrap().volume, 80);
        assert_eq!(config.get(HEADSET, "spotify.exe").unwrap().volume, 30);
        assert_eq!(
            config.apply_list(HEADSET),
            vec![("spotify.exe".into(), 30, false)]
        );
        assert!(config.apply_list("{unknown}").is_empty());
    }

    #[test]
    fn removing_the_last_entry_drops_the_device() {
        let mut config = Config::default();
        config.set(SPEAKERS, "Speakers", "vlc.exe", 30, false, "VLC");
        config.remove(SPEAKERS, "vlc.exe");
        assert!(config.device(SPEAKERS).is_none());
    }

    #[test]
    fn version_one_levels_move_to_the_current_device() {
        let v1 = r#"{"version":1,"settings":{},"apps":{
            "spotify.exe":{"volume":45,"muted":false,"label":"Spotify"}}}"#;
        let mut config: Config = serde_json::from_str(v1).unwrap();

        assert!(config.adopt_legacy(HEADSET, "Headset"));
        assert_eq!(config.get(HEADSET, "spotify.exe").unwrap().volume, 45);
        assert!(config.apps.is_empty());
        assert_eq!(config.version, CURRENT_VERSION);

        // Nothing left to move the second time.
        assert!(!config.adopt_legacy(SPEAKERS, "Speakers"));

        // The top-level legacy field is not written back once empty; `apps`
        // still exists inside each device entry, so check the structure.
        let value: serde_json::Value = serde_json::to_value(&config).unwrap();
        assert!(value.get("apps").is_none(), "stray legacy map: {value}");
    }

    #[test]
    fn legacy_levels_do_not_overwrite_newer_ones() {
        let mut config = Config::default();
        config.set(HEADSET, "Headset", "spotify.exe", 70, false, "Spotify");
        config.apps.insert(
            "spotify.exe".into(),
            AppEntry {
                volume: 10,
                muted: true,
                label: "Spotify".into(),
            },
        );

        config.adopt_legacy(HEADSET, "Headset");
        assert_eq!(config.get(HEADSET, "spotify.exe").unwrap().volume, 70);
    }

    #[test]
    fn round_trips_through_json() {
        let mut config = Config::default();
        config.set(SPEAKERS, "Speakers", "firefox.exe", 42, true, "Firefox");
        config.settings.always_on_top = true;

        let text = serde_json::to_string(&config).unwrap();
        let parsed: Config = serde_json::from_str(&text).unwrap();

        assert_eq!(parsed, config);
        assert_eq!(parsed.get(SPEAKERS, "firefox.exe").unwrap().volume, 42);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let parsed: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed.settings, Settings::default());
        assert!(parsed.apps.is_empty());
    }

    #[test]
    fn a_byte_order_mark_is_tolerated() {
        let dir = std::env::temp_dir().join(format!("volume11-bom-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");

        std::fs::write(
            &path,
            "\u{feff}{\"settings\":{\"default_volume\":35},\"apps\":{}}",
        )
        .unwrap();

        let loaded = Config::load(&path);
        assert_eq!(loaded.settings.default_volume, 35);
        // A tolerated file must not be moved aside as broken.
        assert!(!path.with_extension("json.broken").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_fields_do_not_break_loading() {
        let parsed: Config =
            serde_json::from_str(r#"{"version":1,"somethingNew":true,"apps":{}}"#).unwrap();
        assert_eq!(parsed.version, 1);
    }

    #[test]
    fn volume_is_clamped_on_write() {
        let mut config = Config::default();
        config.set(SPEAKERS, "Speakers", "x.exe", 250, false, "X");
        assert_eq!(config.get(SPEAKERS, "x.exe").unwrap().volume, 100);
    }

    #[test]
    fn save_then_load_matches() {
        let dir = std::env::temp_dir().join(format!("volume11-test-{}", std::process::id()));
        let path = dir.join("config.json");

        let mut config = Config::default();
        config.set(SPEAKERS, "Speakers", "vlc.exe", 30, false, "VLC");
        config.save(&path).unwrap();

        let loaded = Config::load(&path);
        assert_eq!(loaded, config);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
