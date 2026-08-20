//! One terminal pane: a gpui view wrapping a [`TermSession`].
//!
//! Rendering follows the spike's proven path — one div per viewport row with
//! one text child per styled run — with an absolutely-positioned cursor
//! overlay. Cell metrics are measured through gpui's text system so mouse
//! cell math and the cursor overlay stay exact.

use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    div, px, rgb, App, ClipboardItem, Context, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, ScrollWheelEvent,
    SharedString, Window,
};

use crate::keys::{self, KeyInput};
use crate::term_session::{
    CellColor, CursorStyle, RenderableSnapshot, SessionEvent, ShutdownHandle, TermSession,
};
use crate::themes::{ansi_256, Theme};

/// Bounds captured by the measuring canvas: (origin_x, origin_y, w, h).
type MeasuredBounds = (Pixels, Pixels, Pixels, Pixels);

/// Events the workspace listens for.
#[derive(Clone, Debug)]
pub enum PaneEvent {
    Focused,
    TitleChanged,
    Exited,
}

pub struct TerminalPane {
    pub id: String,
    session: Option<TermSession>,
    snapshot: RenderableSnapshot,
    focus_handle: FocusHandle,
    theme: &'static Theme,
    font_family: SharedString,
    font_size: f32,
    cell_width: Pixels,
    line_height: Pixels,
    selecting: bool,
    /// Pane origin in window coordinates (for mouse cell math), updated from
    /// the measuring canvas via `pending_bounds` on each pump tick.
    origin: (Pixels, Pixels),
    /// (origin_x, origin_y, width, height) written during prepaint by the
    /// measuring canvas; applied outside the render pass.
    pending_bounds: std::sync::Arc<std::sync::Mutex<Option<MeasuredBounds>>>,
}

const PADDING: f32 = 6.0;

