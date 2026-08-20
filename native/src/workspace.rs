//! Root view: tabs of split-pane terminals, the tmux-style bottom bar, and
//! the theme/sessions overlays.
//!
//! Structure follows the contract: the serializable pane tree is the
//! `layout::PaneNode` DTO (String terminal ids); live gpui entities live in a
//! side map keyed by those ids.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    div, px, rgb, App, Context, Entity, FocusHandle, Focusable, MouseButton, MouseMoveEvent,
    MouseUpEvent, Pixels, SharedString, Window,
};

use superterminal_core::session::SessionManager;

use crate::git_panel::GitPanel;
use crate::layout::{
    collect_terminal_ids, insert_split, remove_terminal, Layout, PaneNode, SplitDirection, Tab,
};
use crate::pane::{BroadcastHub, PaneEvent, TerminalPane};
use crate::settings::Settings;
use crate::term_session::ShutdownHandle;
use crate::text_field::{TextField, TextFieldEvent};
use crate::themes::{self, Theme};

gpui::actions!(
    superterminal,
    [
        NewTab,
        CloseFocused,
        CloseTab,
        SplitRight,
        SplitDown,
        ToggleThemePicker,
        ToggleSessions,
        SaveSessionAs,
        ToggleSearch,
        ToggleGitPanel,
        SelectTab1,
        SelectTab2,
        SelectTab3,
        SelectTab4,
        SelectTab5,
        SelectTab6,
        SelectTab7,
        SelectTab8,
        SelectTab9
    ]
);

/// Split-container bounds captured by measuring canvases: (x, y, w, h).
type SplitBoundsMap = HashMap<String, (Pixels, Pixels, Pixels, Pixels)>;

/// Sessions live in the directory SHARED with the Tauri app (contract rev 2
/// §6) — both apps read and write the same session files.
pub fn sessions_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join("Library/Application Support/com.tomaspinal.superterminal/sessions")
}

#[derive(Clone, Copy, PartialEq)]
enum Overlay {
    None,
    ThemePicker,
    Sessions,
    AutoRun,
    Search,
}

struct DragState {
    tab_index: usize,
    /// Path of child indices from the tab root to the split being resized.
    path: Vec<usize>,
    direction: SplitDirection,
}

pub struct Workspace {
    tabs: Vec<Tab>,
    active_tab: usize,
    panes: HashMap<String, Entity<TerminalPane>>,
    focused_terminal: Option<String>,
    settings: Settings,
    theme: &'static Theme,
    overlay: Overlay,
    session_manager: SessionManager,
    session_names: Vec<String>,
    session_field: Option<Entity<TextField>>,
    rename_field: Option<(usize, Entity<TextField>)>,
    auto_run_field: Option<Entity<TextField>>,
    search_field: Option<Entity<TextField>>,
    buddy_field: Option<Entity<TextField>>,
    auto_run_interval: u32,
    auto_run_escape: bool,
    auto_run_escape_delay: u32,
    focus_handle: FocusHandle,
    drag: Option<DragState>,
    /// Split-container bounds (window coords) written by measuring canvases,
    /// keyed by "tab_index:path" — used for divider drag math.
    split_bounds: Arc<Mutex<SplitBoundsMap>>,
    next_id: u64,
    /// Shutdown handles for panes being torn down; joined on app quit.
    pending_shutdowns: Arc<Mutex<Vec<ShutdownHandle>>>,
    broadcast: Arc<BroadcastHub>,
    git_panel: Option<Entity<GitPanel>>,
    /// Transient status line for the theme sheet (import/export results).
    theme_action_note: Option<String>,
    /// Buddy reviewer: latest note, in-flight flag, last reviewed content hash.
    buddy_note: Option<String>,
    buddy_busy: Arc<std::sync::atomic::AtomicBool>,
    buddy_last_hash: u64,
    /// After a failed run, hold off retries until this instant so a broken
    /// command doesn't respawn every tick.
    buddy_backoff_until: Option<std::time::Instant>,
    /// Swap mode: the pane waiting to trade places, if any.
    swap_source: Option<String>,
}

