//! SuperTerminal Native: gpui + alacritty_terminal, no webview.

mod buddy_pet;
mod file_viewer;
mod files_panel;
mod git_panel;
mod icons;
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
    px, size, App, Application, Bounds, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions,
};

use workspace::{
    CloseFocused, CloseTab, NewTab, NewWindow, SaveSessionAs, SelectTab1, SelectTab2, SelectTab3,
    SelectTab4, SelectTab5, SelectTab6, SelectTab7, SelectTab8, SelectTab9, SplitDown, SplitRight,
    ToggleGitPanel, ToggleSearch, ToggleSessions, ToggleThemePicker, Workspace,
};

fn main() {
    // When the app is launched FROM a terminal session (dev installs run
    // `open` from inside one), agent-CLI session markers leak into our
    // process env — and from there into every shell this terminal spawns,
    // making tools like `claude` believe they're nested child sessions
    // (e.g. "transcript saving is off"). Scrub them before anything runs;
    // safe here: no other threads exist yet.
    for var in [
        "CLAUDECODE",
        "CLAUDE_CODE_CHILD_SESSION",
        "CLAUDE_CODE_ENTRYPOINT",
        "CLAUDE_CODE_SSE_PORT",
    ] {
        std::env::remove_var(var);
    }

    // Process-global PTY env (TERM etc.) before any spawn; PTYs themselves
    // are always constructed on this (main) thread — contract rev 2 §4.
    alacritty_terminal::tty::setup_env();

    Application::new().run(|cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("cmd-t", NewTab, None),
            KeyBinding::new("cmd-n", NewWindow, None),
            KeyBinding::new("cmd-w", CloseFocused, None),
            KeyBinding::new("cmd-shift-w", CloseTab, None),
            KeyBinding::new("cmd-d", SplitRight, None),
            KeyBinding::new("cmd-shift-d", SplitDown, None),
            KeyBinding::new("cmd-,", ToggleThemePicker, None),
            KeyBinding::new("cmd-o", ToggleSessions, None),
            KeyBinding::new("cmd-s", SaveSessionAs, None),
            KeyBinding::new("cmd-f", ToggleSearch, None),
            KeyBinding::new("cmd-shift-g", ToggleGitPanel, None),
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
                        traffic_light_position: Some(gpui::point(px(12.0), px(11.0))),
                    }),
                    window_min_size: Some(size(px(400.0), px(300.0))),
                    ..Default::default()
                },
                |window, cx| {
                    let workspace = cx.new(Workspace::new);
                    workspace.update(cx, |ws, ws_cx| ws.focus_active_pane(window, ws_cx));
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
                    // ONE global deadline across every pane, not 3s each.
                    let deadline = std::time::Instant::now() + Duration::from_secs(3);
                    for handle in handles {
                        let remaining =
                            deadline.saturating_duration_since(std::time::Instant::now());
                        // Past-deadline handles still get the SIGKILL+join
                        // fast path inside join_with_deadline(0).
                        handle.join_with_deadline(remaining);
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
