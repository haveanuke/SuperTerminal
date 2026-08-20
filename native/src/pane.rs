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
use alacritty_terminal::event_loop::{EventLoopSender, Msg};

/// Shared broadcast state: when enabled, keystrokes from any member pane fan
/// out to every member's PTY.
#[derive(Default)]
pub struct BroadcastHub {
    pub enabled: std::sync::atomic::AtomicBool,
    pub members: std::sync::Mutex<std::collections::HashMap<String, (bool, EventLoopSender)>>,
}

impl BroadcastHub {
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn is_member(&self, id: &str) -> bool {
        self.members
            .lock()
            .unwrap()
            .get(id)
            .is_some_and(|(on, _)| *on)
    }

    pub fn toggle_member(&self, id: &str) {
        if let Some((on, _)) = self.members.lock().unwrap().get_mut(id) {
            *on = !*on;
        }
    }
}
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
    /// The family that is actually installed (settings family may be absent);
    /// used for BOTH row rendering and cell measurement so they never drift.
    resolved_family: Option<SharedString>,
    font_size: f32,
    /// When the workspace shows a background image, panes render their
    /// background translucent so the image shows through.
    translucent: bool,
    cell_width: Pixels,
    line_height: Pixels,
    selecting: bool,
    blink_on: bool,
    blink_tick: u32,
    /// Held marked text during IME composition (not yet sent to the PTY).
    marked_text: Option<String>,
    broadcast: std::sync::Arc<BroadcastHub>,
    /// Auto-run: (command, interval_secs, send_escape, escape_delay_secs).
    pub auto_run: Option<(String, u32, bool, u32)>,
    auto_run_tick: u32,
    /// Last time PTY output arrived (drives the buddy quiet-detection).
    pub last_activity: std::time::Instant,
    /// Pane origin in window coordinates (for mouse cell math), updated from
    /// the measuring canvas via `pending_bounds` on each pump tick.
    origin: (Pixels, Pixels),
    /// (origin_x, origin_y, width, height) written during prepaint by the
    /// measuring canvas; applied outside the render pass.
    pending_bounds: std::sync::Arc<std::sync::Mutex<Option<MeasuredBounds>>>,
    /// Resize debounce: the size waiting to be applied and when it last
    /// changed, plus when/what was last delivered to the PTY.
    resize_candidate: Option<(Pixels, Pixels, std::time::Instant)>,
    last_resize_applied: std::time::Instant,
    last_applied_size: Option<(Pixels, Pixels)>,
}

const PADDING: f32 = 6.0;

