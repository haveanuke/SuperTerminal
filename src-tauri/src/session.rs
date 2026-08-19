use serde_json::Value;
use std::path::{Path, PathBuf};
use tauri::State;

const DANGEROUS_KEYS: [&str; 3] = ["__proto__", "constructor", "prototype"];

fn has_no_dangerous_keys(v: &Value) -> bool {
    match v {
        Value::Array(items) => items.iter().all(has_no_dangerous_keys),
        Value::Object(map) => map
            .iter()
            .all(|(k, v)| !DANGEROUS_KEYS.contains(&k.as_str()) && has_no_dangerous_keys(v)),
        _ => true,
    }
}

fn validate_pane(v: &Value) -> bool {
    let Value::Object(map) = v else { return false };
    match map.get("type").and_then(Value::as_str) {
        Some("terminal") => map.get("terminalId").is_some_and(Value::is_string),
        Some("split") => {
            let dir_ok = matches!(
                map.get("direction").and_then(Value::as_str),
                Some("horizontal") | Some("vertical")
            );
            if !dir_ok {
                return false;
            }
            let children_ok = match map.get("children") {
                Some(Value::Array(children)) if children.len() == 2 => {
                    children.iter().all(validate_pane)
                }
                _ => false,
            };
            if !children_ok {
                return false;
            }
            match map.get("sizes") {
                None => true,
                Some(Value::Array(sizes)) if sizes.len() == 2 => {
                    sizes.iter().all(Value::is_number)
                }
                Some(_) => false,
            }
        }
        _ => false,
    }
}

fn validate_tab(v: &Value) -> bool {
    let Value::Object(map) = v else { return false };
    map.get("id").is_some_and(Value::is_string)
        && map.get("label").is_some_and(Value::is_string)
        && map.get("pane").is_some_and(validate_pane)
}

fn validate_layout(v: &Value) -> bool {
    let Value::Object(map) = v else { return false };
    if !map.get("activeTabId").is_some_and(Value::is_string) {
        return false;
    }
    match map.get("tabs") {
        Some(Value::Array(tabs)) => tabs.iter().all(validate_tab),
        _ => false,
    }
}

pub fn validate_session_data(v: &Value) -> bool {
    let Value::Object(map) = v else { return false };
    map.get("name").is_some_and(Value::is_string)
        && map.get("savedAt").is_some_and(Value::is_string)
        && map.get("layout").is_some_and(validate_layout)
        && has_no_dangerous_keys(v)
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// RFC3339 UTC timestamp (second precision) without a chrono dependency.
/// Civil-from-days per Howard Hinnant's algorithm.
fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let days = secs.div_euclid(86_400);
    let time = secs.rem_euclid(86_400);
    let (h, min, s) = (time / 3600, (time % 3600) / 60, time % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{min:02}:{s:02}Z")
}

pub struct SessionManager {
    dir: PathBuf,
}

impl SessionManager {
    pub fn new(dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        Self { dir }
    }

    fn file_for(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{}.json", sanitize_name(name)))
    }

    pub fn save(&self, name: &str, layout: &Value) -> Result<bool, String> {
        let data = serde_json::json!({
            "name": name,
            "layout": layout,
            "savedAt": now_rfc3339(),
        });
        let pretty = serde_json::to_string_pretty(&data).map_err(|e| e.to_string())?;
        std::fs::write(self.file_for(name), pretty).map_err(|e| e.to_string())?;
        Ok(true)
    }

    pub fn load(&self, name: &str) -> Result<Option<Value>, String> {
        let path = self.file_for(name);
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let parsed: Value = serde_json::from_str(&raw)
            .map_err(|_| format!("session \"{name}\" is corrupted (invalid JSON)"))?;
        if !validate_session_data(&parsed) {
            return Err(format!("session \"{name}\" is corrupted (schema mismatch)"));
        }
        Ok(Some(parsed))
    }

    pub fn list(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.strip_suffix(".json").map(String::from)
            })
            .collect()
    }

    pub fn delete(&self, name: &str) -> bool {
        let path = self.file_for(name);
        if path.exists() {
            std::fs::remove_file(&path).is_ok()
        } else {
            false
        }
    }

    /// One-time copy of sessions from the old Electron app-data dirs. Idempotent
    /// (marker file), never clobbers a session that already exists here. Multiple
    /// candidates because Electron's userData derived from package.json `name`
    /// (`super-terminal`); earlier candidates win for duplicate filenames.
    pub fn migrate_from(&self, old_dirs: &[&Path]) {
        let marker = self.dir.join(".migrated");
        if marker.exists() {
            return;
        }
        for old_dir in old_dirs {
            if let Ok(entries) = std::fs::read_dir(old_dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if !name.ends_with(".json") {
                        continue;
                    }
                    let dest = self.dir.join(&name);
                    if !dest.exists() {
                        let _ = std::fs::copy(entry.path(), dest);
                    }
                }
            }
        }
        let _ = std::fs::write(marker, "");
    }
}

#[tauri::command]
pub fn session_save(
    name: String,
    layout: Value,
    state: State<'_, SessionManager>,
) -> Result<bool, String> {
    state.save(&name, &layout)
}

#[tauri::command]
pub fn session_load(
    name: String,
    state: State<'_, SessionManager>,
) -> Result<Option<Value>, String> {
    state.load(&name)
}

#[tauri::command]
pub fn session_list(state: State<'_, SessionManager>) -> Vec<String> {
    state.list()
}

