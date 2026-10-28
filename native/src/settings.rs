use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Window appearance. `System` follows the Windows app mode.
#[derive(Serialize, Deserialize, Clone, Copy, Default, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub hotkey_enabled: bool,
    pub hotkey: String,
    pub capture_images: bool,
    pub remember_position: bool,
    /// Keep the window open when it loses focus (the pin button).
    pub keep_on_blur: bool,
    pub window_x: Option<i32>,
    pub window_y: Option<i32>,
    /// Auto-trim loose history over this many MB (0 = unlimited). Favorites/pinned/grouped excluded.
    pub max_storage_mb: u64,
    pub theme: Theme,
    /// Look for a new release once, shortly after start-up. Never downloads by itself.
    pub check_update_on_start: bool,
    /// Also offer a pasted picture as a named file, so web uploads don't all get "image.png".
    pub paste_image_as_file: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey_enabled: true,
            hotkey: "win+v".into(),
            capture_images: true,
            remember_position: false,
            keep_on_blur: false,
            window_x: None,
            window_y: None,
            max_storage_mb: 500,
            theme: Theme::System,
            check_update_on_start: true,
            paste_image_as_file: true,
        }
    }
}

impl Settings {
    pub fn max_storage_bytes(&self) -> i64 {
        self.max_storage_mb
            .saturating_mul(1024 * 1024)
            .min(i64::MAX as u64) as i64
    }

    /// Applies each field of `raw` that parses on its own, so one bad value only resets itself
    /// instead of the whole file. Returns whether every field was accepted.
    fn merge_tolerant(raw: &Value) -> (Self, bool) {
        let mut merged = serde_json::to_value(Self::default()).unwrap_or(Value::Null);
        let mut clean = true;
        if let (Some(target), Some(source)) = (merged.as_object_mut(), raw.as_object()) {
            for (key, value) in source {
                if !target.contains_key(key) {
                    continue; // unknown or removed setting
                }
                let previous = target.insert(key.clone(), value.clone());
                if serde_json::from_value::<Self>(Value::Object(target.clone())).is_err() {
                    clean = false;
                    match previous {
                        Some(p) => target.insert(key.clone(), p),
                        None => target.remove(key),
                    };
                }
            }
        } else {
            clean = false;
        }
        (serde_json::from_value(merged).unwrap_or_default(), clean)
    }
}

pub struct SettingsFile {
    path: PathBuf,
}

impl SettingsFile {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join("settings.json"),
        }
    }

    pub fn load(&self) -> Settings {
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return Settings::default();
        };
        let (settings, clean) = match serde_json::from_str::<Value>(&text) {
            Ok(raw) => Settings::merge_tolerant(&raw),
            Err(_) => (Settings::default(), false),
        };
        if !clean {
            // Keep the damaged file for inspection before it gets rewritten.
            let _ = std::fs::copy(&self.path, self.path.with_extension("json.bad"));
            if let Err(error) = self.save(&settings) {
                eprintln!("repairing settings failed: {error}");
            }
        }
        settings
    }

    pub fn save(&self, settings: &Settings) -> std::io::Result<()> {
        let json = serde_json::to_vec_pretty(settings).map_err(std::io::Error::other)?;
        crate::atomic_file::save(&self.path, &json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_bad_field_does_not_reset_the_rest() {
        let raw: Value = serde_json::json!({
            "hotkey": "ctrl+shift+v",
            "maxStorageMb": "not a number",
            "keepOnBlur": true,
            "syncKey": "leftover from an older version"
        });
        let (s, clean) = Settings::merge_tolerant(&raw);
        assert!(!clean);
        assert_eq!(s.hotkey, "ctrl+shift+v");
        assert!(s.keep_on_blur);
        assert_eq!(s.max_storage_mb, 500);
    }

    #[test]
    fn theme_defaults_to_system_and_rejects_unknown_values() {
        let (old_file, clean) = Settings::merge_tolerant(&serde_json::json!({ "hotkey": "win+v" }));
        assert!(clean);
        assert_eq!(old_file.theme, Theme::System);

        let (dark, _) = Settings::merge_tolerant(&serde_json::json!({ "theme": "dark" }));
        assert_eq!(dark.theme, Theme::Dark);

        let (bad, clean) = Settings::merge_tolerant(&serde_json::json!({ "theme": "sepia" }));
        assert!(!clean);
        assert_eq!(bad.theme, Theme::System);
    }

    #[test]
    fn valid_file_is_clean() {
        let raw = serde_json::to_value(Settings::default()).unwrap();
        assert!(Settings::merge_tolerant(&raw).1);
    }

    #[test]
    fn save_reports_an_unwritable_destination() {
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).unwrap();
        let missing = std::env::temp_dir().join(format!(
            "clipkeep-settings-missing-{}",
            u64::from_ne_bytes(random)
        ));
        assert!(
            SettingsFile::new(&missing)
                .save(&Settings::default())
                .is_err()
        );
    }
}