impl TerminalPane {
    pub fn new(
        id: String,
        working_directory: Option<std::path::PathBuf>,
        theme: &'static Theme,
        font_family: String,
        font_size: f32,
        broadcast: std::sync::Arc<BroadcastHub>,
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
                    // for mouse math, size for the PTY grid. Notify so the
                    // queued resize is applied by the next sync even when the
                    // terminal is otherwise idle.
                    let measured = pane.pending_bounds.lock().unwrap().take();
                    if let Some((x, y, w, h)) = measured {
                        pane.origin = (x, y);
                        // Coalesce resizes: every PTY resize SIGWINCHes the
                        // foreground app into a full re-render, and inline
                        // TUIs (claude) leave a stale copy in scrollback per
                        // repaint during live drags. Leading edge keeps
                        // one-shot changes (fullscreen toggle) instant;
                        // continuous changes wait until the size settles.
                        let now = std::time::Instant::now();
                        let already_applied = pane.last_applied_size == Some((w, h))
                            && pane.resize_candidate.is_none();
                        let same_candidate = pane
                            .resize_candidate
                            .map(|(cw, ch, _)| cw == w && ch == h)
                            .unwrap_or(false);
                        if !already_applied && !same_candidate {
                            let calm = now.duration_since(pane.last_resize_applied)
                                >= Duration::from_millis(400);
                            if calm && pane.resize_candidate.is_none() {
                                pane.last_resize_applied = now;
                                pane.last_applied_size = Some((w, h));
                                pane.resize_to(w, h);
                                cx.notify();
                            } else {
                                pane.resize_candidate = Some((w, h, now));
                            }
                        }
                    }
                    if let Some((w, h, since)) = pane.resize_candidate {
                        if since.elapsed() >= Duration::from_millis(150) {
                            pane.resize_candidate = None;
                            pane.last_resize_applied = std::time::Instant::now();
                            pane.last_applied_size = Some((w, h));
                            pane.resize_to(w, h);
                            cx.notify();
                        }
                    }
                    // Auto-run: fire the command every interval (ticks are
                    // ~16ms). ESC lands escape_delay into each cycle without
                    // stretching the cycle. Writes go straight to this pane's
                    // PTY — a timer must never fan out over broadcast.
                    if let Some((command, interval, send_escape, escape_delay)) =
                        pane.auto_run.clone()
                    {
                        pane.auto_run_tick += 1;
                        let interval_ticks = interval.max(1) * 62;
                        let escape_ticks = (escape_delay.max(1) * 62).min(interval_ticks - 1);
                        if send_escape && pane.auto_run_tick == escape_ticks {
                            pane.write_self(vec![0x1b]);
                        }
                        if pane.auto_run_tick >= interval_ticks {
                            pane.auto_run_tick = 0;
                            let mut bytes = command.into_bytes();
                            bytes.push(b'\r');
                            pane.write_self(bytes);
                        }
                    }
                    // Cursor blink: ~530ms phase flip while focused.
                    pane.blink_tick += 1;
                    if pane.blink_tick >= 33 {
                        pane.blink_tick = 0;
                        pane.blink_on = !pane.blink_on;
                        cx.notify();
                    }
                    if pane.session.as_ref().is_some_and(|s| s.take_dirty()) {
                        pane.last_activity = std::time::Instant::now();
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
            alt_screen: false,
            focused_title: None,
            exited: None,
            selection_text: None,
            search_matches: Vec::new(),
        };

        if let Some(session) = &session {
            broadcast_register(&broadcast, &id, session.input_sender());
        }

        Self {
            id,
            session,
            snapshot,
            focus_handle: cx.focus_handle(),
            theme,
            font_family: font_family.into(),
            resolved_family: None,
            font_size,
            translucent: false,
            cell_width,
            line_height,
            selecting: false,
            blink_on: true,
            blink_tick: 0,
            marked_text: None,
            broadcast,
            auto_run: None,
            auto_run_tick: 0,
            last_activity: std::time::Instant::now(),
            origin: (px(0.0), px(0.0)),
            pending_bounds: std::sync::Arc::new(std::sync::Mutex::new(None)),
            resize_candidate: None,
            last_resize_applied: std::time::Instant::now(),
            last_applied_size: None,
        }
    }

    pub fn set_appearance(
        &mut self,
        theme: &'static Theme,
        font_family: &str,
        font_size: f32,
        translucent: bool,
        cx: &mut Context<Self>,
    ) {
        self.translucent = translucent;
        self.theme = theme;
        self.font_family = font_family.to_string().into();
        self.resolved_family = None; // re-resolve on next render
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

    /// Type text into this pane's PTY (bypasses broadcast).
    pub fn send_text(&self, text: &str) {
        self.write_self(text.as_bytes().to_vec());
    }

    /// A live shell is attached (spawned successfully and not exited).
    pub fn has_live_shell(&self) -> bool {
        self.session.as_ref().is_some_and(|s| !s.is_exited())
    }

    /// Cheap busy probe (no cwd lookup) for the always-on cue poll.
    pub fn foreground_busy(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.foreground_busy())
    }

    /// (cwd, foreground-job-running) in one probe.
    pub fn status(&self) -> (Option<String>, bool) {
        self.session
            .as_ref()
            .map(|s| s.status())
            .unwrap_or((None, false))
    }

    pub fn focus(&self, window: &mut Window) {
        self.focus_handle.focus(window);
    }

    /// Begin teardown; the returned handle must be joined off the UI thread.
    pub fn shutdown(&mut self) -> Option<ShutdownHandle> {
        self.broadcast.members.lock().unwrap().remove(&self.id);
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
        if self.broadcast.is_enabled() && self.broadcast.is_member(&self.id) {
            for (on, sender) in self.broadcast.members.lock().unwrap().values() {
                if *on {
                    let _ = sender.send(Msg::Input(bytes.clone().into()));
                }
            }
            return;
        }
        self.write_self(bytes);
    }

    /// Write to this pane's PTY only, ignoring broadcast (timers, escapes).
    fn write_self(&self, bytes: Vec<u8>) {
        if let Some(session) = &self.session {
            session.write(bytes);
        }
    }

    pub fn set_search(&mut self, needle: Option<&str>, cx: &mut Context<Self>) {
        if let Some(session) = self.session.as_mut() {
            session.set_search(needle);
        }
        cx.notify();
    }