#[tauri::command]
pub fn session_delete(name: String, state: State<'_, SessionManager>) -> bool {
    state.delete(&name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU32, Ordering};

    static N: AtomicU32 = AtomicU32::new(0);
    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "st-session-test-{}-{}-{}",
            std::process::id(),
            tag,
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn valid_layout() -> Value {
        json!({
            "tabs": [{ "id": "t1", "label": "one",
                       "pane": { "type": "terminal", "terminalId": "term1" } }],
            "activeTabId": "t1"
        })
    }

    #[test]
    fn save_load_round_trip() {
        let mgr = SessionManager::new(tmp_dir("rt"));
        assert!(mgr.save("work", &valid_layout()).unwrap());
        let loaded = mgr.load("work").unwrap().unwrap();
        assert_eq!(loaded["name"], "work");
        assert_eq!(loaded["layout"]["activeTabId"], "t1");
        assert!(loaded["savedAt"].is_string());
        // sanity: the hand-rolled RFC3339 stamp looks like a date
        let stamp = loaded["savedAt"].as_str().unwrap();
        assert!(stamp.len() == 20 && stamp.ends_with('Z') && &stamp[4..5] == "-", "{stamp}");
    }

    #[test]
    fn sanitizes_names() {
        let mgr = SessionManager::new(tmp_dir("san"));
        mgr.save("../evil name!", &valid_layout()).unwrap();
        assert_eq!(mgr.list(), vec!["___evil_name_".to_string()]);
    }

    #[test]
    fn load_missing_returns_none() {
        let mgr = SessionManager::new(tmp_dir("miss"));
        assert!(mgr.load("nope").unwrap().is_none());
    }

    #[test]
    fn corrupt_json_is_invalid_json_error() {
        let dir = tmp_dir("corrupt");
        std::fs::write(dir.join("bad.json"), "{ not json").unwrap();
        let mgr = SessionManager::new(dir);
        let err = mgr.load("bad").unwrap_err();
        assert!(err.contains("invalid JSON"), "{err}");
    }

    #[test]
    fn schema_mismatch_error() {
        let dir = tmp_dir("schema");
        std::fs::write(
            dir.join("weird.json"),
            r#"{"name":"weird","savedAt":"x","layout":{"tabs":"no"}}"#,
        )
        .unwrap();
        let mgr = SessionManager::new(dir);
        let err = mgr.load("weird").unwrap_err();
        assert!(err.contains("schema mismatch"), "{err}");
    }

    #[test]
    fn rejects_prototype_pollution_keys() {
        let v = json!({
            "name": "x", "savedAt": "t",
            "layout": { "activeTabId": "a", "tabs": [],
                        "__proto__": { "polluted": true } }
        });
        assert!(!validate_session_data(&v));
    }

    #[test]
    fn validates_split_panes_recursively() {
        let v = json!({
            "name": "x", "savedAt": "t",
            "layout": { "activeTabId": "a", "tabs": [{
                "id": "t1", "label": "l",
                "pane": { "type": "split", "direction": "horizontal",
                          "sizes": [50, 50],
                          "children": [
                            { "type": "terminal", "terminalId": "a" },
                            { "type": "split", "direction": "diagonal",
                              "children": [
                                { "type": "terminal", "terminalId": "b" },
                                { "type": "terminal", "terminalId": "c" } ] } ] } }] }
        });
        assert!(!validate_session_data(&v)); // "diagonal" is invalid
    }

    #[test]
    fn accepts_nested_split_layout_round_trip() {
        let split = json!({
            "tabs": [{
                "id": "tab-1", "label": "Split",
                "pane": { "type": "split", "direction": "horizontal",
                          "sizes": [50, 50],
                          "children": [
                            { "type": "terminal", "terminalId": "t1" },
                            { "type": "terminal", "terminalId": "t2" } ] } }],
            "activeTabId": "tab-1"
        });
        let mgr = SessionManager::new(tmp_dir("split"));
        mgr.save("split", &split).unwrap();
        let loaded = mgr.load("split").unwrap().unwrap();
        assert_eq!(loaded["layout"], split);
    }

    #[test]
    fn list_and_delete() {
        let mgr = SessionManager::new(tmp_dir("ld"));
        mgr.save("a", &valid_layout()).unwrap();
        mgr.save("b", &valid_layout()).unwrap();
        let mut l = mgr.list();
        l.sort();
        assert_eq!(l, vec!["a".to_string(), "b".to_string()]);
        assert!(mgr.delete("a"));
        assert!(!mgr.delete("a"));
        assert_eq!(mgr.list(), vec!["b".to_string()]);
    }

    #[test]
    fn migration_copies_once_and_never_clobbers() {
        let old = tmp_dir("mig-old");
        let old2 = tmp_dir("mig-old2");
        let new = tmp_dir("mig-new");
        std::fs::write(old.join("legacy.json"), "{}").unwrap();
        std::fs::write(old.join("both.json"), "\"old\"").unwrap();
        std::fs::write(old2.join("second.json"), "{}").unwrap();
        std::fs::write(old2.join("both.json"), "\"old2\"").unwrap();
        std::fs::write(new.join("both.json"), "\"new\"").unwrap();
        let mgr = SessionManager::new(new.clone());
        mgr.migrate_from(&[old.as_path(), old2.as_path()]);
        // copied from both candidate dirs; existing files never clobbered
        assert!(new.join("legacy.json").exists());
        assert!(new.join("second.json").exists());
        assert_eq!(
            std::fs::read_to_string(new.join("both.json")).unwrap(),
            "\"new\""
        );
        assert!(new.join(".migrated").exists());
        // second run is a no-op even with new files in old dir
        std::fs::write(old.join("later.json"), "{}").unwrap();
        mgr.migrate_from(&[old.as_path()]);
        assert!(!new.join("later.json").exists());
    }
}
