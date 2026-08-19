pub mod shell_env;

use tauri::{AppHandle, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};

pub fn create_main_window(app: &AppHandle) -> tauri::Result<()> {
    let mut builder = WebviewWindowBuilder::new(app, "main", WebviewUrl::default())
        .title("SuperTerminal")
        .inner_size(1200.0, 800.0)
        .min_inner_size(600.0, 400.0)
        .background_color(tauri::window::Color(0x1a, 0x1b, 0x26, 0xff));
    #[cfg(target_os = "macos")]
    {
        builder = builder
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true)
            .traffic_light_position(tauri::LogicalPosition::new(12.0, 14.0));
    }
    builder.build()?;
    Ok(())
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            create_main_window(app.handle())?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| match event {
            RunEvent::Reopen { .. } => {
                if app.webview_windows().is_empty() {
                    let _ = create_main_window(app);
                }
            }
            _ => {}
        });
}