    pub fn search_next(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self.session.as_mut() {
            session.search_jump_next();
        }
        cx.notify();
    }

    /// Visible rows as plain text (buddy review context).
    pub fn visible_text(&self) -> String {
        self.snapshot
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|cell| cell.ch)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn set_auto_run(&mut self, config: Option<(String, u32, bool, u32)>) {
        self.auto_run = config;
        self.auto_run_tick = 0;
        // First run fires immediately (old-app behavior); the tick loop
        // handles every repeat after this.
        if let Some((command, _, _, _)) = &self.auto_run {
            let mut bytes = command.clone().into_bytes();
            bytes.push(b'\r');
            self.write_self(bytes);
        }
    }

    fn measure_cell(&mut self, window: &mut Window, cx: &mut App) {
        // Pick the first INSTALLED family from the fallback chain that is
        // actually MONOSPACE, then measure by shaping through the same
        // pipeline that renders rows — measuring a different font than the
        // one that draws (or letting a proportional font in at all) is
        // exactly how the cursor drifts away from the text.
        if self.resolved_family.is_none() {
            let available = window.text_system().all_font_names();
            let preferred = self.font_family.to_string();
            let resolved = [preferred.as_str(), "Menlo", "Monaco"]
                .into_iter()
                .find(|candidate| {
                    available.iter().any(|name| name == candidate)
                        && Self::family_is_monospace(candidate, self.font_size, window)
                })
                .unwrap_or("Menlo")
                .to_string();
            self.resolved_family = Some(resolved.into());
        }
        let family = self
            .resolved_family
            .clone()
            .unwrap_or_else(|| "Menlo".into());
        let text: SharedString = "MMMMMMMMMM".into();
        let run = gpui::TextRun {
            len: text.len(),
            font: gpui::font(family),
            color: gpui::Hsla::default(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let shaped = window
            .text_system()
            .shape_line(text, px(self.font_size), &[run], None);
        if f32::from(shaped.width) > 0.0 {
            self.cell_width = shaped.width / 10.0;
        }
        self.line_height = px((self.font_size * 1.4).round());
        let _ = cx;
    }

    /// True when wide and narrow probe strings shape to (nearly) the same
    /// width — the property terminal grid math depends on.
    pub fn family_is_monospace(family: &str, font_size: f32, window: &mut Window) -> bool {
        let shape = |text: &'static str| -> f32 {
            let text: SharedString = text.into();
            let run = gpui::TextRun {
                len: text.len(),
                font: gpui::font(family.to_string()),
                color: gpui::Hsla::default(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            f32::from(
                window
                    .text_system()
                    .shape_line(text, px(font_size), &[run], None)
                    .width,
            )
        };
        // Several homogeneous probes at a tight tolerance: 'M'-vs-'i' alone
        // admits both proportional coincidences and small per-glyph errors
        // that accumulate across an 80-column row.
        let wide = shape("MMMMMMMMMM");
        if wide <= 0.0 {
            return false;
        }
        ["iiiiiiiiii", "0000000000", "          ", "()[]{};:.,"]
            .into_iter()
            .all(|probe| ((wide - shape(probe)).abs() / wide) < 0.005)
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
                if let Some(text) = self.snapshot.selection_text.clone() {
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

        // Printable input is delivered through the EntityInputHandler (IME
        // path: dead keys, marked text, CJK); key_down handles only chords.
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

fn broadcast_register(hub: &std::sync::Arc<BroadcastHub>, id: &str, sender: EventLoopSender) {
    hub.members
        .lock()
        .unwrap()
        .insert(id.to_string(), (true, sender));
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

/// IME-correct text input: composed text goes straight to the PTY; marked
/// (in-composition) text is held and never sent until commit.
impl gpui::EntityInputHandler for TerminalPane {
    fn text_for_range(
        &mut self,
        _range: std::ops::Range<usize>,
        _adjusted_range: &mut Option<std::ops::Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        None // a terminal has no addressable backing text
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<gpui::UTF16Selection> {
        Some(gpui::UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        self.marked_text
            .as_ref()
            .map(|text| 0..text.encode_utf16().count())
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_text = None;
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = None;
        if !text.is_empty() {
            self.write(text.as_bytes().to_vec());
            self.scroll_to_bottom_on_input(cx);
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<std::ops::Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = Some(new_text.to_string());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: std::ops::Range<usize>,
        element_bounds: gpui::Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<gpui::Bounds<Pixels>> {
        // Anchor the IME candidate window at the cursor cell.
        let row = self.snapshot.cursor.row?;
        let origin = gpui::point(
            element_bounds.origin.x
                + px(PADDING + self.snapshot.cursor.col as f32 * f32::from(self.cell_width)),
            element_bounds.origin.y + px(PADDING + row as f32 * f32::from(self.line_height)),
        );
        Some(gpui::Bounds {
            origin,
            size: gpui::size(self.cell_width, self.line_height),
        })
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
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
        let search_hits: std::collections::HashSet<(usize, usize)> =
            snapshot.search_matches.iter().copied().collect();
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
                if search_hits.contains(&(col_idx, row_idx)) {
                    bg = Some(theme.yellow);
                    fg = theme.background;
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
        let blink_visible = !focused || self.blink_on;
        let cursor_div = match (snapshot.cursor.style, snapshot.cursor.row) {
            (CursorStyle::Hidden, _) | (_, None) => None,
            _ if !blink_visible => None,
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
            .bg(if self.translucent {
                // Fully transparent over a background image: the image layer
                // already applies the user's chosen opacity, so any alpha here
                // would dim it a second time.
                gpui::rgba(0x0000_0000)
            } else {
                gpui::rgba((theme.background << 8) | 0xFF)
            })
            .p(px(PADDING))
            .overflow_hidden()
            .font_family(
                self.resolved_family
                    .clone()
                    .unwrap_or_else(|| self.font_family.clone()),
            )
            .text_size(px(self.font_size))
            .line_height(line_h)
            .text_color(rgb(theme.foreground))
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.focus(window);
                    cx.emit(PaneEvent::Focused);
                    let m = &event.modifiers;
                    if m.platform {
                        // Cmd+click: open the URL under the pointer, if any
                        // (parity with the old app's web-links handler).
                        let (col, row) = this.cell_at(
                            event.position.x,
                            event.position.y,
                            this.last_origin_x(),
                            this.last_origin_y(),
                        );
                        if let Some(url) = this.url_at(col, row) {
                            let _ = std::process::Command::new("/usr/bin/open").arg(url).spawn();
                        }
                        return;
                    }
                    if m.control || m.shift {
                        return; // reserved-modifier clicks never reach the PTY
                    }
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
                let entity = cx.entity();
                let ime_focus = self.focus_handle.clone();
                gpui::canvas(
                    move |bounds, _window, _cx| {
                        *pending.lock().unwrap() = Some((
                            bounds.origin.x,
                            bounds.origin.y,
                            bounds.size.width,
                            bounds.size.height,
                        ));
                        bounds
                    },
                    move |bounds, _, window, cx| {
                        // Contract rev 2: printable text flows through the
                        // platform IME pipeline, not key_down.
                        window.handle_input(
                            &ime_focus,
                            gpui::ElementInputHandler::new(bounds, entity.clone()),
                            cx,
                        );
                    },
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

    /// URL spanning the given cell, if any: scans the row for http(s):// or
    /// www. runs delimited by whitespace/quotes, trimming trailing
    /// punctuation the way the old web-links matcher did.
    fn url_at(&self, col: usize, row: usize) -> Option<String> {
        let cells = self.snapshot.rows.get(row)?;
        let text: String = cells.iter().map(|cell| cell.ch).collect();
        let chars: Vec<char> = text.chars().collect();
        let is_break = |c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`');
        let mut start = 0;
        while start < chars.len() {
            while start < chars.len() && is_break(chars[start]) {
                start += 1;
            }
            let mut end = start;
            while end < chars.len() && !is_break(chars[end]) {
                end += 1;
            }
            if start < end && col >= start && col < end {
                let mut token: String = chars[start..end].iter().collect();
                while token.ends_with([')', ']', '.', ',', ';', ':', '!', '?']) {
                    token.pop();
                }
                if token.starts_with("http://") || token.starts_with("https://") {
                    return Some(token);
                }
                if token.starts_with("www.") {
                    return Some(format!("https://{token}"));
                }
                return None;
            }
            start = end;
        }
        None
    }

    fn handle_click(&mut self, col: usize, row: usize, cx: &mut Context<Self>) {
        let snapshot = &self.snapshot;
        // Click-to-move guards (ported from the web app): prompt row only, at
        // bottom, no selection, no app mouse tracking, normal buffer implied
        // by mouse_tracking check + display_offset.
        if !snapshot.mouse_tracking
            && !snapshot.alt_screen
            && snapshot.display_offset == 0
            && snapshot.selection.is_empty()
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
