use std::path::Path;
use tauri::{AppHandle, Manager};

const ALLOWED: [&str; 6] = ["png", "jpg", "jpeg", "webp", "gif", "bmp"];

pub fn allowed_ext(name: &str) -> Option<String> {
    let ext = Path::new(name).extension()?.to_str()?.to_ascii_lowercase();
    ALLOWED.contains(&ext.as_str()).then_some(ext)
}

/// Copy the picked image into <app-data>/background/bg.<ext> so the asset
/// protocol can serve it under a scope that never widens past app data.
#[tauri::command]
pub fn store_background_image(src: String, app: AppHandle) -> Result<String, String> {
    let ext = allowed_ext(&src).ok_or("unsupported image type")?;
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("background");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    let dest = dir.join(format!("bg.{ext}"));
    std::fs::copy(&src, &dest).map_err(|e| e.to_string())?;
    Ok(dest.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_known_image_extensions_case_insensitive() {
        assert_eq!(allowed_ext("photo.PNG"), Some("png".to_string()));
        assert_eq!(allowed_ext("a.jpeg"), Some("jpeg".to_string()));
        assert_eq!(allowed_ext("evil.app"), None);
        assert_eq!(allowed_ext("noext"), None);
    }
}