impl Workspace {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let settings = Settings::load();
        for custom in &settings.custom_themes {
            let _ = themes::import_custom(custom);
        }
        let theme = themes::by_name(&settings.theme).unwrap_or_else(themes::default_theme);
        let mut this = Self {
            tabs: Vec::new(),
            active_tab: 0,
            panes: HashMap::new(),
            focused_terminal: None,
            settings,
            theme,
            overlay: Overlay::None,
            session_manager: SessionManager::new(sessions_dir()),
            session_names: Vec::new(),
            session_field: None,
            rename_field: None,
            auto_run_field: None,
            search_field: None,
            buddy_field: None,
            auto_run_interval: 5,
            auto_run_escape: false,
            auto_run_escape_delay: 2,
            focus_handle: cx.focus_handle(),
            drag: None,
            split_bounds: Arc::new(Mutex::new(HashMap::new())),
            next_id: 1,
            pending_shutdowns: Arc::new(Mutex::new(Vec::new())),
            broadcast: Arc::new(BroadcastHub::default()),
            git_panel: None,
            theme_action_note: None,
            buddy_note: None,
            buddy_busy: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            buddy_last_hash: 0,
            buddy_backoff_until: None,
            swap_source: None,
        };
        this.add_tab(None, cx);
        cx.spawn(async move |ws, cx| loop {
            cx.background_executor().timer(Duration::from_secs(4)).await;
            if ws
                .update(cx, |ws: &mut Workspace, cx| ws.buddy_tick(cx))
                .is_err()
            {
                break;
            }
        })
        .detach();
        this
    }

    /// Buddy-as-reviewer: after a burst of terminal activity settles, send
    /// the visible output plus the repo's working-tree diff to the configured
    /// agent CLI and surface its one-line note in the bar.
    fn buddy_tick(&mut self, cx: &mut Context<Self>) {
        use std::hash::{Hash, Hasher};
        if !self.settings.buddy_enabled
            || self.settings.buddy_command.trim().is_empty()
            || self.buddy_busy.load(std::sync::atomic::Ordering::Relaxed)
        {
            return;
        }
        let Some(pane) = self
            .focused_terminal
            .as_ref()
            .and_then(|id| self.panes.get(id))
        else {
            return;
        };
        let pane_ref = pane.read(cx);
        let quiet = pane_ref.last_activity.elapsed() >= Duration::from_secs(3);
        if !quiet {
            return;
        }
        let text = pane_ref.visible_text();
        if text.trim().len() < 80 {
            return;
        }
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut hasher);
        let hash = hasher.finish();
        if hash == self.buddy_last_hash {
            return;
        }
        if self
            .buddy_backoff_until
            .is_some_and(|until| std::time::Instant::now() < until)
        {
            return;
        }
        let cwd = pane_ref.cwd();
        let command = self.settings.buddy_command.clone();
        let args = self.settings.buddy_args.clone();
        let busy = Arc::clone(&self.buddy_busy);
        busy.store(true, std::sync::atomic::Ordering::Relaxed);
        cx.spawn(async move |ws, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    // Working-tree diff of the enclosing repo, capped.
                    let diff = cwd
                        .and_then(|cwd| {
                            superterminal_core::git::process::run_git(
                                Some(std::path::Path::new(&cwd)),
                                &["diff", "--stat", "--patch", "--no-color"],
                                false,
                            )
                            .ok()
                        })
                        .map(|out| {
                            String::from_utf8_lossy(&out.stdout)
                                .chars()
                                .take(6000)
                                .collect::<String>()
                        })
                        .unwrap_or_default();
                    let tail: String = text.chars().rev().take(2000).collect::<String>()
                        .chars().rev().collect();
                    let prompt = format!(
                        "You are a terse code reviewer embedded in a terminal. Given recent terminal output and the current working-tree git diff, reply with ONE short actionable observation (a bug, risk, or next step). No preamble.\n\nTERMINAL OUTPUT:\n{tail}\n\nWORKING DIFF:\n{diff}"
                    );
                    superterminal_core::buddy::run(superterminal_core::buddy::BuddyRequest {
                        command,
                        args,
                        prompt,
                        timeout_ms: Some(30_000),
                    })
                })
                .await;
            let _ = ws.update(cx, |ws: &mut Workspace, cx| {
                ws.buddy_busy
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                if result.ok {
                    // Only a successful review consumes the content hash: a
                    // timeout or launch failure retries once the backoff ends.
                    ws.buddy_last_hash = hash;
                    ws.buddy_backoff_until = None;
                    ws.buddy_note = Some(result.text);
                    cx.notify();
                } else {
                    ws.buddy_backoff_until =
                        Some(std::time::Instant::now() + Duration::from_secs(30));
                }
            });
            Ok::<(), ()>(())
        })
        .detach();
    }

    /// Collect shutdown handles for every live pane plus any pending ones.
    /// The caller joins them OFF the UI thread with a bounded deadline.
    pub fn shutdown_all(&mut self, cx: &mut Context<Self>) -> Vec<ShutdownHandle> {
        let mut handles: Vec<ShutdownHandle> =
            self.pending_shutdowns.lock().unwrap().drain(..).collect();
        for pane in self.panes.values() {
            if let Some(handle) = pane.update(cx, |pane, _| pane.shutdown()) {
                handles.push(handle);
            }
        }
        self.panes.clear();
        handles
    }

    fn fresh_id(&mut self) -> String {
        let id = format!("term-{}", self.next_id);
        self.next_id += 1;
        id
    }

    fn spawn_pane(
        &mut self,
        id: String,
        cwd: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Entity<TerminalPane> {
        let theme = self.theme;
        let family = self.settings.font_family.clone();
        let size = self.settings.font_size;
        let translucent = self.settings.background_image.is_some();
        let pane_id = id.clone();
        let hub = Arc::clone(&self.broadcast);
        let pane =
            cx.new(|pane_cx| TerminalPane::new(pane_id, cwd, theme, family, size, hub, pane_cx));
        pane.update(cx, |pane, pane_cx| {
            pane.set_appearance(
                theme,
                &self.settings.font_family,
                size,
                translucent,
                pane_cx,
            )
        });
        cx.subscribe(&pane, Self::on_pane_event).detach();
        self.panes.insert(id, pane.clone());
        pane
    }

    fn on_pane_event(
        &mut self,
        pane: Entity<TerminalPane>,
        event: &PaneEvent,
        cx: &mut Context<Self>,
    ) {
        let pane_id = pane.read(cx).id.clone();
        match event {
            PaneEvent::Focused => {
                if let Some(source) = self.swap_source.take() {
                    if source != pane_id {
                        for tab in &mut self.tabs {
                            tab.pane = crate::layout::swap_terminals(&tab.pane, &source, &pane_id);
                        }
                    }
                }
                self.focused_terminal = Some(pane_id);
                self.push_git_cwd(cx);
                cx.notify();
            }
            PaneEvent::TitleChanged => cx.notify(),
            PaneEvent::Exited => {
                self.close_terminal(&pane_id, cx);
            }
        }
    }

    /// Route keyboard focus to the currently-focused terminal's pane.
    pub fn focus_active_pane(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pane) = self
            .focused_terminal
            .as_ref()
            .and_then(|id| self.panes.get(id))
        {
            pane.read(cx).focus(window);
        }
    }

    fn add_tab(&mut self, cwd: Option<PathBuf>, cx: &mut Context<Self>) {
        let terminal_id = self.fresh_id();
        self.spawn_pane(terminal_id.clone(), cwd, cx);
        let tab_id = format!("tab-{}", self.next_id);
        self.next_id += 1;
        self.tabs.push(Tab {
            id: tab_id,
            label: "terminal".to_string(),
            pane: PaneNode::terminal(&terminal_id),
        });
        self.active_tab = self.tabs.len() - 1;
        self.focused_terminal = Some(terminal_id);
        cx.notify();
    }

    fn split_focused(&mut self, direction: SplitDirection, cx: &mut Context<Self>) {
        let Some(target) = self.focused_terminal.clone() else {
            return;
        };
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        if !collect_terminal_ids(&tab.pane).contains(&target) {
            return;
        }
        // Split inherits the source pane's live working directory.
        let cwd = self
            .panes
            .get(&target)
            .and_then(|p| p.read(cx).cwd())
            .map(PathBuf::from);
        let new_id = self.fresh_id();
        self.spawn_pane(new_id.clone(), cwd, cx);
        let tab = &mut self.tabs[self.active_tab];
        tab.pane = insert_split(&tab.pane, &target, direction, &new_id);
        self.focused_terminal = Some(new_id);
        cx.notify();
    }

    /// Keep an in-progress tab rename pointing at the same tab after a tab
    /// removal shifts the indices; drop it if its own tab was closed.
    fn fix_rename_after_removal(&mut self, removed: usize) {
        if let Some((rename_index, field)) = self.rename_field.take() {
            if rename_index != removed {
                let adjusted = rename_index - usize::from(rename_index > removed);
                self.rename_field = Some((adjusted, field));
            }
        }
    }

    fn close_terminal(&mut self, terminal_id: &str, cx: &mut Context<Self>) {
        if let Some(pane) = self.panes.remove(terminal_id) {
            pane.update(cx, move |pane, _| {
                if let Some(handle) = pane.shutdown() {
                    // Reap immediately on a detached thread (bounded inside);
                    // resources release now, not at app quit.
                    std::thread::spawn(move || {
                        handle.join_with_deadline(std::time::Duration::from_secs(3))
                    });
                }
            });
        }
        let Some(tab_index) = self
            .tabs
            .iter()
            .position(|t| collect_terminal_ids(&t.pane).contains(&terminal_id.to_string()))
        else {
            return;
        };
        match remove_terminal(&self.tabs[tab_index].pane, terminal_id) {
            Some(rest) => {
                self.tabs[tab_index].pane = rest;
                if self.focused_terminal.as_deref() == Some(terminal_id) {
                    self.focused_terminal = collect_terminal_ids(&self.tabs[tab_index].pane)
                        .into_iter()
                        .next();
                }
            }
            None => {
                let was_active = tab_index == self.active_tab
                    || self.focused_terminal.as_deref() == Some(terminal_id);
                self.tabs.remove(tab_index);
                self.fix_rename_after_removal(tab_index);
                if self.tabs.is_empty() {
                    self.add_tab(None, cx);
                } else {
                    // Removing a tab before the active one shifts every later
                    // index down; follow the shift so the same tab stays
                    // selected. Only an active-tab close moves focus.
                    if tab_index < self.active_tab {
                        self.active_tab -= 1;
                    }
                    self.active_tab = self.active_tab.min(self.tabs.len() - 1);
                    if was_active {
                        self.focused_terminal =
                            collect_terminal_ids(&self.tabs[self.active_tab].pane)
                                .into_iter()
                                .next();
                    }
                }
            }
        }
        cx.notify();
    }

    /// Close a whole tab: every terminal in its tree shuts down (the old
    /// app's removeTab). The last remaining tab is respawned fresh.
    fn close_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let ids = collect_terminal_ids(&tab.pane);
        let was_active = index == self.active_tab
            || self
                .focused_terminal
                .as_ref()
                .is_some_and(|focused| ids.contains(focused));
        for id in ids {
            if let Some(pane) = self.panes.remove(&id) {
                pane.update(cx, move |pane, _| {
                    if let Some(handle) = pane.shutdown() {
                        std::thread::spawn(move || {
                            handle.join_with_deadline(std::time::Duration::from_secs(3))
                        });
                    }
                });
            }
        }
        self.tabs.remove(index);
        self.fix_rename_after_removal(index);
        if self.tabs.is_empty() {
            self.add_tab(None, cx);
        } else {
            if index < self.active_tab {
                self.active_tab -= 1;
            }
            self.active_tab = self.active_tab.min(self.tabs.len() - 1);
            if was_active {
                self.focused_terminal = collect_terminal_ids(&self.tabs[self.active_tab].pane)
                    .into_iter()
                    .next();
            }
        }
        cx.notify();
    }

    fn close_focused(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.focused_terminal.clone() {
            self.close_terminal(&id, cx);
        }
    }

    fn start_tab_rename(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let theme = self.theme;
        let field = cx.new(|field_cx| TextField::new("tab name", theme, field_cx).compact());
        cx.subscribe(
            &field,
            move |ws, _field, event: &TextFieldEvent, cx| match event {
                TextFieldEvent::Submitted(name) => {
                    if let Some((idx, _)) = ws.rename_field.take() {
                        if let Some(tab) = ws.tabs.get_mut(idx) {
                            let trimmed = name.trim();
                            if !trimmed.is_empty() {
                                tab.label = trimmed.to_string();
                            }
                        }
                    }
                    cx.notify();
                }
                TextFieldEvent::Cancelled => {
                    ws.rename_field = None;
                    cx.notify();
                }
            },
        )
        .detach();
        let current = self
            .tabs
            .get(index)
            .map(|tab| tab.label.clone())
            .unwrap_or_default();
        field.update(cx, |field, field_cx| {
            field.set_text_selected(&current, field_cx)
        });
        field.read(cx).focus(window);
        self.rename_field = Some((index, field));
        cx.notify();
    }

    fn select_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.tabs.len() {
            self.active_tab = index;
            self.focused_terminal = collect_terminal_ids(&self.tabs[index].pane)
                .into_iter()
                .next();
            cx.notify();
        }
    }

    /// Push the current settings to every pane and persist them.
    fn apply_appearance(&mut self, cx: &mut Context<Self>) {
        let _ = self.settings.save();
        let theme = self.theme;
        let family = self.settings.font_family.clone();
        let size = self.settings.font_size;
        let translucent = self.settings.background_image.is_some();
        for pane in self.panes.values() {
            pane.update(cx, |pane, pane_cx| {
                pane.set_appearance(theme, &family, size, translucent, pane_cx)
            });
        }
        if let Some(panel) = self.git_panel.clone() {
            panel.update(cx, |panel, panel_cx| panel.set_theme(theme, panel_cx));
        }
        cx.notify();
    }

    fn apply_theme(&mut self, name: &str, cx: &mut Context<Self>) {
        if let Some(theme) = themes::by_name(name) {
            self.theme = theme;
            self.settings.theme = name.to_string();
            self.apply_appearance(cx);
        }
    }

    fn set_font_family(&mut self, family: &str, cx: &mut Context<Self>) {
        self.settings.font_family = family.to_string();
        self.apply_appearance(cx);
    }

    fn set_background_opacity(&mut self, opacity: f32, cx: &mut Context<Self>) {
        self.settings.background_opacity = opacity.clamp(0.05, 1.0);
        self.apply_appearance(cx);
    }

    /// Native file picker without dependencies: osascript's choose-file,
    /// run off the UI thread.
    fn pick_background_image(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |ws, cx| {
            let picked = cx
                .background_executor()
                .spawn(async {
                    std::process::Command::new("/usr/bin/osascript")
                        .args([
                            "-e",
                            "POSIX path of (choose file of type {\"public.image\"} with prompt \"Background image\")",
                        ])
                        .output()
                        .ok()
                        .filter(|out| out.status.success())
                        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
                        .filter(|path| !path.is_empty())
                })
                .await;
            if let Some(path) = picked {
                let _ = ws.update(cx, |ws, cx| {
                    ws.settings.background_image = Some(path);
                    ws.apply_appearance(cx);
                });
            }
            Ok::<(), ()>(())
        })
        .detach();
    }

    fn clear_background_image(&mut self, cx: &mut Context<Self>) {
        self.settings.background_image = None;
        self.apply_appearance(cx);
    }

    fn set_font_size(&mut self, size: f32, cx: &mut Context<Self>) {
        self.settings.font_size = size.clamp(8.0, 32.0);
        self.apply_appearance(cx);
    }

    fn apply_auto_run(&mut self, command: String, cx: &mut Context<Self>) {
        let command = command.trim().to_string();
        if command.is_empty() {
            return;
        }
        let config = Some((
            command,
            self.auto_run_interval,
            self.auto_run_escape,
            self.auto_run_escape_delay,
        ));
        if let Some(pane) = self
            .focused_terminal
            .as_ref()
            .and_then(|id| self.panes.get(id))
        {
            pane.update(cx, |pane, _| pane.set_auto_run(config));
        }
        self.overlay = Overlay::None;
        cx.notify();
    }

    /// Close whatever sheet is open. Closing search also clears the pane's
    /// highlights — every close path must go through here, not just the
    /// field's own Escape handler.
    fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.overlay == Overlay::Search {
            if let Some(pane) = self
                .focused_terminal
                .as_ref()
                .and_then(|id| self.panes.get(id))
            {
                pane.update(cx, |pane, pane_cx| pane.set_search(None, pane_cx));
            }
        }
        self.overlay = Overlay::None;
        self.focus_active_pane(window, cx);
        cx.notify();
    }

    fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.overlay == Overlay::Search {
            self.close_overlay(window, cx);
        } else {
            self.overlay = Overlay::Search;
            cx.notify();
        }
    }

    fn toggle_git_panel(&mut self, cx: &mut Context<Self>) {
        if self.git_panel.take().is_none() {
            let theme = self.theme;
            let panel = cx.new(|panel_cx| GitPanel::new(theme, panel_cx));
            cx.subscribe(
                &panel,
                |ws, _panel, _event: &crate::git_panel::PanelClosed, cx| {
                    ws.git_panel = None;
                    cx.notify();
                },
            )
            .detach();
            self.git_panel = Some(panel);
            self.push_git_cwd(cx);
        }
        cx.notify();
    }

    /// Keep the git panel pointed at the focused terminal's directory.
    fn push_git_cwd(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.git_panel.clone() else {
            return;
        };
        let cwd = self
            .focused_terminal
            .as_ref()
            .and_then(|id| self.panes.get(id))
            .and_then(|pane| pane.read(cx).cwd());
        panel.update(cx, |panel, panel_cx| panel.set_target_cwd(cwd, panel_cx));
    }

    fn import_theme(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |ws, cx| {
            let picked = cx
                .background_executor()
                .spawn(async {
                    std::process::Command::new("/usr/bin/osascript")
                        .args([
                            "-e",
                            "POSIX path of (choose file of type {\"public.json\", \"public.plain-text\"} with prompt \"Theme JSON\")",
                        ])
                        .output()
                        .ok()
                        .filter(|out| out.status.success())
                        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
                        .filter(|path| !path.is_empty())
                        .and_then(|path| std::fs::read_to_string(path).ok())
                })
                .await;
            let _ = ws.update(cx, |ws, cx| {
                let note = match picked
                    .ok_or_else(|| "no file chosen".to_string())
                    .and_then(|raw| {
                        serde_json::from_str::<serde_json::Value>(&raw)
                            .map_err(|_| "invalid JSON file".to_string())
                    })
                    .and_then(|json| {
                        themes::import_custom(&json).map(|theme| (json, theme.name))
                    }) {
                    Ok((json, name)) => {
                        ws.settings.custom_themes.push(json);
                        ws.apply_theme(name, cx);
                        format!("imported and applied: {name}")
                    }
                    Err(err) => err,
                };
                ws.theme_action_note = Some(note);
                cx.notify();
            });
            Ok::<(), ()>(())
        })
        .detach();
    }

    fn export_theme(&mut self, cx: &mut Context<Self>) {
        let theme = self.theme;
        let json = themes::export_json(theme);
        let name: String = theme
            .name
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect();
        let dest = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_default()
            .join("Downloads")
            .join(format!("{name}-theme.json"));
        let note = match serde_json::to_string_pretty(&json)
            .map_err(|e| e.to_string())
            .and_then(|raw| std::fs::write(&dest, raw).map_err(|e| e.to_string()))
        {
            Ok(()) => format!("exported to {}", dest.display()),
            Err(err) => format!("export failed: {err}"),
        };
        self.theme_action_note = Some(note);
        cx.notify();
    }

    fn refresh_sessions(&mut self) {
        self.session_names = self.session_manager.list();
        self.session_names.sort();
    }

    fn current_layout(&self) -> Layout {
        Layout {
            tabs: self.tabs.clone(),
            active_tab_id: self
                .tabs
                .get(self.active_tab)
                .map(|t| t.id.clone())
                .unwrap_or_default(),
        }
    }

    fn save_session(&mut self, name: &str) {
        if name.trim().is_empty() {
            return;
        }
        let layout = self.current_layout();
        let _ = self
            .session_manager
            .save(name.trim(), &layout.to_session_json());
        self.refresh_sessions();
    }

    fn load_session(&mut self, name: &str, cx: &mut Context<Self>) {
        let Ok(Some(data)) = self.session_manager.load(name) else {
            return;
        };
        let Some(layout) = data.get("layout").and_then(Layout::from_session_json) else {
            return;
        };
        // Tear down current panes, then rebuild: fresh terminal per leaf
        // (fresh ids so pane entities and session files never collide).
        let old_ids: Vec<String> = self.panes.keys().cloned().collect();
        for id in old_ids {
            if let Some(pane) = self.panes.remove(&id) {
                pane.update(cx, move |pane, _| {
                    if let Some(handle) = pane.shutdown() {
                        std::thread::spawn(move || {
                            handle.join_with_deadline(std::time::Duration::from_secs(3))
                        });
                    }
                });
            }
        }
        self.tabs.clear();
        for tab in layout.tabs {
            let mut mapping = HashMap::new();
            for old_id in collect_terminal_ids(&tab.pane) {
                let new_id = self.fresh_id();
                self.spawn_pane(new_id.clone(), None, cx);
                mapping.insert(old_id, new_id);
            }
            let pane = remap_ids(&tab.pane, &mapping);
            self.tabs.push(Tab {
                id: tab.id,
                label: tab.label,
                pane,
            });
        }
        if self.tabs.is_empty() {
            self.add_tab(None, cx);
        } else {
            let wanted = layout.active_tab_id;
            self.active_tab = self.tabs.iter().position(|t| t.id == wanted).unwrap_or(0);
            self.focused_terminal = collect_terminal_ids(&self.tabs[self.active_tab].pane)
                .into_iter()
                .next();
        }
        self.overlay = Overlay::None;
        cx.notify();
    }

    // --- rendering ---

    fn render_tree(
        &self,
        node: &PaneNode,
        tab_index: usize,
        path: Vec<usize>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        match node {
            PaneNode::Terminal { terminal_id } => {
                let Some(pane) = self.panes.get(terminal_id) else {
                    return div().size_full().into_any_element();
                };
                let focused = self
                    .focused_terminal
                    .as_deref()
                    .is_some_and(|f| f == terminal_id);
                let theme = self.theme;

                div()
                    .size_full()
                    .relative()
                    .border_1()
                    .border_color(rgb(if focused {
                        theme.ui_accent
                    } else {
                        theme.ui_border
                    }))
                    .child(pane.clone())
                    .child({
                        // Per-terminal management cluster: always visible,
                        // acts on THIS pane (no focus dance).
                        let id_split_h = terminal_id.clone();
                        let id_split_v = terminal_id.clone();
                        let id_swap = terminal_id.clone();
                        let id_timer = terminal_id.clone();
                        let id_close = terminal_id.clone();
                        let bc_on = self.broadcast.is_enabled();
                        let bc_member = bc_on && self.broadcast.is_member(terminal_id);
                        let id_bc = terminal_id.clone();
                        let pane_btn = |label: &'static str| {
                            div()
                                .id(SharedString::from(format!("{label}-{terminal_id}")))
                                .cursor_pointer()
                                .px(px(4.0))
                                .h(px(15.0))
                                .flex()
                                .items_center()
                                .rounded(px(3.0))
                                .text_size(px(9.0))
                                .text_color(rgb(theme.ui_text_muted))
                                .hover(|style| style.bg(rgb(theme.ui_border)))
                                .child(SharedString::from(label))
                        };
                        div()
                            .absolute()
                            .top(px(2.0))
                            .right(px(2.0))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(1.0))
                            .px(px(2.0))
                            .rounded(px(4.0))
                            .bg(rgb(theme.ui_surface))
                            .opacity(0.75)
                            .hover(|style| style.opacity(1.0))
                            .children(bc_on.then(|| {
                                pane_btn(if bc_member { "bc:on" } else { "bc:off" }).on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |ws, _, _, cx| {
                                        ws.broadcast.toggle_member(&id_bc);
                                        cx.notify();
                                    }),
                                )
                            }))
                            .child(pane_btn("split-h").on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |ws, _, window, cx| {
                                    ws.focused_terminal = Some(id_split_h.clone());
                                    ws.split_focused(SplitDirection::Horizontal, cx);
                                    ws.focus_active_pane(window, cx);
                                }),
                            ))
                            .child(pane_btn("split-v").on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |ws, _, window, cx| {
                                    ws.focused_terminal = Some(id_split_v.clone());
                                    ws.split_focused(SplitDirection::Vertical, cx);
                                    ws.focus_active_pane(window, cx);
                                }),
                            ))
                            .child(pane_btn("swap").on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |ws, _, _, cx| {
                                    ws.swap_source =
                                        if ws.swap_source.as_deref() == Some(id_swap.as_str()) {
                                            None
                                        } else {
                                            Some(id_swap.clone())
                                        };
                                    cx.notify();
                                }),
                            ))
                            .child(pane_btn("timer").on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |ws, _, _, cx| {
                                    ws.focused_terminal = Some(id_timer.clone());
                                    ws.overlay = if ws.overlay == Overlay::AutoRun {
                                        Overlay::None
                                    } else {
                                        Overlay::AutoRun
                                    };
                                    cx.notify();
                                }),
                            ))
                            .child(pane_btn("x").on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |ws, _, window, cx| {
                                    ws.close_terminal(&id_close, cx);
                                    ws.focus_active_pane(window, cx);
                                }),
                            ))
                    })
                    .into_any_element()
            }
            PaneNode::Split {
                direction,
                children,
                sizes,
            } => {
                let ratios = sizes.unwrap_or([0.5, 0.5]);
                let horizontal = *direction == SplitDirection::Horizontal;
                let key = format!("{tab_index}:{path:?}");
                let bounds_map = Arc::clone(&self.split_bounds);
                let key_for_canvas = key.clone();

                let mut first_path = path.clone();
                first_path.push(0);
                let mut second_path = path.clone();
                second_path.push(1);

                let drag_path = path.clone();
                let drag_dir = *direction;

                let container = if horizontal {
                    div().flex().flex_row()
                } else {
                    div().flex().flex_col()
                };
                container
                    .size_full()
                    .relative()
                    .child(
                        gpui::canvas(
                            move |bounds, _, _| {
                                bounds_map.lock().unwrap().insert(
                                    key_for_canvas.clone(),
                                    (
                                        bounds.origin.x,
                                        bounds.origin.y,
                                        bounds.size.width,
                                        bounds.size.height,
                                    ),
                                );
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .child(
                        div()
                            .flex_grow()
                            .flex_basis(gpui::relative(ratios[0]))
                            .overflow_hidden()
                            .child(self.render_tree(&children[0], tab_index, first_path, cx)),
                    )
                    .child(
                        // Divider: draggable to resize.
                        div()
                            .id(SharedString::from(format!("divider-{key}")))
                            .flex_none()
                            .bg(rgb(self.theme.ui_border))
                            .when(horizontal, |d| d.w(px(3.0)).h_full().cursor_col_resize())
                            .when(!horizontal, |d| d.h(px(3.0)).w_full().cursor_row_resize())
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |ws, _, _, cx| {
                                    ws.drag = Some(DragState {
                                        tab_index,
                                        path: drag_path.clone(),
                                        direction: drag_dir,
                                    });
                                    cx.notify();
                                }),
                            ),
                    )
                    .child(
                        div()
                            .flex_grow()
                            .flex_basis(gpui::relative(ratios[1]))
                            .overflow_hidden()
                            .child(self.render_tree(&children[1], tab_index, second_path, cx)),
                    )
                    .into_any_element()
            }
        }
    }

    fn overlay_button(
        &self,
        label: &'static str,
        on_click: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = self.theme;
        div()
            .id(SharedString::from(format!("btn-{label}")))
            .cursor_pointer()
            .px(px(4.0))
            .rounded(px(3.0))
            .text_color(rgb(theme.ui_text_muted))
            .hover(|style| {
                style
                    .bg(rgb(theme.ui_border))
                    .text_color(rgb(theme.ui_text))
            })
            .child(SharedString::from(label))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |ws, _, window, cx| on_click(ws, window, cx)),
            )
    }

    fn set_split_sizes(&mut self, tab_index: usize, path: &[usize], sizes: [f32; 2]) {
        fn walk(node: &mut PaneNode, path: &[usize], sizes: [f32; 2]) {
            match (node, path) {
                (PaneNode::Split { sizes: s, .. }, []) => *s = Some(sizes),
                (PaneNode::Split { children, .. }, [head, rest @ ..]) => {
                    if let Some(child) = children.get_mut(*head) {
                        walk(child, rest, sizes);
                    }
                }
                _ => {}
            }
        }
        if let Some(tab) = self.tabs.get_mut(tab_index) {
            walk(&mut tab.pane, path, sizes);
        }
    }

    /// Controls for the focused pane, living in the bar (tmux-style): they
    /// act on whichever pane has focus, so they never clip in narrow splits.
    fn render_focused_controls(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let theme = self.theme;
        let focused = self.focused_terminal.clone()?;
        let pane = self.panes.get(&focused)?;
        let title = pane.read(cx).title();
        let cwd = pane.read(cx).cwd();
        Some(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .px(px(6.0))
                .text_color(rgb(theme.ui_text_muted))
                .child(SharedString::from(title))
                .children(cwd.map(|cwd| {
                    let display = cwd.replace(&std::env::var("HOME").unwrap_or_default(), "~");
                    let reveal = cwd.clone();
                    div()
                        .id("bar-cwd")
                        .cursor_pointer()
                        .max_w(px(280.0))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_color(rgb(theme.ui_accent))
                        .child(SharedString::from(display))
                        .on_mouse_down(MouseButton::Left, move |_, _, _| {
                            let _ = std::process::Command::new("/usr/bin/open")
                                .arg("-R")
                                .arg(&reveal)
                                .spawn();
                        })
                }))
                .children(self.swap_source.is_some().then(|| {
                    div()
                        .text_size(px(10.0))
                        .text_color(rgb(theme.yellow))
                        .child("click a pane to swap")
                })),
        )
    }

    fn render_font_family_row(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = self.theme;
        let current = self.settings.font_family.clone();
        const MONO_HINTS: [&str; 10] = [
            "Mono", "Menlo", "Monaco", "Courier", "Consolas", "Code", "Term", "Hack", "Fira",
            "Input",
        ];
        let mut families: Vec<String> = window
            .text_system()
            .all_font_names()
            .into_iter()
            .filter(|name| MONO_HINTS.iter().any(|hint| name.contains(hint)))
            .collect();
        families.sort();
        families.dedup();
        let chips: Vec<_> = families
            .into_iter()
            .map(|family| {
                let selected = family == current;
                let apply = family.clone();
                div()
                    .id(SharedString::from(format!("font-{family}")))
                    .cursor_pointer()
                    .px(px(8.0))
                    .py(px(2.0))
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(rgb(if selected {
                        theme.ui_accent
                    } else {
                        theme.ui_border
                    }))
                    .text_size(px(11.0))
                    .font_family(SharedString::from(family.clone()))
                    .child(SharedString::from(family))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |ws, _, _, cx| ws.set_font_family(&apply, cx)),
                    )
            })
            .collect();
        div()
            .flex()
            .flex_row()
            .items_start()
            .gap(px(8.0))
            .text_color(rgb(theme.ui_text_muted))
            .child(div().w(px(72.0)).pt(px(3.0)).child("font"))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap(px(4.0))
                    .children(chips),
            )
    }

    fn render_background_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let opacity = self.settings.background_opacity;
        let has_image = self.settings.background_image.is_some();
        let label: SharedString = match &self.settings.background_image {
            Some(path) => std::path::Path::new(path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.clone())
                .into(),
            None => "none".into(),
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .text_color(rgb(theme.ui_text_muted))
            .child(div().w(px(72.0)).child("background"))
            .child(div().text_size(px(11.0)).child(label))
            .child(self.overlay_button(
                "choose",
                |ws, _window, cx| ws.pick_background_image(cx),
                cx,
            ))
            .children(has_image.then(|| {
                self.overlay_button("clear", |ws, _window, cx| ws.clear_background_image(cx), cx)
            }))
            .children(has_image.then(|| {
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(4.0))
                    .child(self.overlay_button(
                        "dim",
                        |ws, _window, cx| {
                            ws.set_background_opacity(ws.settings.background_opacity - 0.1, cx)
                        },
                        cx,
                    ))
                    .child(SharedString::from(format!("{:.0}%", opacity * 100.0)))
                    .child(self.overlay_button(
                        "brighten",
                        |ws, _window, cx| {
                            ws.set_background_opacity(ws.settings.background_opacity + 0.1, cx)
                        },
                        cx,
                    ))
            }))
    }

    fn render_buddy_row(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        if self.buddy_field.is_none() {
            let theme_ref = self.theme;
            let field = cx.new(|field_cx| {
                TextField::new(
                    "agent command, e.g. claude -p {prompt}",
                    theme_ref,
                    field_cx,
                )
            });
            cx.subscribe(&field, |ws, _field, event: &TextFieldEvent, cx| {
                if let TextFieldEvent::Submitted(line) = event {
                    let mut parts = line.split_whitespace().map(String::from);
                    if let Some(command) = parts.next() {
                        ws.settings.buddy_command = command;
                        let args: Vec<String> = parts.collect();
                        ws.settings.buddy_args = if args.is_empty() {
                            vec!["-p".to_string(), "{prompt}".to_string()]
                        } else {
                            args
                        };
                        ws.settings.buddy_enabled = true;
                        let _ = ws.settings.save();
                        cx.notify();
                    }
                }
            })
            .detach();
            self.buddy_field = Some(field);
        }
        let enabled = self.settings.buddy_enabled;
        let configured = !self.settings.buddy_command.trim().is_empty();
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .text_color(rgb(theme.ui_text_muted))
            .child(div().w(px(72.0)).child("buddy"))
            .child(self.overlay_button(
                if enabled {
                    "reviewer: on"
                } else {
                    "reviewer: off"
                },
                |ws, _window, cx| {
                    ws.settings.buddy_enabled = !ws.settings.buddy_enabled;
                    let _ = ws.settings.save();
                    cx.notify();
                },
                cx,
            ))
            .child(div().flex_grow().child(self.buddy_field.clone().unwrap()))
            .children(configured.then(|| {
                div().text_size(px(10.0)).child(SharedString::from(format!(
                    "using: {} {}",
                    self.settings.buddy_command,
                    self.settings.buddy_args.join(" ")
                )))
            }))
    }

    fn render_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let mut segments = Vec::new();
        for (index, tab) in self.tabs.iter().enumerate() {
            let active = index == self.active_tab;
            segments.push(
                div()
                    .id(SharedString::from(format!("tab-seg-{index}")))
                    .cursor_pointer()
                    .px(px(10.0))
                    .py(px(2.0))
                    .rounded(px(3.0))
                    .bg(rgb(if active {
                        theme.ui_accent
                    } else {
                        theme.ui_surface
                    }))
                    .text_color(rgb(if active {
                        theme.ui_background
                    } else {
                        theme.ui_text_muted
                    }))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(5.0))
                    .child(
                        if let Some((rename_index, field)) = self
                            .rename_field
                            .as_ref()
                            .filter(|(rename_index, _)| *rename_index == index)
                        {
                            let _ = rename_index;
                            div().w(px(120.0)).child(field.clone()).into_any_element()
                        } else {
                            div()
                                .child(SharedString::from(tab.label.clone()))
                                .into_any_element()
                        },
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("tab-close-{index}")))
                            .cursor_pointer()
                            .px(px(2.0))
                            .text_color(rgb(if active {
                                theme.ui_background
                            } else {
                                theme.ui_text_muted
                            }))
                            .hover(|style| style.text_color(rgb(theme.red)))
                            .child("x")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |ws, _, window, cx| {
                                    cx.stop_propagation();
                                    ws.close_tab(index, cx);
                                    ws.focus_active_pane(window, cx);
                                }),
                            ),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |ws, event: &gpui::MouseDownEvent, window, cx| {
                            if ws
                                .rename_field
                                .as_ref()
                                .is_some_and(|(rename_index, _)| *rename_index == index)
                            {
                                return; // typing into the rename field
                            }
                            if event.click_count >= 2 {
                                ws.start_tab_rename(index, window, cx);
                            } else {
                                ws.select_tab(index, cx);
                                ws.focus_active_pane(window, cx);
                            }
                        }),
                    ),
            );
        }

        div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .px(px(8.0))
            .py(px(3.0))
            .bg(rgb(theme.ui_surface))
            .border_t_1()
            .border_color(rgb(theme.ui_border))
            .text_size(px(11.0))
            .children(segments)
            .child(self.overlay_button(
                "+",
                |ws, window, cx| {
                    ws.add_tab(None, cx);
                    ws.focus_active_pane(window, cx);
                },
                cx,
            ))
            .child(div().flex_grow())
            .children(self.buddy_note.clone().map(|note| {
                div()
                    .id("buddy-note")
                    .max_w(px(420.0))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(10.0))
                    .text_color(rgb(theme.ui_text_muted))
                    .child(SharedString::from(note))
            }))
            .children(self.render_focused_controls(cx))
            .child({
                let enabled = self.broadcast.is_enabled();
                let theme = self.theme;
                div()
                    .id("bc-toggle")
                    .cursor_pointer()
                    .px(px(6.0))
                    .py(px(2.0))
                    .rounded(px(3.0))
                    .bg(rgb(if enabled {
                        theme.ui_accent
                    } else {
                        theme.ui_surface
                    }))
                    .text_color(rgb(if enabled {
                        theme.ui_background
                    } else {
                        theme.ui_text_muted
                    }))
                    .child("broadcast")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|ws, _, window, cx| {
                            let now = !ws.broadcast.is_enabled();
                            ws.broadcast
                                .enabled
                                .store(now, std::sync::atomic::Ordering::Relaxed);
                            ws.focus_active_pane(window, cx);
                            cx.notify();
                        }),
                    )
            })
            .child(self.overlay_button("git", |ws, _window, cx| ws.toggle_git_panel(cx), cx))
            .child(self.overlay_button("search", |ws, window, cx| ws.toggle_search(window, cx), cx))
            .child(self.overlay_button(
                "sessions",
                |ws, _window, cx| {
                    ws.refresh_sessions();
                    ws.overlay = if ws.overlay == Overlay::Sessions {
                        Overlay::None
                    } else {
                        Overlay::Sessions
                    };
                    cx.notify();
                },
                cx,
            ))
            .child(self.overlay_button(
                "theme",
                |ws, window, cx| {
                    ws.overlay = if ws.overlay == Overlay::ThemePicker {
                        Overlay::None
                    } else {
                        window.focus(&ws.focus_handle);
                        Overlay::ThemePicker
                    };
                    cx.notify();
                },
                cx,
            ))
    }

    fn render_overlay(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let theme = self.theme;
        match self.overlay {
            Overlay::None => None,
            Overlay::ThemePicker => {
                let current = self.settings.theme.clone();
                let chips: Vec<_> = themes::all_themes()
                    .into_iter()
                    .map(|preset| {
                        let name = preset.name;
                        let selected = name == current;
                        // The palette IS the content: bg swatch + four accents.
                        let strip = [
                            preset.background,
                            preset.red,
                            preset.green,
                            preset.blue,
                            preset.magenta,
                        ];
                        div()
                            .id(SharedString::from(format!("theme-{name}")))
                            .cursor_pointer()
                            .px(px(10.0))
                            .py(px(6.0))
                            .rounded(px(5.0))
                            .border_1()
                            .border_color(rgb(if selected {
                                theme.ui_accent
                            } else {
                                theme.ui_border
                            }))
                            .bg(rgb(preset.background))
                            .hover(|style| style.border_color(rgb(theme.ui_accent)))
                            .flex()
                            .flex_col()
                            .gap(px(5.0))
                            .child(div().flex().flex_row().gap(px(3.0)).children(
                                strip.into_iter().map(|color| {
                                    div().w(px(14.0)).h(px(6.0)).rounded(px(2.0)).bg(rgb(color))
                                }),
                            ))
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(rgb(preset.foreground))
                                    .child(SharedString::from(name)),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |ws, _, window, cx| {
                                    ws.apply_theme(name, cx);
                                    ws.overlay = Overlay::None;
                                    ws.focus_active_pane(window, cx);
                                }),
                            )
                    })
                    .collect();
                let font_size = self.settings.font_size;
                Some(
                    self.sheet("theme", "click to apply - esc closes", cx)
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .flex_wrap()
                                .gap(px(6.0))
                                .children(chips),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.0))
                                .pt(px(4.0))
                                .text_color(rgb(theme.ui_text_muted))
                                .child(self.overlay_button(
                                    "-",
                                    |ws, _window, cx| {
                                        ws.set_font_size(ws.settings.font_size - 1.0, cx)
                                    },
                                    cx,
                                ))
                                .child(SharedString::from(format!("{font_size:.0} px")))
                                .child(self.overlay_button(
                                    "+",
                                    |ws, _window, cx| {
                                        ws.set_font_size(ws.settings.font_size + 1.0, cx)
                                    },
                                    cx,
                                )),
                        )
                        .child(self.render_font_family_row(window, cx))
                        .child(self.render_background_row(cx))
                        .child(self.render_buddy_row(cx))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.0))
                                .text_color(rgb(theme.ui_text_muted))
                                .child(div().w(px(72.0)).child("custom"))
                                .child(self.overlay_button(
                                    "import theme",
                                    |ws, _window, cx| ws.import_theme(cx),
                                    cx,
                                ))
                                .child(self.overlay_button(
                                    "export current",
                                    |ws, _window, cx| ws.export_theme(cx),
                                    cx,
                                ))
                                .children(self.theme_action_note.clone().map(|note| {
                                    div().text_size(px(10.0)).child(SharedString::from(note))
                                })),
                        )
                        .into_any_element(),
                )
            }
            Overlay::Search => {
                if self.search_field.is_none() {
                    let theme_ref = self.theme;
                    let field = cx
                        .new(|field_cx| TextField::new("find in scrollback", theme_ref, field_cx));
                    cx.subscribe_in(
                        &field,
                        window,
                        |ws, field, event: &TextFieldEvent, window, cx| match event {
                            TextFieldEvent::Submitted(_) => {
                                // Enter jumps to the next (older) match.
                                let needle = field.read(cx).value.clone();
                                if let Some(pane) =
                                    ws.focused_terminal.as_ref().and_then(|id| ws.panes.get(id))
                                {
                                    pane.update(cx, |pane, pane_cx| {
                                        pane.set_search(Some(&needle), pane_cx);
                                        pane.search_next(pane_cx);
                                    });
                                }
                            }
                            TextFieldEvent::Cancelled => {
                                // Same close path as the toolbar/root-escape:
                                // clears highlights and restores pane focus.
                                ws.close_overlay(window, cx);
                            }
                        },
                    )
                    .detach();
                    self.search_field = Some(field);
                }
                let field = self.search_field.clone().unwrap();
                field.read(cx).focus(window);
                // Live highlight as the needle changes.
                let needle = field.read(cx).value.clone();
                if let Some(pane) = self
                    .focused_terminal
                    .as_ref()
                    .and_then(|id| self.panes.get(id))
                {
                    pane.update(cx, |pane, pane_cx| {
                        pane.set_search((!needle.is_empty()).then_some(needle.as_str()), pane_cx)
                    });
                }
                Some(
                    self.sheet("search", "enter jumps to older matches - esc clears", cx)
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(6.0))
                                .child(div().text_color(rgb(theme.ui_accent)).child("find >"))
                                .child(div().flex_grow().child(field)),
                        )
                        .into_any_element(),
                )
            }
            Overlay::AutoRun => {
                if self.auto_run_field.is_none() {
                    let theme_ref = self.theme;
                    let field = cx.new(|field_cx| {
                        TextField::new("command, e.g. kubectl get pods", theme_ref, field_cx)
                    });
                    cx.subscribe(
                        &field,
                        |ws, _field, event: &TextFieldEvent, cx| match event {
                            TextFieldEvent::Submitted(command) => {
                                ws.apply_auto_run(command.clone(), cx);
                            }
                            TextFieldEvent::Cancelled => {
                                ws.overlay = Overlay::None;
                                cx.notify();
                            }
                        },
                    )
                    .detach();
                    self.auto_run_field = Some(field);
                }
                let field = self.auto_run_field.clone().unwrap();
                field.read(cx).focus(window);
                let interval = self.auto_run_interval;
                let escape = self.auto_run_escape;
                let escape_delay = self.auto_run_escape_delay;
                let active = self
                    .focused_terminal
                    .as_ref()
                    .and_then(|id| self.panes.get(id))
                    .is_some_and(|pane| pane.read(cx).auto_run.is_some());
                Some(
                    self.sheet("auto-run", "enter starts - esc closes", cx)
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(6.0))
                                .child(div().text_color(rgb(theme.ui_accent)).child("run >"))
                                .child(div().flex_grow().child(field)),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.0))
                                .text_color(rgb(theme.ui_text_muted))
                                .child(SharedString::from(format!("every {interval}s")))
                                .child(self.overlay_button(
                                    "-",
                                    |ws, _window, cx| {
                                        ws.auto_run_interval =
                                            ws.auto_run_interval.saturating_sub(1).max(1);
                                        cx.notify();
                                    },
                                    cx,
                                ))
                                .child(self.overlay_button(
                                    "+",
                                    |ws, _window, cx| {
                                        ws.auto_run_interval = (ws.auto_run_interval + 1).min(3600);
                                        cx.notify();
                                    },
                                    cx,
                                ))
                                .child(self.overlay_button(
                                    if escape {
                                        "esc after: on"
                                    } else {
                                        "esc after: off"
                                    },
                                    |ws, _window, cx| {
                                        ws.auto_run_escape = !ws.auto_run_escape;
                                        cx.notify();
                                    },
                                    cx,
                                ))
                                .children(
                                    escape.then(|| {
                                        SharedString::from(format!("{escape_delay}s delay"))
                                    }),
                                )
                                .children(active.then(|| {
                                    self.overlay_button(
                                        "stop",
                                        |ws, window, cx| {
                                            if let Some(pane) = ws
                                                .focused_terminal
                                                .as_ref()
                                                .and_then(|id| ws.panes.get(id))
                                            {
                                                pane.update(cx, |pane, _| pane.set_auto_run(None));
                                            }
                                            ws.overlay = Overlay::None;
                                            ws.focus_active_pane(window, cx);
                                            cx.notify();
                                        },
                                        cx,
                                    )
                                })),
                        )
                        .into_any_element(),
                )
            }
            Overlay::Sessions => {
                if self.session_field.is_none() {
                    let theme_ref = self.theme;
                    let field =
                        cx.new(|field_cx| TextField::new("session name", theme_ref, field_cx));
                    cx.subscribe(
                        &field,
                        |ws, field, event: &TextFieldEvent, cx| match event {
                            TextFieldEvent::Submitted(name) => {
                                ws.save_session(name);
                                field.update(cx, |f, cx| f.reset(cx));
                                cx.notify();
                            }
                            TextFieldEvent::Cancelled => {
                                ws.overlay = Overlay::None;
                                cx.notify();
                            }
                        },
                    )
                    .detach();
                    self.session_field = Some(field);
                }
                let field = self.session_field.clone().unwrap();
                field.read(cx).focus(window);

                let rows: Vec<_> = self
                    .session_names
                    .clone()
                    .into_iter()
                    .map(|name| {
                        let load_name = name.clone();
                        let delete_name = name.clone();
                        div()
                            .id(SharedString::from(format!("session-{name}")))
                            .px(px(8.0))
                            .py(px(4.0))
                            .rounded(px(4.0))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .hover(|style| style.bg(rgb(theme.ui_surface)))
                            .child(
                                div()
                                    .id(SharedString::from(format!("session-load-{name}")))
                                    .cursor_pointer()
                                    .flex_grow()
                                    .text_color(rgb(theme.ui_text))
                                    .child(SharedString::from(name.clone()))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |ws, _, window, cx| {
                                            ws.load_session(&load_name, cx);
                                            ws.focus_active_pane(window, cx);
                                        }),
                                    ),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("session-del-{name}")))
                                    .cursor_pointer()
                                    .px(px(4.0))
                                    .rounded(px(3.0))
                                    .opacity(0.5)
                                    .hover(|style| style.opacity(1.0).bg(rgb(theme.ui_surface)))
                                    .text_color(rgb(theme.red))
                                    .child("x")
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |ws, _, _, cx| {
                                            ws.session_manager.delete(&delete_name);
                                            ws.refresh_sessions();
                                            cx.notify();
                                        }),
                                    ),
                            )
                    })
                    .collect();
                let empty = rows.is_empty();
                Some(
                    self.sheet("sessions", "enter saves - click loads - esc closes", cx)
                        .child(
                            // Prompt-style save line.
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(6.0))
                                .child(div().text_color(rgb(theme.ui_accent)).child("save as >"))
                                .child(div().flex_grow().child(field)),
                        )
                        .child(div().flex().flex_col().gap(px(1.0)).children(rows))
                        .children(empty.then(|| {
                            div()
                                .text_size(px(11.0))
                                .text_color(rgb(theme.ui_text_muted))
                                .child("no saved sessions yet - type a name and press enter")
                        }))
                        .into_any_element(),
                )
            }
        }
    }

    /// Bottom sheet anchored above the bar: panels read as extensions of the
    /// tmux bar, not floating dialogs.
    fn sheet(
        &self,
        title: &'static str,
        hint: &'static str,
        _cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = self.theme;
        div()
            .id(SharedString::from(format!("sheet-{title}")))
            .absolute()
            .bottom_0()
            .left_0()
            .right_0()
            .max_h(px(360.0))
            .bg(rgb(theme.ui_background))
            .border_t_2()
            .border_color(rgb(theme.ui_accent))
            .px(px(14.0))
            .py(px(10.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .text_size(px(12.0))
            .text_color(rgb(theme.ui_text))
            .on_mouse_down(MouseButton::Left, |_, _, _| {}) // swallow
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .child(
                        div()
                            .text_color(rgb(theme.ui_accent))
                            .child(SharedString::from(title)),
                    )
                    .child(div().flex_grow())
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(rgb(theme.ui_text_muted))
                            .child(SharedString::from(hint)),
                    ),
            )
    }
}

