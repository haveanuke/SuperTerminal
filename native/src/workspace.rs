//! Root view: tabs of split-pane terminals, the tmux-style bottom bar, and
//! the theme/sessions overlays.
//!
//! Structure follows the contract: the serializable pane tree is the
//! `layout::PaneNode` DTO (String terminal ids); live gpui entities live in a
//! side map keyed by those ids.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use gpui::prelude::*;
use gpui::{
    div, px, rgb, App, Context, Entity, FocusHandle, Focusable, MouseButton, MouseMoveEvent,
    MouseUpEvent, Pixels, SharedString, Window,
};

use superterminal_core::session::SessionManager;

use crate::layout::{
    collect_terminal_ids, insert_split, remove_terminal, Layout, PaneNode, SplitDirection, Tab,
};
use crate::pane::{PaneEvent, TerminalPane};
use crate::settings::Settings;
use crate::term_session::ShutdownHandle;
use crate::text_field::{TextField, TextFieldEvent};
use crate::themes::{self, Theme};

gpui::actions!(
    superterminal,
    [
        NewTab,
        CloseFocused,
        SplitRight,
        SplitDown,
        ToggleThemePicker,
        ToggleSessions,
        SaveSessionAs,
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
    focus_handle: FocusHandle,
    drag: Option<DragState>,
    /// Split-container bounds (window coords) written by measuring canvases,
    /// keyed by "tab_index:path" — used for divider drag math.
    split_bounds: Arc<Mutex<SplitBoundsMap>>,
    next_id: u64,
    /// Shutdown handles for panes being torn down; joined on app quit.
    pending_shutdowns: Arc<Mutex<Vec<ShutdownHandle>>>,
}

impl Workspace {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let settings = Settings::load();
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
            focus_handle: cx.focus_handle(),
            drag: None,
            split_bounds: Arc::new(Mutex::new(HashMap::new())),
            next_id: 1,
            pending_shutdowns: Arc::new(Mutex::new(Vec::new())),
        };
        this.add_tab(None, cx);
        this
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
        let pane_id = id.clone();
        let pane = cx.new(|pane_cx| TerminalPane::new(pane_id, cwd, theme, family, size, pane_cx));
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
                self.focused_terminal = Some(pane_id);
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
            label: "Terminal".to_string(),
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
                self.tabs.remove(tab_index);
                if self.tabs.is_empty() {
                    self.add_tab(None, cx);
                } else {
                    self.active_tab = self.active_tab.min(self.tabs.len() - 1);
                    self.focused_terminal = collect_terminal_ids(&self.tabs[self.active_tab].pane)
                        .into_iter()
                        .next();
                }
            }
        }
        cx.notify();
    }

    fn close_focused(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.focused_terminal.clone() {
            self.close_terminal(&id, cx);
        }
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

    fn apply_theme(&mut self, name: &str, cx: &mut Context<Self>) {
        if let Some(theme) = themes::by_name(name) {
            self.theme = theme;
            self.settings.theme = name.to_string();
            let _ = self.settings.save();
            let family = self.settings.font_family.clone();
            let size = self.settings.font_size;
            for pane in self.panes.values() {
                pane.update(cx, |pane, pane_cx| {
                    pane.set_appearance(theme, &family, size, pane_cx)
                });
            }
            cx.notify();
        }
    }

    fn set_font_size(&mut self, size: f32, cx: &mut Context<Self>) {
        let size = size.clamp(8.0, 32.0);
        self.settings.font_size = size;
        let _ = self.settings.save();
        let theme = self.theme;
        let family = self.settings.font_family.clone();
        for pane in self.panes.values() {
            pane.update(cx, |pane, pane_cx| {
                pane.set_appearance(theme, &family, size, pane_cx)
            });
        }
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
            .hover(|style| style.bg(rgb(theme.ui_border)))
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
                .child(self.overlay_button(
                    "split-h",
                    |ws, _window, cx| ws.split_focused(SplitDirection::Horizontal, cx),
                    cx,
                ))
                .child(self.overlay_button(
                    "split-v",
                    |ws, _window, cx| ws.split_focused(SplitDirection::Vertical, cx),
                    cx,
                ))
                .child(self.overlay_button(
                    "close",
                    |ws, window, cx| {
                        ws.close_focused(cx);
                        ws.focus_active_pane(window, cx);
                    },
                    cx,
                )),
        )
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
                    .child(SharedString::from(format!("{}:{}", index + 1, tab.label)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |ws, _, window, cx| {
                            ws.select_tab(index, cx);
                            ws.focus_active_pane(window, cx);
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
            .children(self.render_focused_controls(cx))
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
                |ws, _window, cx| {
                    ws.overlay = if ws.overlay == Overlay::ThemePicker {
                        Overlay::None
                    } else {
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
                let items: Vec<_> = themes::presets()
                    .iter()
                    .map(|preset| {
                        let name = preset.name;
                        let selected = name == current;
                        div()
                            .id(SharedString::from(format!("theme-{name}")))
                            .cursor_pointer()
                            .px(px(10.0))
                            .py(px(4.0))
                            .rounded(px(4.0))
                            .bg(rgb(if selected {
                                theme.ui_border
                            } else {
                                theme.ui_surface
                            }))
                            .hover(|style| style.bg(rgb(theme.ui_border)))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .w(px(12.0))
                                    .h(px(12.0))
                                    .rounded(px(3.0))
                                    .bg(rgb(preset.background))
                                    .border_1()
                                    .border_color(rgb(preset.ui_accent)),
                            )
                            .child(SharedString::from(name))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |ws, _, _, cx| {
                                    ws.apply_theme(name, cx);
                                    ws.overlay = Overlay::None;
                                }),
                            )
                    })
                    .collect();
                let font_size = self.settings.font_size;
                Some(
                    self.modal_frame("Theme", cx)
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.0))
                                .child(SharedString::from(format!("font size: {font_size:.0}")))
                                .child(self.overlay_button(
                                    "-",
                                    |ws, _window, cx| {
                                        ws.set_font_size(ws.settings.font_size - 1.0, cx)
                                    },
                                    cx,
                                ))
                                .child(self.overlay_button(
                                    "+",
                                    |ws, _window, cx| {
                                        ws.set_font_size(ws.settings.font_size + 1.0, cx)
                                    },
                                    cx,
                                )),
                        )
                        .child(div().flex().flex_col().gap(px(2.0)).children(items))
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

                let items: Vec<_> = self
                    .session_names
                    .clone()
                    .into_iter()
                    .map(|name| {
                        let load_name = name.clone();
                        let delete_name = name.clone();
                        div()
                            .id(SharedString::from(format!("session-{name}")))
                            .px(px(10.0))
                            .py(px(4.0))
                            .rounded(px(4.0))
                            .hover(|style| style.bg(rgb(theme.ui_border)))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .id(SharedString::from(format!("session-load-{name}")))
                                    .cursor_pointer()
                                    .flex_grow()
                                    .child(SharedString::from(name.clone()))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |ws, _, _, cx| {
                                            ws.load_session(&load_name, cx);
                                        }),
                                    ),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("session-del-{name}")))
                                    .cursor_pointer()
                                    .text_color(rgb(theme.red))
                                    .child("delete")
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
                Some(
                    self.modal_frame("Sessions", cx)
                        .child(field)
                        .child(
                            div()
                                .text_size(px(10.0))
                                .text_color(rgb(theme.ui_text_muted))
                                .child("enter saves the current layout - click a name to load"),
                        )
                        .child(div().flex().flex_col().gap(px(2.0)).children(items))
                        .into_any_element(),
                )
            }
        }
    }

    fn modal_frame(
        &self,
        title: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = self.theme;
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::rgba(0x00000088))
            .id(SharedString::from(format!("modal-{title}")))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|ws, _, _, cx| {
                    ws.overlay = Overlay::None;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .id(SharedString::from(format!("modal-body-{title}")))
                    .on_mouse_down(MouseButton::Left, |_, _, _| {}) // swallow
                    .w(px(380.0))
                    .max_h(px(460.0))
                    .overflow_hidden()
                    .rounded(px(8.0))
                    .bg(rgb(theme.ui_surface))
                    .border_1()
                    .border_color(rgb(theme.ui_border))
                    .p(px(12.0))
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .text_color(rgb(theme.ui_text))
                    .text_size(px(12.0))
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(rgb(theme.ui_accent))
                            .child(SharedString::from(title)),
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

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(theme.ui_background))
            .on_action(cx.listener(|ws, _: &NewTab, window, cx| {
                ws.add_tab(None, cx);
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
            .on_action(cx.listener(|ws, _: &ToggleThemePicker, _, cx| {
                ws.overlay = if ws.overlay == Overlay::ThemePicker {
                    Overlay::None
                } else {
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
                    .h(px(28.0))
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
