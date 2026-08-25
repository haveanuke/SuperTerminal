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
    /// Prepend the bundled tool-adapter shims (claude/codex ring the bell
    /// with zero user setup) to PATH in newly spawned terminals.
    pub tool_adapters: bool,
    /// Phone-companion capability token (URL fragment auth). Generated on
    /// first server start; regenerating invalidates old bookmarks.
    pub companion_token: Option<String>,
    /// Hold the Mac awake automatically while any terminal runs a job.
    pub auto_caffeinate: bool,
    /// Speak buddy review notes aloud (macOS `say`).
    pub buddy_tts: bool,
    /// TTS voice name (None = system default), rate in words/min, and an
    /// approximate pitch multiplier (mapped onto say's pbas command).
    pub buddy_tts_voice: Option<String>,
    pub buddy_tts_rate: u32,
    pub buddy_tts_pitch: f32,
    /// Imported custom themes, in the old app's export JSON format.
    pub custom_themes: Vec<serde_json::Value>,
    /// Watched folder for the phone preview gallery; None = the default
    /// `$HOME/Pictures/SuperTerminal`. Absolute path, `~` never stored.
    pub preview_dir: Option<String>,
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
            tool_adapters: true,
            companion_token: None,
            auto_caffeinate: false,
            buddy_tts: false,
            buddy_tts_voice: None,
            buddy_tts_rate: 175,
            buddy_tts_pitch: 1.0,
            custom_themes: Vec::new(),
            preview_dir: None,
        }
    }
}

/// Package version, for the settings footer.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Short git hash ("+" appended when the tree was dirty), stamped into the
/// bundle's Info.plist by bundle.sh — the footer identity that actually
/// distinguishes one build from the next (the version alone cannot).
/// Bundle-time stamping keeps compile caching intact (a build.rs that
/// re-runs per build forces a crate recompile every build) and is exact
/// for the path that matters: every install flows through bundle.sh.
/// Outside a bundle (cargo run/test) this is honestly "dev".
pub fn build_hash() -> String {
    static CACHED: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CACHED
        .get_or_init(|| {
            let plist = std::env::current_exe().ok().and_then(|exe| {
                let contents = exe.parent()?.parent()?;
                std::fs::read_to_string(contents.join("Info.plist")).ok()
            });
            plist
                .as_deref()
                .and_then(parse_build_identity)
                .unwrap_or_else(|| "dev".into())
        })
        .clone()
}

/// Minimal plist scrape: the <string> following our identity key. The file
/// is our own bundle.sh output, so full plist parsing is not warranted.
fn parse_build_identity(plist: &str) -> Option<String> {
    let after_key = plist.split("<key>STBuildIdentity</key>").nth(1)?;
    let value = after_key
        .split("<string>")
        .nth(1)?
        .split("</string>")
        .next()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// The running binary's own modification time — read at runtime, so it can
/// never go stale the way a compile-time stamp can (a rebuild that reuses a
/// cached build script would otherwise show a time older than the binary).
pub fn build_time() -> String {
    static CACHED: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CACHED
        .get_or_init(|| {
            let epoch = std::env::current_exe()
                .and_then(|exe| exe.metadata())
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs());
            let Some(epoch) = epoch else {
                return String::new();
            };
            std::process::Command::new("/bin/date")
                .args(["-r", &epoch.to_string(), "+%m-%d %H:%M"])
                .output()
                .ok()
                .filter(|out| out.status.success())
                .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
                .unwrap_or_default()
        })
        .clone()
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

/// The watched preview folder: the setting verbatim, else
/// `$HOME/Pictures/SuperTerminal`. None only when HOME is unset.
pub fn resolved_preview_dir(settings: &Settings) -> Option<PathBuf> {
    if let Some(dir) = &settings.preview_dir {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Pictures/SuperTerminal"))
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
            tool_adapters: false,
            companion_token: Some("cafef00d".into()),
            auto_caffeinate: true,
            buddy_tts: true,
            buddy_tts_voice: Some("Samantha".into()),
            buddy_tts_rate: 200,
            buddy_tts_pitch: 1.2,
            custom_themes: Vec::new(),
            preview_dir: Some("/tmp/renders".into()),
        };
        s.save_to(&path).unwrap();
        assert_eq!(Settings::load_from(&path), s);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn written_json_includes_auto_caffeinate_off_by_default() {
        let path = tmp("autocaf").join("settings.json");
        Settings::default().save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"autoCaffeinate\": false"), "got: {text}");
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
    fn build_identity_is_always_present() {
        assert!(!APP_VERSION.is_empty());
        // Outside a .app bundle (this test binary) the hash is honestly
        // "dev"; inside, bundle.sh's Info.plist stamp takes over.
        assert_eq!(build_hash(), "dev");
        // Runtime binary mtime: present for any real executable (the test
        // binary included), formatted MM-dd HH:mm.
        let time = build_time();
        assert_eq!(time.len(), "08-24 19:54".len(), "{time:?}");
    }

    #[test]
    fn build_identity_parses_from_bundle_plist() {
        let plist = "<dict><key>CFBundleName</key><string>X</string>\
                     <key>STBuildIdentity</key>\n    <string>dbec417+</string></dict>";
        assert_eq!(parse_build_identity(plist), Some("dbec417+".into()));
        assert_eq!(parse_build_identity("<dict></dict>"), None);
        assert_eq!(
            parse_build_identity("<key>STBuildIdentity</key><string></string>"),
            None
        );
    }

    #[test]
    fn preview_dir_defaults_to_pictures_superterminal() {
        let s = Settings::default();
        assert_eq!(s.preview_dir, None);
        let resolved = resolved_preview_dir(&s).expect("HOME is set in tests");
        assert!(resolved.ends_with("Pictures/SuperTerminal"), "{resolved:?}");
    }

    #[test]
    fn preview_dir_setting_overrides_default() {
        let s = Settings {
            preview_dir: Some("/tmp/renders".into()),
            ..Settings::default()
        };
        assert_eq!(
            resolved_preview_dir(&s),
            Some(PathBuf::from("/tmp/renders"))
        );
    }

    #[test]
    fn settings_dir_is_the_app_support_dir() {
        assert!(
            settings_dir().ends_with(Path::new("Library/Application Support").join(APP_DIR_NAME))
        );
    }
}
