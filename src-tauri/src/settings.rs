//! Tiny persisted app settings (app_data_dir/settings.json).
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default = "default_true")]
    pub pets_enabled: bool,
    /// Whether the tray "current session" monitor popover is enabled. When off,
    /// left-clicking the tray icon does nothing (and any open popover is hidden).
    #[serde(default = "default_true")]
    pub monitor_enabled: bool,
    /// Whether the session-end chime plays. When off, the session-end toast +
    /// taskbar flash still fire; only the sound is suppressed.
    #[serde(default = "default_true")]
    pub sound_enabled: bool,
    /// Session-end chime volume in `0.0..=1.0`. `1.0` plays the bundled wav at
    /// full level; lower values scale the PCM samples before playback.
    #[serde(default = "default_volume")]
    pub sound_volume: f64,
    /// Last on-screen position (physical px) the user dragged the popover to.
    /// `None` until first drag → popover anchors to the bottom-right corner.
    #[serde(default)]
    pub popover_x: Option<f64>,
    #[serde(default)]
    pub popover_y: Option<f64>,
    /// Whether to publish today's token total to Discord Rich Presence.
    /// Off by default; only takes effect when a `discord_client_id` is also set.
    #[serde(default)]
    pub discord_enabled: bool,
    /// Discord application (client) ID used for Rich Presence. There is no
    /// sensible default — supply your own app id from the Discord developer
    /// portal. When absent, the Discord integration stays off regardless of
    /// `discord_enabled`.
    #[serde(default)]
    pub discord_client_id: Option<String>,
}
fn default_true() -> bool {
    true
}
/// Default chime volume (80%): audible but not jarring.
fn default_volume() -> f64 {
    0.8
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            pets_enabled: true,
            monitor_enabled: true,
            sound_enabled: true,
            sound_volume: default_volume(),
            popover_x: None,
            popover_y: None,
            discord_enabled: false,
            discord_client_id: None,
        }
    }
}

fn settings_path() -> Option<PathBuf> {
    cmcore::paths::app_data_dir().ok().map(|d| d.join("settings.json"))
}
pub fn load() -> Settings {
    settings_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<Settings>(&s).ok())
        .unwrap_or_default()
}
pub fn save(s: &Settings) {
    if let Some(p) = settings_path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(s) {
            let _ = std::fs::write(p, json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_pets_disabled() {
        let s = Settings {
            pets_enabled: false,
            ..Settings::default()
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(
            json.contains("\"petsEnabled\":false") || json.contains("\"petsEnabled\": false"),
            "expected camelCase petsEnabled in {json}"
        );

        let back: Settings = serde_json::from_str(&json).unwrap();
        assert!(!back.pets_enabled);
    }

    #[test]
    fn missing_field_defaults_to_true() {
        let back: Settings = serde_json::from_str("{}").unwrap();
        assert!(back.pets_enabled);
        assert!(back.monitor_enabled);
        assert!(back.sound_enabled);
        assert_eq!(back.sound_volume, 0.8);
        assert_eq!(back.popover_x, None);
        assert_eq!(back.popover_y, None);
    }

    #[test]
    fn sound_settings_round_trip() {
        let s = Settings {
            sound_enabled: false,
            sound_volume: 0.35,
            ..Settings::default()
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(
            json.contains("\"soundEnabled\":false") || json.contains("\"soundEnabled\": false"),
            "expected camelCase soundEnabled in {json}"
        );
        assert!(
            json.contains("\"soundVolume\":0.35") || json.contains("\"soundVolume\": 0.35"),
            "expected camelCase soundVolume in {json}"
        );
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert!(!back.sound_enabled);
        assert_eq!(back.sound_volume, 0.35);
    }

    #[test]
    fn popover_position_round_trips() {
        let s = Settings {
            popover_x: Some(1280.0),
            popover_y: Some(40.0),
            ..Settings::default()
        };
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.popover_x, Some(1280.0));
        assert_eq!(back.popover_y, Some(40.0));
    }
}
