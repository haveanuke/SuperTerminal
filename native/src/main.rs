//! SuperTerminal Native: gpui + alacritty_terminal, no webview.

mod keys;
mod layout;
mod pane;
mod settings;
mod term_session;
mod text_field;
mod themes;
mod workspace;

use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    px, size, App, Application, Bounds, Focusable, KeyBinding, TitlebarOptions, WindowBounds,
    WindowOptions,
};

use workspace::{
    CloseFocused, NewTab, SaveSessionAs, SelectTab1, SelectTab2, SelectTab3, SelectTab4,
    SelectTab5, SelectTab6, SelectTab7, SelectTab8, SelectTab9, SplitDown, SplitRight,
    ToggleSessions, ToggleThemePicker, Workspace,
};

fn main() {
    // Process-global PTY env (TERM etc.) before any spawn; PTYs themselves
    // are always constructed on this (main) thread — contract rev 2 §4.
    alacritty_terminal::tty::setup_env();

    Application::new().run(|cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("cmd-t", NewTab, None),
            KeyBinding::new("cmd-w", CloseFocused, None),
            KeyBinding::new("cmd-d", SplitRight, None),
            KeyBinding::new("cmd-shift-d", SplitDown, None),
            KeyBinding::new("cmd-,", ToggleThemePicker, None),
            KeyBinding::new("cmd-o", ToggleSessions, None),
            KeyBinding::new("cmd-s", SaveSessionAs, None),
            KeyBinding::new("cmd-1", SelectTab1, None),
            KeyBinding::new("cmd-2", SelectTab2, None),
            KeyBinding::new("cmd-3", SelectTab3, None),
            KeyBinding::new("cmd-4", SelectTab4, None),
            KeyBinding::new("cmd-5", SelectTab5, None),
            KeyBinding::new("cmd-6", SelectTab6, None),
            KeyBinding::new("cmd-7", SelectTab7, None),
            KeyBinding::new("cmd-8", SelectTab8, None),
            KeyBinding::new("cmd-9", SelectTab9, None),
        ]);

        let bounds = Bounds::centered(None, size(px(1100.0), px(720.0)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("SuperTerminal".into()),
                        appears_transparent: true,
                        traffic_light_position: Some(gpui::point(px(12.0), px(12.0))),
                    }),
                    window_min_size: Some(size(px(400.0), px(300.0))),
                    ..Default::default()
                },
                |window, cx| {
                    let workspace = cx.new(Workspace::new);
                    workspace.read(cx).focus_handle(cx).focus(window);
                    workspace
                },
            )
            .expect("failed to open window");

        // Bounded shutdown (contract rev 2 §3): collect handles on the UI
        // thread, join them on ONE background thread with a deadline.
        let workspace_entity = window.entity(cx).ok();
        cx.on_window_closed(move |cx| {
            if let Some(workspace) = workspace_entity.clone() {
                let handles = workspace.update(cx, |ws, ws_cx| ws.shutdown_all(ws_cx));
                std::thread::spawn(move || {
                    for handle in handles {
                        handle.join_with_deadline(Duration::from_secs(3));
                    }
                });
            }
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        cx.activate(true);
    });
}
