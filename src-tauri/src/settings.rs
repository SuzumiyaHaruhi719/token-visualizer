//! Tiny persisted app settings (app_data_dir/settings.json).
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default = "default_true")]
    pub pets_enabled: bool,
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
impl Default for Settings {
    fn default() -> Self {
        Self {
            pets_enabled: true,
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
    }
}
