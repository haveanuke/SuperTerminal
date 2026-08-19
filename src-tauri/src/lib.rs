pub mod bg;
pub mod buddy;
pub mod git;
pub mod pty;
pub mod session;
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
        .manage(pty::PtyManager::default())
        .manage(git::GitState::default())
        .invoke_handler(tauri::generate_handler![
            git::git_resolve_repo,
            git::git_status,
            git::actions::git_stage,
            git::actions::git_stage_all,
            git::actions::git_unstage,
            git::actions::git_unstage_all,
            git::actions::git_discard,
            git::actions::git_commit,
            git::network::git_push,
            git::network::git_pull,
            git::network::git_fetch,
            git::graph::git_graph,
            pty::pty_create,
            pty::pty_write,
            pty::pty_write_broadcast,
            pty::pty_resize,
            pty::pty_dispose,
            pty::pty_cwd,
            session::session_save,
            session::session_load,
            session::session_list,
            session::session_delete,
            buddy::buddy_react,
            bg::store_background_image
        ])
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let sessions = session::SessionManager::new(data_dir.join("sessions"));
            #[cfg(target_os = "macos")]
            if let Ok(home) = app.path().home_dir() {
                // Electron's userData was `super-terminal` (package.json name);
                // `SuperTerminal` covers any productName-based variant.
                let support = home.join("Library/Application Support");
                sessions.migrate_from(&[
                    support.join("super-terminal/sessions").as_path(),
                    support.join("SuperTerminal/sessions").as_path(),
                ]);
            }
            app.manage(sessions);
            create_main_window(app.handle())?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                let app = window.app_handle();
                if app.webview_windows().is_empty() {
                    app.state::<pty::PtyManager>().dispose_all();
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| match event {
            // Closing the last window emits an automatic ExitRequested (code None).
            // Prevent it so the app stays in the dock, macOS-style; an explicit
            // Quit carries a code and exits normally.
            RunEvent::ExitRequested { code, api, .. } => {
                if code.is_none() {
                    api.prevent_exit();
                }
            }
            RunEvent::Reopen { .. } => {
                if app.webview_windows().is_empty() {
                    let _ = create_main_window(app);
                }
            }
            RunEvent::Exit => {
                app.state::<pty::PtyManager>().dispose_all();
            }
            _ => {}
        });
}