fn remap_ids(node: &PaneNode, mapping: &HashMap<String, String>) -> PaneNode {
    match node {
        PaneNode::Terminal { terminal_id } => PaneNode::Terminal {
            terminal_id: mapping
                .get(terminal_id)
                .cloned()
                .unwrap_or_else(|| terminal_id.clone()),
        },
        PaneNode::Split {
            direction,
            children,
            sizes,
        } => PaneNode::Split {
            direction: *direction,
            children: children.iter().map(|c| remap_ids(c, mapping)).collect(),
            sizes: *sizes,
        },
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let active_tree = self.tabs.get(self.active_tab).map(|tab| tab.pane.clone());

        let content = match active_tree {
            Some(tree) => self.render_tree(&tree, self.active_tab, Vec::new(), cx),
            None => div().size_full().into_any_element(),
        };

        // Clicking away from a tab rename commits it (matching the old
        // app's blur behavior) — never re-steal focus from whatever the
        // user clicked.
        if let Some((index, field)) = self.rename_field.clone() {
            if !field.read(cx).is_focused(window) {
                let name = field.read(cx).value.trim().to_string();
                if !name.is_empty() {
                    if let Some(tab) = self.tabs.get_mut(index) {
                        tab.label = name;
                    }
                }
                self.rename_field = None;
            }
        }

        let background_layer = self.settings.background_image.as_ref().map(|path| {
            gpui::img(std::path::PathBuf::from(path))
                .absolute()
                .inset_0()
                .size_full()
                .object_fit(gpui::ObjectFit::Cover)
                .opacity(self.settings.background_opacity)
        });

        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(rgb(theme.ui_background))
            .children(background_layer)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|ws, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && ws.overlay != Overlay::None {
                    ws.close_overlay(window, cx);
                }
            }))
            .on_action(cx.listener(|ws, _: &NewTab, window, cx| {
                ws.add_tab(None, cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &CloseTab, window, cx| {
                let index = ws.active_tab;
                ws.close_tab(index, cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &CloseFocused, window, cx| {
                ws.close_focused(cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &SplitRight, window, cx| {
                ws.split_focused(SplitDirection::Horizontal, cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &SplitDown, window, cx| {
                ws.split_focused(SplitDirection::Vertical, cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &ToggleThemePicker, window, cx| {
                ws.overlay = if ws.overlay == Overlay::ThemePicker {
                    Overlay::None
                } else {
                    window.focus(&ws.focus_handle);
                    Overlay::ThemePicker
                };
                cx.notify();
            }))
            .on_action(cx.listener(|ws, _: &ToggleSessions, _, cx| {
                ws.refresh_sessions();
                ws.overlay = if ws.overlay == Overlay::Sessions {
                    Overlay::None
                } else {
                    Overlay::Sessions
                };
                cx.notify();
            }))
            .on_action(cx.listener(|ws, _: &ToggleSearch, window, cx| {
                ws.toggle_search(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &ToggleGitPanel, window, cx| {
                ws.toggle_git_panel(cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &SaveSessionAs, _, cx| {
                ws.refresh_sessions();
                ws.overlay = Overlay::Sessions;
                cx.notify();
            }))
            .on_action(cx.listener(|ws, _: &SelectTab1, window, cx| {
                ws.select_tab(0, cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &SelectTab2, window, cx| {
                ws.select_tab(1, cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &SelectTab3, window, cx| {
                ws.select_tab(2, cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &SelectTab4, window, cx| {
                ws.select_tab(3, cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &SelectTab5, window, cx| {
                ws.select_tab(4, cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &SelectTab6, window, cx| {
                ws.select_tab(5, cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &SelectTab7, window, cx| {
                ws.select_tab(6, cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &SelectTab8, window, cx| {
                ws.select_tab(7, cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &SelectTab9, window, cx| {
                ws.select_tab(8, cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_mouse_move(cx.listener(|ws, event: &MouseMoveEvent, _, cx| {
                let Some(drag) = &ws.drag else { return };
                let key = format!("{}:{:?}", drag.tab_index, drag.path);
                let Some((x, y, w, h)) = ws.split_bounds.lock().unwrap().get(&key).copied() else {
                    return;
                };
                let ratio = match drag.direction {
                    SplitDirection::Horizontal => {
                        (f32::from(event.position.x) - f32::from(x)) / f32::from(w).max(1.0)
                    }
                    SplitDirection::Vertical => {
                        (f32::from(event.position.y) - f32::from(y)) / f32::from(h).max(1.0)
                    }
                }
                .clamp(0.1, 0.9);
                let (tab_index, path) = (drag.tab_index, drag.path.clone());
                ws.set_split_sizes(tab_index, &path, [ratio, 1.0 - ratio]);
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|ws, _: &MouseUpEvent, _, cx| {
                    if ws.drag.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .child(
                // Titlebar drag strip under the traffic lights.
                div()
                    .flex_none()
                    .h(px(34.0))
                    .w_full()
                    .bg(rgb(theme.ui_background))
                    .window_control_area(gpui::WindowControlArea::Drag),
            )
            .child(
                div()
                    .flex_grow()
                    .overflow_hidden()
                    .relative()
                    .child(content),
            )
            .child(self.render_bar(cx))
            .children(self.render_overlay(window, cx))
    }
}