impl TerminalPane {
    pub fn new(
        id: String,
        working_directory: Option<std::path::PathBuf>,
        theme: &'static Theme,
        font_family: String,
        font_size: f32,
        cx: &mut Context<Self>,
    ) -> Self {
        // Metrics are estimates until the first render measures for real.
        let cell_width = px(font_size * 0.6);
        let line_height = px((font_size * 1.4).round());

        let session = TermSession::spawn(
            80,
            24,
            f32::from(cell_width) as u16,
            f32::from(line_height) as u16,
            working_directory,
        )
        .ok();

        // Dirty-flag pump (spike-proven): PTY thread flips the flag, this task
        // turns it into re-renders; bounded by construction.
        cx.spawn(async move |pane, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                let alive = pane.update(cx, |pane: &mut TerminalPane, cx| {
                    // Apply the bounds measured during the last paint: origin
                    // for mouse math, size for the PTY grid.
                    let measured = pane.pending_bounds.lock().unwrap().take();
                    if let Some((x, y, w, h)) = measured {
                        pane.origin = (x, y);
                        pane.resize_to(w, h);
                    }
                    if pane.session.as_ref().is_some_and(|s| s.take_dirty()) {
                        pane.process_events(cx);
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();

        let snapshot = RenderableSnapshot {
            cols: 80,
            lines: 24,
            rows: Vec::new(),
            cursor: crate::term_session::SnapshotCursor {
                col: 0,
                row: Some(0),
                style: CursorStyle::Bar,
            },
            display_offset: 0,
            selection: Vec::new(),
            app_cursor_mode: false,
            mouse_tracking: false,
            focused_title: None,
            exited: None,
        };

        Self {
            id,
            session,
            snapshot,
            focus_handle: cx.focus_handle(),
            theme,
            font_family: font_family.into(),
            font_size,
            cell_width,
            line_height,
            selecting: false,
            origin: (px(0.0), px(0.0)),
            pending_bounds: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }

    pub fn set_appearance(
        &mut self,
        theme: &'static Theme,
        font_family: &str,
        font_size: f32,
        cx: &mut Context<Self>,
    ) {
        self.theme = theme;
        self.font_family = font_family.to_string().into();
        self.font_size = font_size;
        cx.notify();
    }

    pub fn title(&self) -> String {
        self.snapshot
            .focused_title
            .clone()
            .unwrap_or_else(|| "Terminal".to_string())
    }

    pub fn cwd(&self) -> Option<String> {
        self.session.as_ref()?.cwd()
    }

    pub fn focus(&self, window: &mut Window) {
        self.focus_handle.focus(window);
    }

    /// Begin teardown; the returned handle must be joined off the UI thread.
    pub fn shutdown(&mut self) -> Option<ShutdownHandle> {
        self.session.take().map(TermSession::shutdown)
    }

    fn process_events(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        for event in session.drain_events() {
            match event {
                SessionEvent::TitleChanged(title) => {
                    drop(title);
                    cx.emit(PaneEvent::TitleChanged);
                }
                SessionEvent::Exited(_) => {
                    cx.emit(PaneEvent::Exited);
                }
                SessionEvent::Bell => {}
            }
        }
    }

    fn write(&self, bytes: Vec<u8>) {
        if let Some(session) = &self.session {
            session.write(bytes);
        }
    }

    fn measure_cell(&mut self, window: &mut Window, cx: &mut App) {
        // Contract rev 2 §9: explicit fallback chain; Menlo ships with macOS.
        let mut font = gpui::font(self.font_family.clone());
        font.fallbacks = Some(gpui::FontFallbacks::from_fonts(vec![
            "Menlo".to_string(),
            "Monaco".to_string(),
        ]));
        let font_id = window.text_system().resolve_font(&font);
        if let Ok(advance) = window.text_system().em_advance(font_id, px(self.font_size)) {
            self.cell_width = advance;
        }
        self.line_height = px((self.font_size * 1.4).round());
        let _ = cx;
    }

    fn grid_size_for(&self, bounds_w: Pixels, bounds_h: Pixels) -> (usize, usize) {
        let usable_w = f32::from(bounds_w) - PADDING * 2.0;
        let usable_h = f32::from(bounds_h) - PADDING * 2.0;
        let cols = (usable_w / f32::from(self.cell_width)).floor().max(2.0) as usize;
        let lines = (usable_h / f32::from(self.line_height)).floor().max(2.0) as usize;
        (cols, lines)
    }

    fn cell_at(
        &self,
        pos_x: Pixels,
        pos_y: Pixels,
        origin_x: Pixels,
        origin_y: Pixels,
    ) -> (usize, usize) {
        let x = (f32::from(pos_x) - f32::from(origin_x) - PADDING).max(0.0);
        let y = (f32::from(pos_y) - f32::from(origin_y) - PADDING).max(0.0);
        let col = (x / f32::from(self.cell_width)) as usize;
        let row = (y / f32::from(self.line_height)) as usize;
        (col, row)
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        let m = &ks.modifiers;

        // Restarting a dead pane: any key on an exited pane asks the
        // workspace to close it (contract rev 1, shutdown section).
        if self.snapshot.exited.is_some() {
            cx.emit(PaneEvent::Exited);
            return;
        }

        // App-level chords (Cmd+T/W/digit/...) are handled by the workspace
        // via gpui actions before reaching us; what arrives here is terminal
        // input. Try the ported chord table first.
        let input = KeyInput {
            key: ks.key.as_str(),
            cmd: m.platform,
            alt: m.alt,
            ctrl: m.control,
            shift: m.shift,
        };
        if m.platform
            || m.control
            || matches!(
                ks.key.as_str(),
                "enter"
                    | "backspace"
                    | "delete"
                    | "escape"
                    | "tab"
                    | "up"
                    | "down"
                    | "left"
                    | "right"
                    | "home"
                    | "end"
                    | "pageup"
                    | "pagedown"
            )
            || ks.key.starts_with('f') && ks.key.len() <= 3
        {
            if m.platform && ks.key == "c" {
                if let Some(text) = self.session.as_ref().and_then(|s| s.selection_text()) {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    return;
                }
            }
            if m.platform && ks.key == "v" {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.write(text.into_bytes());
                }
                return;
            }
            if let Some(bytes) = keys::key_to_bytes(&input, self.snapshot.app_cursor_mode, true) {
                self.write(bytes);
                self.scroll_to_bottom_on_input(cx);
                return;
            }
            if m.platform {
                return; // reserved app chord; never forward
            }
        }

        // Alt+printable with option-as-meta.
        if m.alt {
            if let Some(bytes) = keys::key_to_bytes(&input, self.snapshot.app_cursor_mode, true) {
                self.write(bytes);
                self.scroll_to_bottom_on_input(cx);
                return;
            }
        }

        // Printable input: key_char carries the composed character (shift and
        // option layers already applied by the platform).
        if let Some(key_char) = &ks.key_char {
            if !key_char.is_empty() {
                self.write(key_char.as_bytes().to_vec());
                self.scroll_to_bottom_on_input(cx);
            }
        }
        let _ = window;
    }

    fn scroll_to_bottom_on_input(&mut self, cx: &mut Context<Self>) {
        if self.snapshot.display_offset > 0 {
            if let Some(session) = self.session.as_mut() {
                session.queue_scroll(-(self.snapshot.display_offset as i32));
            }
        }
        cx.notify();
    }

    fn resolve_fg(&self, color: CellColor) -> u32 {
        match color {
            CellColor::Default => self.theme.foreground,
            CellColor::Indexed(i) => ansi_256(i, self.theme),
            CellColor::Rgb(r, g, b) => ((r as u32) << 16) | ((g as u32) << 8) | b as u32,
        }
    }

    fn resolve_bg(&self, color: CellColor) -> Option<u32> {
        match color {
            CellColor::Default => None, // pane background shows through
            CellColor::Indexed(i) => Some(ansi_256(i, self.theme)),
            CellColor::Rgb(r, g, b) => Some(((r as u32) << 16) | ((g as u32) << 8) | b as u32),
        }
    }
}

/// Scale an 0xRRGGBB color's channels by 2/3 (DIM), never via alpha.
fn dim(color: u32) -> u32 {
    let r = ((color >> 16) & 0xff) * 2 / 3;
    let g = ((color >> 8) & 0xff) * 2 / 3;
    let b = (color & 0xff) * 2 / 3;
    (r << 16) | (g << 8) | b
}

/// One visual run: consecutive cells sharing style + selection state.
struct Run {
    text: String,
    fg: u32,
    bg: Option<u32>,
    bold: bool,
    italic: bool,
    underline: bool,
}

impl EventEmitter<PaneEvent> for TerminalPane {}

impl Focusable for TerminalPane {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.measure_cell(window, cx);

        if let Some(session) = self.session.as_mut() {
            self.snapshot = session.sync_and_snapshot();
        }

        let theme = self.theme;
        let focused = self.focus_handle.is_focused(window);
        let snapshot = &self.snapshot;
        let cell_w = self.cell_width;
        let line_h = self.line_height;

        // Build styled runs per row.
        let selection: std::collections::HashSet<(usize, usize)> =
            snapshot.selection.iter().copied().collect();
        let mut row_divs = Vec::with_capacity(snapshot.lines);
        for (row_idx, row) in snapshot.rows.iter().enumerate() {
            let mut runs: Vec<Run> = Vec::new();
            for (col_idx, cell) in row.iter().enumerate() {
                if cell.wide_spacer {
                    continue;
                }
                let selected = selection.contains(&(col_idx, row_idx));
                let style = &cell.style;
                let (mut fg, mut bg) = if style.inverse {
                    let fg_resolved = self.resolve_fg(style.fg);
                    let bg_resolved = self.resolve_bg(style.bg).unwrap_or(theme.background);
                    (bg_resolved, Some(fg_resolved))
                } else {
                    (self.resolve_fg(style.fg), self.resolve_bg(style.bg))
                };
                if style.dim {
                    fg = dim(fg);
                }
                if style.hidden {
                    fg = bg.unwrap_or(theme.background);
                }
                if selected {
                    bg = Some(theme.selection);
                }
                let ch = if cell.ch == '\0' { ' ' } else { cell.ch };
                let matches_last = runs.last().is_some_and(|r| {
                    r.fg == fg
                        && r.bg == bg
                        && r.bold == style.bold
                        && r.italic == style.italic
                        && r.underline == style.underline
                });
                if matches_last {
                    runs.last_mut().unwrap().text.push(ch);
                } else {
                    runs.push(Run {
                        text: ch.to_string(),
                        fg,
                        bg,
                        bold: style.bold,
                        italic: style.italic,
                        underline: style.underline,
                    });
                }
            }
            // Trailing default-style whitespace is layout-neutral; keep rows
            // non-empty so every row occupies one line.
            while runs
                .last()
                .is_some_and(|r| r.bg.is_none() && r.text.trim().is_empty() && runs.len() > 1)
            {
                runs.pop();
            }
            if runs.is_empty() {
                runs.push(Run {
                    text: " ".to_string(),
                    fg: theme.foreground,
                    bg: None,
                    bold: false,
                    italic: false,
                    underline: false,
                });
            }

            let row_div = div()
                .flex()
                .flex_row()
                .whitespace_nowrap()
                .h(line_h)
                .children(runs.into_iter().map(|run| {
                    let mut d = div()
                        .child(SharedString::from(run.text))
                        .text_color(rgb(run.fg));
                    if let Some(bg) = run.bg {
                        d = d.bg(rgb(bg));
                    }
                    if run.bold {
                        d = d.font_weight(gpui::FontWeight::BOLD);
                    }
                    if run.italic {
                        d = d.italic();
                    }
                    if run.underline {
                        d = d.underline();
                    }
                    d
                }));
            row_divs.push(row_div);
        }

        // Cursor overlay (2px bar focused, hollow block unfocused; hidden when
        // the app hides it or it scrolled out of view).
        let cursor_div = match (snapshot.cursor.style, snapshot.cursor.row) {
            (CursorStyle::Hidden, _) | (_, None) => None,
            (style, Some(row)) => {
                let left = px(PADDING + snapshot.cursor.col as f32 * f32::from(cell_w));
                let top = px(PADDING + row as f32 * f32::from(line_h));
                let d = div().absolute().left(left).top(top).h(line_h);
                Some(if !focused {
                    d.w(cell_w).border_1().border_color(rgb(theme.cursor))
                } else {
                    match style {
                        CursorStyle::Underline => {
                            d.w(cell_w).border_b_2().border_color(rgb(theme.cursor))
                        }
                        CursorStyle::Block => d.w(cell_w).bg(rgb(theme.cursor)).opacity(0.7),
                        _ => d.w(px(2.0)).bg(rgb(theme.cursor)),
                    }
                })
            }
        };

        let pane = cx.entity();
        let pane_for_move = pane.clone();
        let pane_for_up = pane.clone();
        let pane_for_scroll = pane.clone();

        div()
            .id(SharedString::from(self.id.clone()))
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .relative()
            .bg(rgb(theme.background))
            .p(px(PADDING))
            .overflow_hidden()
            .font_family(self.font_family.clone())
            .text_size(px(self.font_size))
            .line_height(line_h)
            .text_color(rgb(theme.foreground))
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.focus(window);
                    cx.emit(PaneEvent::Focused);
                    // Cell math needs the pane's origin: derive from the event
                    // position within the hitbox — gpui reports window coords,
                    // and the pane's origin is tracked by the workspace via
                    // absolute positioning; v1 uses the wrapping element's
                    // bounds through event.position relative math below.
                    let (col, row) = this.cell_at(
                        event.position.x,
                        event.position.y,
                        this.last_origin_x(),
                        this.last_origin_y(),
                    );
                    this.handle_click(col, row, cx);
                }),
            )
            .on_mouse_move(
                cx.listener(move |this, event: &MouseMoveEvent, _window, cx| {
                    let _ = &pane_for_move;
                    if this.selecting && event.dragging() {
                        let (col, row) = this.cell_at(
                            event.position.x,
                            event.position.y,
                            this.last_origin_x(),
                            this.last_origin_y(),
                        );
                        if let Some(session) = this.session.as_mut() {
                            session.queue_selection_update(col, row);
                        }
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _event: &MouseUpEvent, _window, _cx| {
                    let _ = &pane_for_up;
                    this.selecting = false;
                }),
            )
            .on_scroll_wheel(
                cx.listener(move |this, event: &ScrollWheelEvent, _window, cx| {
                    let _ = &pane_for_scroll;
                    let delta = event.delta.pixel_delta(this.line_height).y;
                    let lines = (f32::from(delta) / f32::from(this.line_height)).round() as i32;
                    if lines != 0 {
                        if let Some(session) = this.session.as_mut() {
                            session.queue_scroll(lines);
                        }
                        cx.notify();
                    }
                }),
            )
            .child({
                // Measuring canvas: captures this pane's window-space bounds
                // during prepaint; applied on the next pump tick (no
                // re-entrant entity updates from inside render).
                let pending = std::sync::Arc::clone(&self.pending_bounds);
                gpui::canvas(
                    move |bounds, _window, _cx| {
                        *pending.lock().unwrap() = Some((
                            bounds.origin.x,
                            bounds.origin.y,
                            bounds.size.width,
                            bounds.size.height,
                        ));
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full()
            })
            .child(div().flex().flex_col().children(row_divs))
            .children(cursor_div)
            .children(self.snapshot.exited.is_some().then(|| {
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(rgb(theme.background))
                    .opacity(0.85)
                    .child(
                        div()
                            .text_color(rgb(theme.ui_text_muted))
                            .child("[process exited - press any key to close]"),
                    )
            }))
    }
}

impl TerminalPane {
    // v1 origin tracking: panes are laid out by flex; gpui hands us window
    // coordinates in mouse events. The workspace stores each pane's origin
    // after layout via `set_origin` (called from its own render pass with
    // prepainted bounds). Until the first layout, origin is zero.
    fn last_origin_x(&self) -> Pixels {
        self.origin.0
    }
    fn last_origin_y(&self) -> Pixels {
        self.origin.1
    }

    pub fn resize_to(&mut self, width: Pixels, height: Pixels) {
        let (cols, lines) = self.grid_size_for(width, height);
        if let Some(session) = self.session.as_mut() {
            session.queue_resize(
                cols,
                lines,
                f32::from(self.cell_width) as u16,
                f32::from(self.line_height) as u16,
            );
        }
    }

    fn handle_click(&mut self, col: usize, row: usize, cx: &mut Context<Self>) {
        let snapshot = &self.snapshot;
        // Click-to-move guards (ported from the web app): prompt row only, at
        // bottom, no selection, no app mouse tracking, normal buffer implied
        // by mouse_tracking check + display_offset.
        if !snapshot.mouse_tracking && snapshot.display_offset == 0 && snapshot.selection.is_empty()
        {
            if let Some(cursor_row) = snapshot.cursor.row {
                if let Some(bytes) = keys::click_to_move_bytes(
                    col,
                    row,
                    snapshot.cursor.col,
                    cursor_row,
                    snapshot.cols.max(1),
                    snapshot.app_cursor_mode,
                ) {
                    self.write(bytes);
                    cx.notify();
                    return;
                }
            }
        }
        // Otherwise: begin a selection drag.
        if let Some(session) = self.session.as_mut() {
            session.queue_selection_clear();
            session.queue_selection_start(col, row);
        }
        self.selecting = true;
        cx.notify();
    }
}
