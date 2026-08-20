//! App settings: `settings.json` in the native app's own support directory.
//! Sessions are intentionally NOT here — they live in the directory shared
//! with the Tauri app (see `workspace::sessions_dir`).

use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

pub const APP_DIR_NAME: &str = "com.tomaspinal.superterminal.native";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub theme: String,
    pub font_size: f32,
    pub font_family: String,
    pub background_image: Option<String>,
    pub background_opacity: f32,
    pub buddy_enabled: bool,
    pub buddy_command: String,
    /// `{prompt}` is substituted with the review prompt.
    pub buddy_args: Vec<String>,
    /// The pet's persisted identity (bones regenerate from its user id).
    pub buddy_companion: Option<crate::buddy_pet::CompanionSave>,
    pub buddy_pet_visible: bool,
    /// Last dragged pet position (window coordinates), clamped on render.
    pub buddy_pet_pos: Option<(f32, f32)>,
    /// Audio cue when a terminal finishes working / awaits input.
    pub audio_cues: bool,
    /// Speak buddy review notes aloud (macOS `say`).
    pub buddy_tts: bool,
    /// Imported custom themes, in the old app's export JSON format.
    pub custom_themes: Vec<serde_json::Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "Tokyo Night".to_string(),
            // Contract defaults; rendering falls back through Menlo/Monaco
            // when JetBrains Mono is not installed (pane font fallbacks).
            font_size: 14.0,
            font_family: "JetBrains Mono".to_string(),
            background_image: None,
            background_opacity: 0.3,
            buddy_enabled: false,
            buddy_command: String::new(),
            buddy_args: vec!["-p".to_string(), "{prompt}".to_string()],
            buddy_companion: None,
            buddy_pet_visible: true,
            buddy_pet_pos: None,
            audio_cues: true,
            buddy_tts: false,
            custom_themes: Vec::new(),
        }
    }
}

pub fn settings_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Library/Application Support")
        .join(APP_DIR_NAME)
}

pub fn settings_path() -> PathBuf {
    settings_dir().join("settings.json")
}

impl Settings {
    pub fn load() -> Settings {
        Self::load_from(&settings_path())
    }

    /// Missing or corrupt files load defaults; partial files fill defaults.
    pub fn load_from(path: &Path) -> Settings {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Settings::default(),
        }
    }

    pub fn save(&self) -> io::Result<()> {
        self.save_to(&settings_path())
    }

    /// Atomic write (tmp + rename), same discipline as session files.
    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "st-native-settings-{}-{}",
            std::process::id(),
            name
        ))
    }

    #[test]
    fn defaults_match_contract() {
        let s = Settings::default();
        assert_eq!(s.theme, "Tokyo Night");
        assert_eq!(s.font_size, 14.0);
        assert_eq!(s.font_family, "JetBrains Mono");
    }

    #[test]
    fn missing_file_loads_defaults() {
        assert_eq!(
            Settings::load_from(&tmp("nope").join("settings.json")),
            Settings::default()
        );
    }

    #[test]
    fn round_trip_preserves_values() {
        let path = tmp("round").join("settings.json");
        let s = Settings {
            theme: "Nord".into(),
            font_size: 16.0,
            font_family: "Menlo".into(),
            background_image: Some("/tmp/bg.png".into()),
            background_opacity: 0.5,
            buddy_enabled: true,
            buddy_command: "claude".into(),
            buddy_args: vec!["-p".into(), "{prompt}".into()],
            buddy_companion: Some(crate::buddy_pet::CompanionSave {
                user_id: "roundtrip".into(),
                name: "Pixel".into(),
                pet_count: 7,
                hatched_at: 1,
            }),
            buddy_pet_visible: false,
            buddy_pet_pos: Some((120.0, 240.0)),
            audio_cues: false,
            buddy_tts: true,
            custom_themes: Vec::new(),
        };
        s.save_to(&path).unwrap();
        assert_eq!(Settings::load_from(&path), s);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn written_json_uses_camel_case_keys() {
        let path = tmp("camel").join("settings.json");
        Settings::default().save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"fontSize\""), "got: {text}");
        assert!(text.contains("\"fontFamily\""), "got: {text}");
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn partial_and_corrupt_files_fall_back() {
        let dir = tmp("partial");
        std::fs::create_dir_all(&dir).unwrap();
        let partial = dir.join("partial.json");
        std::fs::write(&partial, r#"{ "theme": "Dracula" }"#).unwrap();
        let s = Settings::load_from(&partial);
        assert_eq!(s.theme, "Dracula");
        assert_eq!(s.font_size, 14.0);
        let corrupt = dir.join("corrupt.json");
        std::fs::write(&corrupt, "{ not json").unwrap();
        assert_eq!(Settings::load_from(&corrupt), Settings::default());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn settings_dir_is_the_app_support_dir() {
        assert!(
            settings_dir().ends_with(Path::new("Library/Application Support").join(APP_DIR_NAME))
        );
    }
}
