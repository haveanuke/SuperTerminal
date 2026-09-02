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

use crate::companion::wire::{WireRun, WireSnapshot};
use crate::hosts::Target;
use crate::keys::{self, KeyInput};
use alacritty_terminal::event_loop::{EventLoopSender, Msg};
use superterminal_core::activity::Activity;

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
    /// Where this pane's shell runs. Local panes behave exactly as before;
    /// a non-local target with no session is a dead pane (Slice 1 does not
    /// connect anywhere yet).
    target: Target,
    session: Option<TermSession>,
    snapshot: RenderableSnapshot,
    /// Latest frame received from an attached peer. `None` for every local
    /// pane, always — `render()` takes the exact pre-Task-3
    /// `local_paint_frame` path whenever this is `None`, so a local pane's
    /// output is untouched. Nothing sets this to `Some` yet: a later task
    /// populates it from the attachment's own `.latest()` snapshot.
    attached_frame: Option<std::sync::Arc<WireSnapshot>>,
    /// Rows scrolled back from an attached peer's live bottom, LOCAL to
    /// this viewer only. Never sent to the broadcaster: D2 makes geometry
    /// broadcaster-owned, so scrolling an attached pane can only change
    /// which slice of the RECEIVED scrollback this viewer currently shows,
    /// never the remote PTY. Meaningless (and unread) while `attached_frame`
    /// is `None`.
    attached_scroll_offset: usize,
    /// Sub-line remainder carried between wheel events, so a slow trackpad
    /// gesture accumulates into a line instead of rounding to nothing on
    /// every event. See [`scroll_lines_from_delta`].
    scroll_accum: f32,
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
    /// Shaped advance per non-ASCII (char, bold, italic) seen on screen —
    /// keyed by style because a bold/italic fallback face can carry a
    /// different advance than the upright one. The coalescer only groups
    /// glyphs whose advance matches their grid span. Cleared when metrics
    /// change.
    advance_cache: std::collections::HashMap<(char, bool, bool), f32>,
    /// Cell metrics are valid for the current family + size; render skips
    /// the per-frame probe shaping until appearance changes invalidate it.
    metrics_measured: bool,
    /// The terminal grid may differ from `snapshot`: PTY output arrived
    /// (pump sets this on dirty) or search state was mutated directly.
    /// Together with `has_pending_ops` this is the ONLY reason render
    /// re-syncs — blink/focus frames reuse the snapshot untouched.
    snapshot_stale: bool,
    /// The snapshot changed (or metrics did) since the last glyph-advance
    /// scan; blink/selection frames skip the full-grid walk.
    advance_scan_pending: bool,
    selecting: bool,
    /// Live pointer position (window coords) during a selection drag; the
    /// pump auto-scrolls while it sits past the pane's vertical edge.
    drag_position: Option<(Pixels, Pixels)>,
    drag_scroll_tick: u8,
    /// Freshly measured pane size for drag edge math — unlike
    /// `last_applied_size` it is NOT held back by the resize debounce.
    measured_size: Option<(Pixels, Pixels)>,
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
    /// Last time the user sent input to this pane (typing without echo —
    /// password prompts — produces no output; this keeps the buddy quiet
    /// gate honest there).
    pub last_input: std::time::Instant,
    /// "User is looking at this pane" (pane focused AND window active),
    /// cached each render so bell handling can judge attention AT ARRIVAL
    /// rather than ~a second later on the cue tick.
    attended: bool,
    /// An unattended bell arrived since the cue tick last drained it.
    bell_pending: bool,
    /// Companion hub while the phone server runs (None = zero-cost off).
    companion: Option<std::sync::Arc<crate::companion::hub::Hub>>,
    /// Last hub generation this pane force-published for (server start).
    companion_generation: u64,
    /// Set when the pump already synced the snapshot this frame — render
    /// then consumes the cache instead of syncing again.
    snapshot_fresh: bool,
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

        Self::from_parts(
            id,
            Target::Local,
            session,
            cell_width,
            line_height,
            theme,
            font_family,
            font_size,
            broadcast,
            cx,
        )
    }

    /// A pane for a target that isn't (yet, or no longer) reachable: never
    /// attempts a local shell, unlike `new`. Renders and tears down exactly
    /// like a pane whose local spawn failed — the existing, already-safe
    /// "no session" path. Slice 2 gives a resolved remote target a real
    /// connection; here every non-local target restores dead.
    pub fn dead(
        id: String,
        target: Target,
        theme: &'static Theme,
        font_family: String,
        font_size: f32,
        broadcast: std::sync::Arc<BroadcastHub>,
        cx: &mut Context<Self>,
    ) -> Self {
        let cell_width = px(font_size * 0.6);
        let line_height = px((font_size * 1.4).round());
        Self::from_parts(
            id,
            target,
            None,
            cell_width,
            line_height,
            theme,
            font_family,
            font_size,
            broadcast,
            cx,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn from_parts(
        id: String,
        target: Target,
        session: Option<TermSession>,
        cell_width: Pixels,
        line_height: Pixels,
        theme: &'static Theme,
        font_family: String,
        font_size: f32,
        broadcast: std::sync::Arc<BroadcastHub>,
        cx: &mut Context<Self>,
    ) -> Self {
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
                        pane.measured_size = Some((w, h));
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
                    // Selection drag held past the top/bottom edge auto-scrolls
                    // the viewport (parity with xterm.js in the old app). The
                    // buffer-anchored selection start stays put; the end tracks
                    // the revealed edge row. gpui mouse-move listeners are
                    // hover-gated, so outside the pane the pointer is tracked
                    // by render reading window.mouse_position() — the
                    // every-tick notify below keeps that loop turning for the
                    // drag's duration. Scrolls run every 4th tick (~64ms) so
                    // speed is governed by drag_scroll_lines, not pump rate.
                    if pane.selecting {
                        pane.drag_scroll_tick = pane.drag_scroll_tick.wrapping_add(1);
                        if let (Some((drag_x, drag_y)), Some((_, height))) =
                            (pane.drag_position, pane.measured_size)
                        {
                            if pane.drag_scroll_tick.is_multiple_of(4) {
                                let top = f32::from(pane.origin.1) + PADDING;
                                let bottom = f32::from(pane.origin.1) + f32::from(height) - PADDING;
                                let lines = drag_scroll_lines(f32::from(drag_y), top, bottom);
                                if lines != 0 {
                                    let (col, _) = pane.cell_at(
                                        drag_x,
                                        drag_y,
                                        pane.last_origin_x(),
                                        pane.last_origin_y(),
                                    );
                                    let row = if lines > 0 {
                                        0
                                    } else {
                                        pane.snapshot.lines.saturating_sub(1)
                                    };
                                    // Unreachable for an attached pane, and
                                    // structurally so: `selecting` is the
                                    // only way in, and `click_gesture`
                                    // refuses to set it while attached
                                    // (`set_attached_frame` also clears it).
                                    if let Some(session) = pane.session.as_mut() {
                                        session.queue_scroll(lines);
                                        session.queue_selection_update(col, row);
                                    }
                                }
                            }
                        }
                        cx.notify();
                    }
                    // Cursor blink: ~530ms phase flip while focused.
                    pane.blink_tick += 1;
                    if pane.blink_tick >= 33 {
                        pane.blink_tick = 0;
                        pane.blink_on = !pane.blink_on;
                        cx.notify();
                    }
                    let dirty = pane.session.as_ref().is_some_and(|s| s.take_dirty());
                    if dirty {
                        pane.last_activity = std::time::Instant::now();
                        pane.snapshot_stale = true;
                        pane.process_events(cx);
                        cx.notify();
                    }
                    // Companion publish: on fresh output, or once per hub
                    // generation bump (server start needs idle grids too).
                    if let Some(hub) = pane.companion.clone() {
                        let generation = hub.generation.load(std::sync::atomic::Ordering::Relaxed);
                        if dirty || generation != pane.companion_generation {
                            pane.companion_generation = generation;
                            // D4, said out loud. `session` being `None` on an
                            // attached pane is what stops it re-publishing a
                            // terminal it does not own — an accident that
                            // happens to enforce the rule. State the rule so
                            // a later change cannot quietly undo it.
                            let publishable = may_publish_to_companion(
                                pane.attached_frame.is_some(),
                                pane.session.is_some(),
                            );
                            if let Some(session) = pane.session.as_mut().filter(|_| publishable) {
                                let (display, live) = session.sync_and_snapshot_with_live();
                                pane.snapshot = display;
                                pane.snapshot_fresh = true;
                                pane.snapshot_stale = false;
                                pane.advance_scan_pending = true;
                                // The phone mirrors the LIVE screen, never
                                // the Mac's scrollback viewport.
                                let published = live.unwrap_or_else(|| pane.snapshot.clone());
                                hub.publish_snapshot(&pane.id, std::sync::Arc::new(published));
                                cx.notify();
                            }
                        }
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
            bracketed_paste: false,
            mouse_tracking: false,
            alt_screen: false,
            focused_title: None,
            exited: None,
            selection_text: None,
            search_matches: Vec::new(),
            history_rows: Vec::new(),
        };

        if let Some(session) = &session {
            if may_broadcast_locally(&target) {
                broadcast_register(&broadcast, &id, session.input_sender());
            }
        }

        Self {
            id,
            target,
            session,
            snapshot,
            attached_frame: None,
            attached_scroll_offset: 0,
            scroll_accum: 0.0,
            focus_handle: cx.focus_handle(),
            theme,
            font_family: font_family.into(),
            resolved_family: None,
            font_size,
            translucent: false,
            cell_width,
            line_height,
            advance_cache: std::collections::HashMap::new(),
            metrics_measured: false,
            advance_scan_pending: true,
            snapshot_stale: true,
            selecting: false,
            drag_position: None,
            drag_scroll_tick: 0,
            measured_size: None,
            blink_on: true,
            blink_tick: 0,
            marked_text: None,
            broadcast,
            auto_run: None,
            auto_run_tick: 0,
            last_activity: std::time::Instant::now(),
            last_input: std::time::Instant::now(),
            attended: false,
            bell_pending: false,
            companion: None,
            companion_generation: 0,
            snapshot_fresh: false,
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
        self.advance_cache.clear();
        self.metrics_measured = false;
        self.advance_scan_pending = true;
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

    /// Where this pane's shell runs. Local panes behave exactly as before.
    pub fn target(&self) -> &Target {
        &self.target
    }

    /// Type text into this pane's PTY (bypasses broadcast).
    pub fn send_text(&self, text: &str) {
        self.write_self(text.as_bytes().to_vec());
    }

    /// A live shell sits behind this pane (spawned successfully and not
    /// exited) — or, for an attached pane, a shell on ANOTHER machine that
    /// the attachment is still delivering frames from. See [`shell_is_live`].
    pub fn has_live_shell(&self) -> bool {
        shell_is_live(
            self.attached_frame.is_some(),
            self.session.as_ref().is_some_and(|s| !s.is_exited()),
        )
    }

    /// Cheap busy probe (no cwd lookup) for the always-on cue poll.
    pub fn foreground_busy(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.foreground_busy())
    }

    /// Tri-state foreground probe: cues, `finished`, caffeinate, sidebar
    /// dots, and the folder-write guard all read THIS, not the boolean.
    ///
    /// With no session, the answer depends on `target`: a local pane with
    /// no session is definitively `Idle` (the shell never started, or
    /// exited); a remote pane with no session — e.g. a restored dead
    /// `Target::Remote` pane, which has no telemetry — is `Unknown`. See
    /// [`crate::hosts::pane_activity`].
    pub fn foreground_activity(&self) -> Activity {
        let session_activity = self.session.as_ref().map(|s| s.foreground_activity());
        crate::hosts::pane_activity(&self.target, session_activity)
    }

    /// The phone's busy dot. A foreground app alone is not "working" —
    /// claude parked at its prompt owns the tty for hours, which painted
    /// every session orange — but output activity alone is not "working"
    /// either: a terminal blocked on a long silent job goes quiet and read
    /// idle. Neither is knowable from the pty, so instrumented agents
    /// report their own state through the adapters' lifecycle hooks and
    /// that wins; the output window survives only as the fallback for
    /// uninstrumented foregrounds. See [`busy_dot`].
    pub fn companion_busy(&self) -> bool {
        let (agent, pgid) = match self.session.as_ref() {
            Some(session) => (session.agent_state(), session.foreground_pgid()),
            None => (None, 0),
        };
        busy_dot(
            self.foreground_busy(),
            self.last_activity.elapsed(),
            agent,
            pgid,
        )
    }

    /// Tri-state form of the phone's dot. Local behaviour is unchanged:
    /// the agent/output heuristic in [`busy_dot`] still decides, and is
    /// deliberately NOT merged with [`Self::foreground_activity`].
    ///
    /// With no local PTY there is no heuristic to run, and `busy_dot`
    /// answering `false` is the ABSENCE of a signal, not the observation of
    /// a prompt — so the probe is offered as `None` rather than as
    /// `Some(false)`. See [`companion_activity_of`].
    pub fn companion_activity(&self) -> Activity {
        companion_activity_of(
            &self.target,
            self.session.as_ref().map(|_| self.companion_busy()),
        )
    }

    /// Tri-state (cwd, foreground-job-running) probe. With no session the
    /// cwd is `None` either way; the activity half follows `target` — see
    /// [`Self::foreground_activity`] and [`crate::hosts::pane_activity`].
    pub fn status_activity(&self) -> (Option<String>, Activity) {
        match self.session.as_ref() {
            Some(session) => session.status_activity(),
            None => (None, crate::hosts::pane_activity(&self.target, None)),
        }
    }

    pub fn focus(&self, window: &mut Window) {
        self.focus_handle.focus(window);
    }

    /// Attach/detach the companion hub (attaching also forces a publish on
    /// the next pump tick via the generation check).
    pub fn set_companion(&mut self, hub: Option<std::sync::Arc<crate::companion::hub::Hub>>) {
        self.companion_generation = 0;
        self.companion = hub;
    }

    pub fn input_sender(&self) -> Option<alacritty_terminal::event_loop::EventLoopSender> {
        self.session.as_ref().map(|s| s.input_sender())
    }

    /// Begin teardown; the returned handle must be joined off the UI thread.
    pub fn shutdown(&mut self) -> Option<ShutdownHandle> {
        if let Some(hub) = self.companion.take() {
            // 410 for in-flight phone input; the workspace sweep removes
            // the entry (and ends streams) on its next tick.
            hub.retire(&self.id);
        }
        self.broadcast.members.lock().unwrap().remove(&self.id);
        // An attached pane has no PTY to reap, so `None` here is right — but
        // it does still hold the last frame it received. Dropping it stops a
        // torn-down pane painting a terminal on another machine, and resets
        // the viewer's scroll window so a rebuilt pane starts at the live
        // bottom. Closing the attachment's own stream lands with the
        // `Attachment` field in Task 5.
        self.attached_frame = None;
        self.attached_scroll_offset = 0;
        self.session.take().map(TermSession::shutdown)
    }

    /// Hand this pane the newest frame from its attachment. Task 5 calls
    /// this from the attachment's own thread hop; nothing calls it yet.
    ///
    /// This is where an attached pane's FRESHNESS comes from. The pump's
    /// `take_dirty()` cannot supply it — there is no local grid to go dirty,
    /// so it answers "never dirty" forever — and it must keep answering that,
    /// because `dirty` also drives `process_events` and the companion
    /// publish. Refresh for an attached pane is therefore pushed by the
    /// arriving frame, not polled off a local session.
    #[allow(dead_code)] // the attachment that calls this arrives in Task 5
    pub fn set_attached_frame(
        &mut self,
        frame: std::sync::Arc<WireSnapshot>,
        cx: &mut Context<Self>,
    ) {
        self.attached_scroll_offset = scroll_after_frame(self.attached_scroll_offset, &frame);
        self.attached_frame = Some(frame);
        self.last_activity = std::time::Instant::now();
        // A selection drag can only ever have been started on a LOCAL grid
        // (`click_gesture` refuses one while attached); becoming attached
        // invalidates any in-flight drag rather than leaving the pump
        // auto-scrolling off stale state.
        self.selecting = false;
        self.drag_position = None;
        cx.notify();
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
                SessionEvent::Bell => {
                    // Attention judged at arrival: a bell in the pane the
                    // user is watching is theirs (readline beeps, less at
                    // EOF) — only unattended bells become cues.
                    if !self.attended {
                        self.bell_pending = true;
                    }
                }
            }
        }
    }

    /// Drain the pending-bell flag (cue tick). Callers discard the value
    /// when audio cues are off — bells are never queued for later.
    pub fn take_bell(&mut self) -> bool {
        std::mem::take(&mut self.bell_pending)
    }

    pub fn write(&mut self, bytes: Vec<u8>) {
        self.last_input = std::time::Instant::now();
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

    /// Write to this pane's own terminal only, ignoring broadcast (timers,
    /// escapes). See [`input_route`] for why the destination is decided
    /// rather than inferred from "there happens to be a session".
    fn write_self(&self, bytes: Vec<u8>) {
        match input_route(self.attached_frame.is_some(), self.session.is_some()) {
            InputRoute::LocalPty => {
                if let Some(session) = &self.session {
                    session.write(bytes);
                }
            }
            // Dropped DELIBERATELY, not incidentally: there is no route to
            // the remote PTY yet, and writing these into the local shell
            // would type into the wrong machine. Task 5 replaces this arm
            // with `Attachment::send`, hopped off the gpui path (`send()`
            // blocks up to 5s).
            InputRoute::Peer => {}
            // No shell at all: the spawn failed, or the process has exited.
            InputRoute::Nowhere => {}
        }
    }

    pub fn set_search(&mut self, needle: Option<&str>, cx: &mut Context<Self>) {
        if let Some(session) = self.session.as_mut() {
            session.set_search(needle);
        }
        // Search mutates the session directly (no queued op); the next
        // render must re-sync to surface the new matches.
        self.snapshot_stale = true;
        cx.notify();
    }

    pub fn search_next(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self.session.as_mut() {
            session.search_jump_next();
        }
        self.snapshot_stale = true;
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
        // Metrics only move when appearance does; shaping the probe every
        // frame (render runs on each blink/output notify) is pure waste.
        if self.metrics_measured && self.resolved_family.is_some() {
            return;
        }
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
            let measured = shaped.width / 10.0;
            if measured != self.cell_width {
                self.advance_cache.clear();
                self.advance_scan_pending = true;
            }
            self.cell_width = measured;
            // Only a positive measurement is worth caching — a zero-width
            // probe (text system not ready) must retry next frame.
            self.metrics_measured = true;
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
}

/// Output silence longer than this reads as "not working" for the phone's
/// busy dot, even while an app owns the tty.
pub const COMPANION_BUSY_WINDOW: std::time::Duration = std::time::Duration::from_secs(4);

/// The busy-dot predicate, pure for testing.
///
/// Precedence, strongest evidence first:
/// 1. Nothing owns the tty -> idle. The shell prompt is unambiguous.
/// 2. An INSTRUMENTED agent owns it -> believe the agent. Its own lifecycle
///    hooks are the only source that knows the difference between "thinking"
///    and "waiting for you"; the pty cannot see it, and no macOS API can
///    recover it after the fact.
/// 3. Otherwise fall back to recent output. This is a guess, and a knowingly
///    imperfect one: a silent long job reads idle. It only applies to
///    uninstrumented foregrounds.
///
/// Known limit: claude runs no hook after a USER INTERRUPT (Ctrl-C), so the
/// state stays `working`. The next prompt does NOT clear it — that fires
/// `UserPromptSubmit`, which sets working again — so the dot stays busy
/// until the END of that next turn, or until the session exits or a new
/// claude launches and truncates the file. This IS stickier than the old
/// behavior, where the output heuristic fell idle after four seconds; it
/// errs toward false-busy rather than false-idle, which is the direction
/// this feature exists to correct. Detecting the interrupt from the byte
/// stream was tried and removed: pasted text legitimately contains 0x03,
/// and clearing on a paste would make a WORKING agent read idle.
///
/// The agent's pid must match the process group that owns the tty, or the
/// file is a leftover from an agent that already exited and we ignore it.
fn busy_dot(
    foreground_busy: bool,
    since_output: std::time::Duration,
    agent: Option<(crate::term_session::AgentState, i32)>,
    foreground_pgid: i32,
) -> bool {
    if !foreground_busy {
        return false;
    }
    if let Some((state, pid)) = agent {
        if pid == foreground_pgid {
            return state == crate::term_session::AgentState::Working;
        }
    }
    since_output < COMPANION_BUSY_WINDOW
}

/// Whole lines to scroll for one wheel event, plus the fraction to carry
/// into the next one.
///
/// The carry is the entire point. macOS sends PRECISE pixel deltas for a
/// trackpad gesture — often 1-8px per event — while a line is
/// `font_size * 1.4`, i.e. 20px at the default size. Rounding each event on
/// its own therefore produced 0 for every event of a gentle two-finger
/// scroll, and discarding the remainder meant those events summed to
/// nothing no matter how long the gesture ran. A mouse wheel was unaffected
/// because macOS sends one large delta per notch, which is why this looked
/// like a per-machine quirk rather than a bug.
///
/// `trunc` rather than `round`, so the retained fraction always points the
/// same way as the motion that produced it; rounding would let a reversal
/// strand a fraction pointing the wrong way and lose a line.
fn scroll_lines_from_delta(accum: f32, delta_px: f32, line_height: f32) -> (i32, f32) {
    // Before the first layout `line_height` is 0. Dividing by it yields inf
    // or NaN, and a NaN carry would poison every later event — the pane
    // would never scroll again for as long as it lived.
    if !(line_height > 0.0) || !delta_px.is_finite() || !accum.is_finite() {
        return (0, if accum.is_finite() { accum } else { 0.0 });
    }
    let total = accum + delta_px / line_height;
    let lines = total.trunc();
    (lines as i32, total - lines)
}

/// Lines to scroll per auto-scroll step while a selection drag sits past the
/// pane's vertical edge (positive = toward history, matching `queue_scroll`).
/// Speed scales with how far past the edge the pointer is, capped at 5.
fn drag_scroll_lines(pointer_y: f32, top: f32, bottom: f32) -> i32 {
    let (overshoot, sign) = if pointer_y < top {
        (top - pointer_y, 1)
    } else if pointer_y > bottom {
        (pointer_y - bottom, -1)
    } else {
        return 0;
    };
    sign * (1 + (overshoot / 40.0) as i32).min(5)
}

impl TerminalPane {
    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        let m = &ks.modifiers;

        // Restarting a dead pane: any key on an exited pane asks the
        // workspace to close it (contract rev 1, shutdown section). Routed
        // through the same predicate as the overlay that advertises it, so
        // the two can never disagree about whether this pane has died.
        if exit_notice(
            self.attached_frame.is_some(),
            self.snapshot.exited.is_some(),
        ) == ExitNotice::LocalProcessExited
        {
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
            || keys::is_function_key(ks.key.as_str())
        {
            if m.platform && ks.key == "c" {
                if let Some(text) = self.snapshot.selection_text.clone() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    return;
                }
            }
            if m.platform && ks.key == "v" {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    // Same framing as phone Send: paste-aware TUIs must see
                    // a paste, not a timing-dependent burst of keystrokes.
                    let bytes =
                        crate::companion::input::text_bytes(&text, self.snapshot.bracketed_paste);
                    self.write(bytes);
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
        if self.attached_frame.is_some() {
            // The local analogue of the `display_offset` reset below: typing
            // lands at the broadcaster's live bottom, so the viewer's own
            // scrollback window snaps there too. D2 — this changes only
            // which slice THIS viewer paints, never the remote PTY.
            self.attached_scroll_offset = 0;
        } else if self.snapshot.display_offset > 0 {
            if let Some(session) = self.session.as_mut() {
                session.queue_scroll(-(self.snapshot.display_offset as i32));
            }
        }
        cx.notify();
    }
}

fn resolve_fg(color: CellColor, theme: &Theme) -> u32 {
    match color {
        CellColor::Default => theme.foreground,
        CellColor::Indexed(i) => ansi_256(i, theme),
        CellColor::Rgb(r, g, b) => ((r as u32) << 16) | ((g as u32) << 8) | b as u32,
    }
}

fn resolve_bg(color: CellColor, theme: &Theme) -> Option<u32> {
    match color {
        CellColor::Default => None, // pane background shows through
        CellColor::Indexed(i) => Some(ansi_256(i, theme)),
        CellColor::Rgb(r, g, b) => Some(((r as u32) << 16) | ((g as u32) << 8) | b as u32),
    }
}

/// Whether a pane at this target may join the LOCAL keystroke fan-out.
/// `BroadcastHub` membership must be encoded from the pane's target here —
/// at the one place every pane is constructed — never inferred from "the
/// pane has a session". A later phase's attached pane (a local view of a
/// terminal running on another Mac) will also carry a sender, because it
/// forwards keystrokes to its peer; if membership followed that structural
/// fact alone, enabling local broadcast would fan your keystrokes into a
/// terminal on a different machine. Same reasoning as `hub::Origin` on the
/// companion hub, stated before Phase C2 exists to be swept in by it.
fn may_broadcast_locally(target: &Target) -> bool {
    target.is_local()
}

fn broadcast_register(hub: &std::sync::Arc<BroadcastHub>, id: &str, sender: EventLoopSender) {
    hub.members
        .lock()
        .unwrap()
        .insert(id.to_string(), (true, sender));
}

// ---------------------------------------------------------------------------
// Answering honestly with no local session.
//
// An ATTACHED pane (`attached_frame` populated) is a view of a terminal
// running on another machine: no PTY, no local process, and a `snapshot`
// that is still the empty placeholder `from_parts` built. Every predicate
// below exists because the naive answer — whatever `Option::None` happens to
// produce — is PLAUSIBLE and WRONG at that pane, and a plausible wrong answer
// does not fail; it quietly misleads a consumer.
//
// Each takes `attached` explicitly rather than reading it off the pane, so
// the decision is testable without a gpui harness (there is none, and none
// may be introduced).
// ---------------------------------------------------------------------------

/// Whether this pane may publish its own screen to THIS Mac's companion hub
/// (the phone).
///
/// D4. Today the pump enforces this by accident — its publish arm is
/// `if let Some(session) = pane.session.as_mut()`, and an attached pane has
/// no session — so the rule holds for a reason that has nothing to do with
/// the rule. Stated here so it survives: an attached pane is a VIEW of
/// another machine's terminal, and re-publishing it would offer the phone a
/// remote view of a remote view, attributed to this Mac. That must stay true
/// even if an attached pane ever acquires a session for some other purpose.
fn may_publish_to_companion(attached: bool, has_local_session: bool) -> bool {
    has_local_session && !attached
}

/// Where a pane's input bytes go.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum InputRoute {
    /// This pane's own PTY.
    LocalPty,
    /// A terminal on another machine, reached through the attachment.
    Peer,
    /// Nowhere: the shell never started, or has already exited.
    Nowhere,
}

/// Attachment wins over the presence of a local session, always. A pane that
/// is showing another machine's terminal must never type into a shell on
/// this one — that is a wrong-machine keystroke, not a dropped one.
fn input_route(attached: bool, has_local_session: bool) -> InputRoute {
    if attached {
        InputRoute::Peer
    } else if has_local_session {
        InputRoute::LocalPty
    } else {
        InputRoute::Nowhere
    }
}

/// Whether a live shell sits behind this pane.
///
/// For an attached pane that is the ATTACHMENT's liveness, not the absence of
/// a local session: the shell is real, it is simply on another machine.
/// Today "attached" means "a frame has arrived"; Task 6 refines it to
/// "the attachment is still fresh" (`peer_client::attach::Freshness`).
fn shell_is_live(attached: bool, local_shell_live: bool) -> bool {
    attached || local_shell_live
}

/// The phone's busy dot as a tri-state.
///
/// `local_busy` is `Some` exactly when there IS a local PTY to probe. The old
/// shape ran `Activity::from_local_busy(companion_busy())` unconditionally,
/// which turned "no local busy signal at all" into `Idle` — the absence of
/// evidence read as evidence of a prompt, which is the precise failure the
/// tri-state exists to prevent. Routing through `hosts::pane_activity` keeps
/// all three activity accessors on one rule and leaves `None` for Task 6 to
/// replace with the peer's own reported activity.
fn companion_activity_of(target: &Target, local_busy: Option<bool>) -> Activity {
    crate::hosts::pane_activity(target, local_busy.map(Activity::from_local_busy))
}

/// What a plain left click starts.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum ClickGesture {
    /// Click-to-move: encode arrows from the cursor to the clicked cell.
    MoveCursor,
    /// Begin a selection drag.
    StartSelection,
    /// Neither gesture has a correct answer here.
    Ignore,
}

/// Both gestures are computed from the LOCAL grid, and an attached pane's
/// local grid is the empty placeholder — so both must be refused there, not
/// answered from it.
///
/// Click-to-move is the dangerous one: every guard below (`!mouse_tracking`,
/// `!alt_screen`, `display_offset == 0`, no selection) passes on that
/// placeholder, so an attached pane would encode a burst of arrow keys from
/// a phantom cursor at (0, 0) against a phantom width of 80. It is inert
/// today only because `write()` has nowhere to send them; Task 5 gives it
/// somewhere.
///
/// Selection is refused for a different reason: selection does not cross the
/// wire (D5). Refusing it here also keeps `selecting` false on an attached
/// pane, which is what makes the pump's drag auto-scroll and the mouse-move
/// selection update structurally unreachable rather than merely inert.
fn click_gesture(
    attached: bool,
    mouse_tracking: bool,
    alt_screen: bool,
    display_offset: usize,
    has_selection: bool,
) -> ClickGesture {
    if attached {
        return ClickGesture::Ignore;
    }
    if !mouse_tracking && !alt_screen && display_offset == 0 && !has_selection {
        ClickGesture::MoveCursor
    } else {
        ClickGesture::StartSelection
    }
}

/// Whether to draw the "[process exited]" overlay, and say the same thing to
/// `on_key_down`'s "any key closes an exited pane".
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum ExitNotice {
    None,
    LocalProcessExited,
}

/// `snapshot.exited` describes a process on THIS Mac. For an attached pane it
/// is permanently `None` (nothing ever syncs that snapshot), so the overlay
/// would never appear when the REMOTE process exits — a dead terminal looking
/// alive indefinitely. The honest answer is not to substitute the local
/// state: protocol 2 carries no exit signal at all (`companion::wire::WireSnapshot`
/// has no such field), so an attached pane must say NOTHING rather than say
/// the local thing. Task 6 supplies the missing signal from the attachment's
/// own `Freshness`/`Status`, at which point this gains a third state.
fn exit_notice(attached: bool, local_exited: bool) -> ExitNotice {
    if !attached && local_exited {
        ExitNotice::LocalProcessExited
    } else {
        ExitNotice::None
    }
}

/// The colour to draw the cursor overlay in.
///
/// The cursor sits on the BROADCASTER's canvas, so it has to be legible
/// against the broadcaster's background — the same reasoning as D5's
/// translucency ruling, which covers the pane's background but not the
/// overlay drawn on top of it. The wire carries no cursor colour, so the
/// viewer's own is used, nudged clear of the broadcaster's background when
/// it would otherwise vanish into it.
///
/// A LOCAL pane is untouched: `contrast_boost` is deliberately NOT applied
/// there, because a theme whose own cursor sits close to its own background
/// would then be silently recoloured, which is a change to a local terminal.
fn cursor_color(theme_cursor: u32, frame_background: u32, attached: bool) -> u32 {
    if attached {
        crate::themes::contrast_boost(theme_cursor, frame_background)
    } else {
        theme_cursor
    }
}

/// Where an attached pane's cursor is being PAINTED, in viewport cells — the
/// same decision [`attached_paint_frame`] makes, without decoding a row.
///
/// Exists so the IME candidate window anchors at the cursor the user can
/// actually see. Anchoring from `self.snapshot.cursor` instead reads a
/// placeholder that describes nothing, and contradicts Task 3's rule that a
/// scrolled-back attached frame shows no cursor at all.
fn attached_cursor_cell(wire: &WireSnapshot, offset: usize) -> Option<(usize, usize)> {
    if clamp_attached_offset(wire.history.len(), offset) != 0 {
        return None;
    }
    wire.cursor
        .as_ref()
        .map(|cursor| (cursor.col as usize, cursor.row as usize))
}

/// The attached pane's new local scroll offset after a wheel gesture.
///
/// D2: geometry and scrollback are broadcaster-owned, so this moves only
/// which slice of the ALREADY-RECEIVED history this viewer paints — never
/// the remote PTY. The sign convention is inherited from the local path
/// (`queue_scroll(lines)`, positive = back into history) and from
/// [`drag_scroll_lines`], which returns positive when the pointer sits above
/// the top edge.
fn attached_scroll_after_wheel(previous: usize, lines: i32, history_len: usize) -> usize {
    let next = previous as i64 + lines as i64;
    clamp_attached_offset(history_len, next.max(0) as usize)
}

/// Fold an arriving frame into the viewer's own scroll offset.
///
/// The offset is clamped against the NEW frame's history and the clamp is
/// KEPT. That is the deliberate choice: when the broadcaster runs `clear`,
/// history drops to zero and the viewer lands at the live bottom; when
/// history regrows, the viewer stays at the bottom instead of silently
/// springing back to where it was scrolled before, with no user action.
/// Clamping only at paint time would produce that spring-back.
///
/// It clamps against `history` alone — never `history + rows` — because the
/// live window is always exactly `rows.len()` tall (see
/// [`windowed_wire_rows`]), so scrolling back can only reach as far as the
/// history behind it.
fn scroll_after_frame(previous_offset: usize, frame: &WireSnapshot) -> usize {
    clamp_attached_offset(frame.history.len(), previous_offset)
}

/// Scale an 0xRRGGBB color's channels by 2/3 (DIM), never via alpha.
fn dim(color: u32) -> u32 {
    let r = ((color >> 16) & 0xff) * 2 / 3;
    let g = ((color >> 8) & 0xff) * 2 / 3;
    let b = (color & 0xff) * 2 / 3;
    (r << 16) | (g << 8) | b
}

/// One visual run: consecutive cells sharing style + selection state,
/// pinned at its starting grid column so painted x never drifts off-grid.
/// `cells` is the grid span (wide chars count 2), painted as the explicit
/// element width so backgrounds cover exactly their cells. `safe` records
/// whether every glyph's advance was verified to match its grid span.
#[derive(Debug, PartialEq)]
struct Run {
    col: usize,
    cells: usize,
    safe: bool,
    text: String,
    fg: u32,
    bg: Option<u32>,
    bold: bool,
    italic: bool,
    underline: bool,
}

/// Resolved look of one visible cell — selection, search, inverse, dim and
/// hidden already applied by the caller.
#[derive(Clone, Copy, PartialEq)]
struct CellLook {
    fg: u32,
    bg: Option<u32>,
    bold: bool,
    italic: bool,
    underline: bool,
}

/// Group visible cells into paint runs. Beyond style equality, a run only
/// extends while glyph advances are trustworthy per `advance_safe(ch,
/// cells)` — the render pass verifies each glyph's shaped advance against
/// its grid span, so an in-font border like `╭──╮` stays one element while
/// a fallback glyph (U+23BF shapes ~1.7 cells wide via Menlo's fallback)
/// pins alone at its own column and can only misdraw its own span. Column
/// contiguity is also required — a skipped wide-char spacer breaks the
/// one-flowed-cell-per-char assumption. Trailing default-style whitespace
/// runs are trimmed; rows never come back empty so each still occupies one
/// line.
fn coalesce_runs(
    cells: impl Iterator<Item = (usize, char, usize, CellLook)>,
    advance_safe: &dyn Fn(char, usize, &CellLook) -> bool,
    default_fg: u32,
) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for (col, ch, span, look) in cells {
        let safe = advance_safe(ch, span, &look);
        let extends = runs.last().is_some_and(|run| {
            let same_look = run.fg == look.fg
                && run.bg == look.bg
                && run.bold == look.bold
                && run.italic == look.italic
                && run.underline == look.underline;
            same_look && run.safe && safe && col == run.col + run.cells
        });
        if extends {
            let run = runs.last_mut().unwrap();
            run.text.push(ch);
            run.cells += span;
        } else {
            runs.push(Run {
                col,
                cells: span,
                safe,
                text: ch.to_string(),
                fg: look.fg,
                bg: look.bg,
                bold: look.bold,
                italic: look.italic,
                underline: look.underline,
            });
        }
    }
    while runs
        .last()
        .is_some_and(|r| r.bg.is_none() && r.text.trim().is_empty() && runs.len() > 1)
    {
        runs.pop();
    }
    if runs.is_empty() {
        runs.push(Run {
            col: 0,
            cells: 1,
            safe: true,
            text: " ".to_string(),
            fg: default_fg,
            bg: None,
            bold: false,
            italic: false,
            underline: false,
        });
    }
    runs
}

/// Everything the render loop needs for one frame, fully resolved: colors
/// already concrete `0xRRGGBB`, runs already merged, and the cursor already
/// reduced to "where, if anywhere". A local snapshot reaches this shape by
/// resolving `CellColor` through the viewer's theme and running
/// [`coalesce_runs`] ([`local_paint_frame`]); a wire snapshot is already in
/// this shape and is decoded directly ([`wire_paint_frame`]). Once built,
/// painting no longer needs to know which kind of snapshot produced it.
#[derive(Debug, PartialEq)]
struct PaintFrame {
    /// What to paint behind a run that leaves its own `bg` unset: the
    /// viewer's theme background for a local frame, or
    /// `WireSnapshot::background` (Task 1) for a wire frame.
    background: u32,
    rows: Vec<Vec<Run>>,
    /// (col, row) in viewport coordinates, if the cursor should be drawn at
    /// all. Already collapses "hidden style" (local) or "omitted from the
    /// wire" (wire) into one `None` — the caller layers blink/focus on top.
    cursor: Option<(usize, usize)>,
    /// The SHAPE to draw at `cursor`. Carried on the frame rather than read
    /// back off the pane's own snapshot: for a wire frame the pane's
    /// snapshot describes a terminal on another machine's screen, or is
    /// empty, so reading the style from it would draw the local default
    /// regardless of what the broadcaster is actually showing. Meaningless
    /// when `cursor` is `None`.
    cursor_style: CursorStyle,
}

/// Resolve a LOCAL snapshot into a [`PaintFrame`]: theme-resolved colors
/// with selection/search/inverse/dim/hidden applied, then coalesced. This is
/// the exact per-row and per-cursor logic `render()` used to run inline —
/// extracting it changes where the decision lives, not what gets painted,
/// which is also how it becomes testable without a gpui harness.
fn local_paint_frame(
    snapshot: &RenderableSnapshot,
    theme: &Theme,
    advance_safe: &dyn Fn(char, usize, &CellLook) -> bool,
) -> PaintFrame {
    let selection: std::collections::HashSet<(usize, usize)> =
        snapshot.selection.iter().copied().collect();
    let search_hits: std::collections::HashSet<(usize, usize)> =
        snapshot.search_matches.iter().copied().collect();
    let rows = snapshot
        .rows
        .iter()
        .enumerate()
        .map(|(row_idx, row)| {
            let looks = row
                .iter()
                .enumerate()
                .filter(|(_, cell)| !cell.wide_spacer)
                .map(|(col_idx, cell)| {
                    let selected = selection.contains(&(col_idx, row_idx));
                    let style = &cell.style;
                    let (mut fg, mut bg) = if style.inverse {
                        let fg_resolved = resolve_fg(style.fg, theme);
                        let bg_resolved = resolve_bg(style.bg, theme).unwrap_or(theme.background);
                        (bg_resolved, Some(fg_resolved))
                    } else {
                        (resolve_fg(style.fg, theme), resolve_bg(style.bg, theme))
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
                    let span = if row.get(col_idx + 1).is_some_and(|next| next.wide_spacer) {
                        2
                    } else {
                        1
                    };
                    (
                        col_idx,
                        ch,
                        span,
                        CellLook {
                            fg,
                            bg,
                            bold: style.bold,
                            italic: style.italic,
                            underline: style.underline,
                        },
                    )
                });
            coalesce_runs(looks, advance_safe, theme.foreground)
        })
        .collect();
    let cursor = match (snapshot.cursor.style, snapshot.cursor.row) {
        (CursorStyle::Hidden, _) | (_, None) => None,
        (_, Some(row)) => Some((snapshot.cursor.col, row)),
    };
    PaintFrame {
        background: theme.background,
        rows,
        cursor,
        cursor_style: snapshot.cursor.style,
    }
}

/// Resolve a WIRE snapshot into a [`PaintFrame`]. The broadcaster already
/// coalesced and resolved every run (`wire.rs`'s `row_runs`), so this is a
/// straight decode — hex strings become `0xRRGGBB` — with one substitution:
/// `WireRun.bg: None` ("page background shows through", `wire.rs`) becomes
/// the snapshot's own background (Task 1), so a run is never left with an
/// ambiguous background to inherit from whatever it happens to be painted
/// over.
///
/// Runs are copied through AS GIVEN, never re-coalesced: the wire already
/// merged cells into `WireRun { col, width, text }`, so a receiver cannot
/// tell which glyph inside a multi-char run consumed an extra column (a
/// wide character's spacer, for instance). Per-glyph pinning is therefore a
/// LOCAL-ONLY refinement (see D5 in the peer-instances design doc) —
/// reconstructing it here by guessing glyph widths would misplace every
/// character after a wrong guess.
fn wire_paint_frame(wire: &WireSnapshot) -> PaintFrame {
    let background = parse_wire_hex(&wire.background);
    let rows = wire
        .rows
        .iter()
        .map(|row| decode_wire_row(row, background))
        .collect();
    let cursor = wire
        .cursor
        .as_ref()
        .map(|c| (c.col as usize, c.row as usize));
    // An unrecognized spelling falls back to Bar rather than refusing the
    // frame: a wrong cursor shape is a cosmetic flaw, and dropping the
    // whole frame over one would blank a working terminal.
    let cursor_style = match wire.cursor.as_ref().map(|c| c.shape.as_str()) {
        Some("block") => CursorStyle::Block,
        Some("underline") => CursorStyle::Underline,
        _ => CursorStyle::Bar,
    };
    PaintFrame {
        background,
        rows,
        cursor,
        cursor_style,
    }
}

/// Decode one already-coalesced wire row into paint runs, substituting
/// `background` for any run that left its `bg` unset. Shared by
/// [`wire_paint_frame`] (the live rows) and [`attached_paint_frame`] (a
/// scrolled-back window over `history` + `rows`) so there is exactly one
/// place that turns a `WireRun` into a `Run`.
fn decode_wire_row(row: &[WireRun], background: u32) -> Vec<Run> {
    row.iter()
        .map(|run| Run {
            col: run.col as usize,
            cells: run.width as usize,
            safe: true, // no per-glyph pinning check applies to wire runs
            text: run.text.clone(),
            fg: parse_wire_hex(&run.fg),
            bg: Some(run.bg.as_deref().map(parse_wire_hex).unwrap_or(background)),
            bold: run.b,
            italic: run.i,
            underline: run.u,
        })
        .collect()
}

/// Decode a wire "#rrggbb" string into `0xRRGGBB` — the inverse of
/// `wire::hex`. The wire always emits this exact shape (enforced by the
/// `/version` capability check before a peer ever attaches), so a malformed
/// string here means a build mismatch slipped past that gate; fall back to
/// black rather than let a bad color panic the render loop.
fn parse_wire_hex(s: &str) -> u32 {
    let s = s.trim_start_matches('#');
    // `is_ascii_hexdigit` first because `from_str_radix` also accepts a
    // leading `+`, which would let "#+abcde" decode as 0x0abcde instead of
    // taking the documented black fallback.
    if s.len() == 6 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
        u32::from_str_radix(s, 16).unwrap_or(0)
    } else {
        0
    }
}

/// The largest offset an attached pane can actually scroll to: every row
/// available above the live window, i.e. exactly `history_len` — beyond
/// that there is nothing more to show. The live window ([`wire_paint_frame`]
/// / offset 0) is always exactly `wire.rows`, so scrolling back can only
/// ever reach as far as the history behind it, never resize that window.
fn clamp_attached_offset(history_len: usize, offset: usize) -> usize {
    offset.min(history_len)
}

/// The window of rows an attached pane shows: `offset` (clamped) rows of
/// `history` immediately above the live bottom, followed by however many of
/// the newest `rows` are needed to keep the window exactly `rows.len()`
/// tall — the same height either way, just anchored further back.
///
/// **The window MOVES.** The broadcaster's `history` is a tail relative to
/// ITS live screen, capped at `HISTORY_TAIL` (`term_session.rs`), and the
/// wire carries no row identity — so calling this again with the SAME
/// `offset` against a LATER snapshot does not reveal the same historical
/// row; it reveals whatever is now `offset` rows back from the new bottom.
/// The contract is "stays scrolled back by `offset` rows", never "keeps
/// showing the same row forever".
fn windowed_wire_rows<'a>(
    history: &'a [Vec<WireRun>],
    rows: &'a [Vec<WireRun>],
    offset: usize,
) -> Vec<&'a Vec<WireRun>> {
    let clamped = clamp_attached_offset(history.len(), offset);
    // Index into the conceptual `history ++ rows` sequence where the
    // window starts; always in `0..=history.len()` since `clamped <=
    // history.len()`, so this never underflows.
    let start = history.len() - clamped;
    (0..rows.len())
        .map(|i| {
            let idx = start + i;
            if idx < history.len() {
                &history[idx]
            } else {
                &rows[idx - history.len()]
            }
        })
        .collect()
}

/// Resolve an attached pane's [`PaintFrame`] at a LOCAL scroll `offset`.
/// Scrolling is local to the viewer only (D2: geometry is
/// broadcaster-owned) — this never resizes or scrolls the remote PTY, it
/// only picks which slice of the already-received `history` + `rows` to
/// paint.
///
/// At offset 0 (after clamping — e.g. a snapshot with no history ignores
/// any requested offset) this is exactly [`wire_paint_frame`], cursor
/// included. Scrolled back, the cursor is hidden: its row/col describe the
/// LIVE screen, which is not what a historical window shows, so there is
/// nothing correct to draw it at.
fn attached_paint_frame(wire: &WireSnapshot, offset: usize) -> PaintFrame {
    let clamped = clamp_attached_offset(wire.history.len(), offset);
    if clamped == 0 {
        return wire_paint_frame(wire);
    }
    let background = parse_wire_hex(&wire.background);
    let rows = windowed_wire_rows(&wire.history, &wire.rows, clamped)
        .into_iter()
        .map(|row| decode_wire_row(row, background))
        .collect();
    PaintFrame {
        background,
        rows,
        cursor: None,
        cursor_style: CursorStyle::Hidden,
    }
}

/// The pane container's background for one frame — D5's ruling: an attached
/// pane is NEVER translucent, however the workspace has `translucent` set.
/// Its foregrounds were chosen against the BROADCASTER's background, so
/// letting a local background image show through would restore exactly the
/// unreadability Task 1 removed. A local pane keeps the prior behaviour:
/// fully transparent when translucent (the image layer already applies the
/// user's chosen opacity), else the resolved theme background.
#[derive(Debug, PartialEq)]
enum ContainerBg {
    Transparent,
    Opaque(u32),
}

fn container_background(background: u32, translucent: bool, attached: bool) -> ContainerBg {
    if translucent && !attached {
        ContainerBg::Transparent
    } else {
        ContainerBg::Opaque(background)
    }
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
        // Anchor the IME candidate window at the cursor cell — the one being
        // PAINTED, not the one in `self.snapshot`. For an attached pane that
        // snapshot is the empty placeholder `from_parts` built, so anchoring
        // from it would pop a Japanese or Chinese composition's candidate
        // window at a phantom cursor; and a scrolled-back attached frame
        // paints no cursor at all (Task 3), so it offers no anchor rather
        // than a wrong one.
        let (cursor_col, row) = match &self.attached_frame {
            Some(wire) => attached_cursor_cell(wire, self.attached_scroll_offset)?,
            None => (self.snapshot.cursor.col, self.snapshot.cursor.row?),
        };
        let origin = gpui::point(
            element_bounds.origin.x + px(PADDING + cursor_col as f32 * f32::from(self.cell_width)),
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

        // Hover-gated move listeners go quiet once a selection drag leaves the
        // pane, so while selecting the pointer is sampled here instead — the
        // pump notifies every tick during a drag, keeping this fresh.
        if self.selecting {
            let position = window.mouse_position();
            self.drag_position = Some((position.x, position.y));
        }

        if let Some(session) = self.session.as_mut() {
            // With the companion publishing, the pump may have synced this
            // frame already; re-syncing is only needed for queued UI ops
            // (resize/scroll/selection must apply before paint).
            // Queued UI ops always force a sync (they may arrive after the
            // pump's publish). Otherwise sync only when the grid actually
            // changed since the snapshot was taken — blink and focus frames
            // reuse it, skipping the full grid copy AND the advance scan.
            if session.has_pending_ops() || (!self.snapshot_fresh && self.snapshot_stale) {
                self.snapshot = session.sync_and_snapshot();
                self.advance_scan_pending = true;
            }
            self.snapshot_fresh = false;
            self.snapshot_stale = false;
        }

        let theme = self.theme;
        let focused = self.focus_handle.is_focused(window);
        self.attended = focused && window.is_window_active();
        // Measure any new non-ASCII glyphs on screen against the resolved
        // family: the coalescer only trusts a glyph whose shaped advance
        // matches its grid span, so fallback glyphs pin to their own column.
        // Only when the snapshot (or metrics) changed — blink and selection
        // frames must not pay for a full-grid walk.
        let mut unmeasured: std::collections::HashSet<(char, bool, bool)> =
            std::collections::HashSet::new();
        if self.advance_scan_pending {
            self.advance_scan_pending = false;
            for row in &self.snapshot.rows {
                for cell in row {
                    let key = (cell.ch, cell.style.bold, cell.style.italic);
                    if !cell.ch.is_ascii()
                        && !cell.wide_spacer
                        && !self.advance_cache.contains_key(&key)
                    {
                        unmeasured.insert(key);
                    }
                }
            }
        }
        if !unmeasured.is_empty() {
            let family = self
                .resolved_family
                .clone()
                .unwrap_or_else(|| self.font_family.clone());
            for (ch, bold, italic) in unmeasured {
                let text: SharedString = ch.to_string().into();
                // Shape with the attributes the run will paint with: bold or
                // italic fallback faces can advance differently.
                let mut font = gpui::font(family.clone());
                if bold {
                    font.weight = gpui::FontWeight::BOLD;
                }
                if italic {
                    font.style = gpui::FontStyle::Italic;
                }
                let run = gpui::TextRun {
                    len: text.len(),
                    font,
                    color: gpui::Hsla::default(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let shaped =
                    window
                        .text_system()
                        .shape_line(text, px(self.font_size), &[run], None);
                self.advance_cache
                    .insert((ch, bold, italic), f32::from(shaped.width));
            }
        }

        let snapshot = &self.snapshot;
        let cell_w = self.cell_width;
        let line_h = self.line_height;
        let advance_cache = &self.advance_cache;
        let advance_safe = move |ch: char, span: usize, look: &CellLook| -> bool {
            if ch.is_ascii() {
                return true; // vouched for by the monospace probe
            }
            let expected = span as f32 * f32::from(cell_w);
            advance_cache
                .get(&(ch, look.bold, look.italic))
                .is_some_and(|adv| (adv - expected).abs() <= expected * 0.02)
        };

        // One resolved paint frame. A local pane's theme resolution,
        // selection/search/inverse/dim/hidden, and coalescing all happen
        // inside `local_paint_frame` (`PaintFrame` doc). An attached pane
        // (`attached_frame` populated) reaches the same shape through
        // `attached_paint_frame`, which windows `history` + `rows` by the
        // viewer's own local scroll offset (D2: never scrolls the remote
        // PTY) and falls back to `wire_paint_frame` at offset 0.
        let attached = self.attached_frame.is_some();
        let frame = match &self.attached_frame {
            Some(wire) => attached_paint_frame(wire, self.attached_scroll_offset),
            None => local_paint_frame(snapshot, theme, &advance_safe),
        };
        let frame_background = frame.background;
        let mut row_divs = Vec::with_capacity(frame.rows.len());
        for runs in frame.rows {
            // Runs are pinned at col * cell_width instead of flowed: flowed
            // widths drift off-grid (gpui ceils each text element to whole
            // pixels, and fallback glyphs advance wider than a cell), which
            // clipped the last characters of heavily-styled rows.
            let row_div = div()
                .relative()
                .h(line_h)
                .children(runs.into_iter().map(|run| {
                    // Explicit grid-sized box: the background covers exactly the
                    // run's cells (wide-char spans included). Glyph ink is NOT
                    // clipped — a fallback glyph may legitimately paint past its
                    // cell, and pinning already contains the layout damage.
                    let mut d = div()
                        .absolute()
                        .top(px(0.0))
                        .left(px(run.col as f32 * f32::from(cell_w)))
                        .w(px(run.cells as f32 * f32::from(cell_w)))
                        .h(line_h)
                        .whitespace_nowrap()
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
        // The cursor is drawn ON the frame's canvas, so its colour is judged
        // against that canvas — the broadcaster's background for an attached
        // pane, the viewer's own theme background (unchanged, untouched) for
        // a local one. See [`cursor_color`].
        let cursor_rgb = cursor_color(theme.cursor, frame_background, attached);
        let cursor_div = match frame.cursor {
            None => None,
            Some(_) if !blink_visible => None,
            Some((col, row)) => {
                let left = px(PADDING + col as f32 * f32::from(cell_w));
                let top = px(PADDING + row as f32 * f32::from(line_h));
                let d = div().absolute().left(left).top(top).h(line_h);
                Some(if !focused {
                    d.w(cell_w).border_1().border_color(rgb(cursor_rgb))
                } else {
                    match frame.cursor_style {
                        CursorStyle::Underline => {
                            d.w(cell_w).border_b_2().border_color(rgb(cursor_rgb))
                        }
                        CursorStyle::Block => d.w(cell_w).bg(rgb(cursor_rgb)).opacity(0.7),
                        _ => d.w(px(2.0)).bg(rgb(cursor_rgb)),
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
            .bg(
                match container_background(frame_background, self.translucent, attached) {
                    // Fully transparent over a background image: the image
                    // layer already applies the user's chosen opacity, so
                    // any alpha here would dim it a second time.
                    ContainerBg::Transparent => gpui::rgba(0x0000_0000),
                    ContainerBg::Opaque(bg) => gpui::rgba((bg << 8) | 0xFF),
                },
            )
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
                    // Unreachable while attached: `selecting` is only ever
                    // set by `handle_click`, which refuses both gestures on
                    // an attached pane (`click_gesture`).
                    if this.selecting && event.dragging() {
                        this.drag_position = Some((event.position.x, event.position.y));
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
                    this.drag_position = None;
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(move |this, _event: &MouseUpEvent, _window, _cx| {
                    // Releases outside the pane are invisible to the hover-
                    // gated on_mouse_up; without this the pump auto-scrolls
                    // forever off the stale drag state.
                    this.selecting = false;
                    this.drag_position = None;
                }),
            )
            .on_scroll_wheel(
                cx.listener(move |this, event: &ScrollWheelEvent, _window, cx| {
                    let _ = &pane_for_scroll;
                    let delta = event.delta.pixel_delta(this.line_height).y;
                    let (lines, carry) = scroll_lines_from_delta(
                        this.scroll_accum,
                        f32::from(delta),
                        f32::from(this.line_height),
                    );
                    this.scroll_accum = carry;
                    if lines != 0 {
                        // The one site that makes an attached pane's
                        // scrollback reachable at all: Task 3 built the
                        // windowing and wired it into `render()`, but
                        // nothing moved the offset. D2 keeps this local —
                        // it repicks the slice of the RECEIVED history this
                        // viewer paints and never touches the remote PTY.
                        match this.attached_frame.as_ref().map(|w| w.history.len()) {
                            Some(history_len) => {
                                this.attached_scroll_offset = attached_scroll_after_wheel(
                                    this.attached_scroll_offset,
                                    lines,
                                    history_len,
                                );
                            }
                            None => {
                                if let Some(session) = this.session.as_mut() {
                                    session.queue_scroll(lines);
                                }
                            }
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
            // "[process exited]" describes a process on THIS Mac. An attached
            // pane must never borrow it to describe a terminal on another —
            // see [`exit_notice`], and the Task 6 gap it names.
            .children(
                (exit_notice(attached, self.snapshot.exited.is_some())
                    == ExitNotice::LocalProcessExited)
                    .then(|| {
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
                    }),
            )
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
        // by mouse_tracking check + display_offset. Both gestures read the
        // LOCAL grid, so an attached pane refuses both — see
        // [`click_gesture`] for why click-to-move is the dangerous half.
        let gesture = click_gesture(
            self.attached_frame.is_some(),
            snapshot.mouse_tracking,
            snapshot.alt_screen,
            snapshot.display_offset,
            !snapshot.selection.is_empty(),
        );
        if gesture == ClickGesture::Ignore {
            return;
        }
        if gesture == ClickGesture::MoveCursor {
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
        // Otherwise: begin a selection drag. Only ever reached on a
        // non-attached pane, which is what keeps `selecting` — and so the
        // pump's drag auto-scroll and the mouse-move selection update —
        // structurally unreachable while attached.
        if let Some(session) = self.session.as_mut() {
            session.queue_selection_clear();
            session.queue_selection_start(col, row);
        }
        self.selecting = true;
        self.drag_position = None;
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        attached_paint_frame, busy_dot, clamp_attached_offset, coalesce_runs, container_background,
        drag_scroll_lines, local_paint_frame, may_broadcast_locally, parse_wire_hex,
        scroll_lines_from_delta, windowed_wire_rows, wire_paint_frame, CellLook, ContainerBg, Run,
        COMPANION_BUSY_WINDOW,
    };
    use crate::companion::wire::{WireCursor, WireRun, WireSnapshot};
    use crate::hosts::{ProfileId, Target};
    use crate::term_session::{
        AgentState, CellColor, CellStyle, CursorStyle, RenderableSnapshot, SnapshotCell,
        SnapshotCursor,
    };
    use crate::themes::{default_theme, Theme};

    #[test]
    fn a_local_target_may_join_the_local_fan_out() {
        assert!(may_broadcast_locally(&Target::Local));
    }

    #[test]
    fn a_remote_target_may_never_join_the_local_fan_out() {
        // An attached pane (Phase C2) forwards keystrokes to its peer, so it
        // will also carry a sender — membership must be encoded from the
        // target, never inferred from "has a sender", or enabling local
        // broadcast would fan keystrokes into a terminal on another Mac.
        assert!(!may_broadcast_locally(&Target::Remote(ProfileId(
            "host-1".to_string()
        ))));
    }

    #[test]
    fn uninstrumented_foreground_falls_back_to_recent_output() {
        use std::time::Duration;
        // No agent has reported, so the heuristic is all we have.
        assert!(!busy_dot(true, Duration::from_secs(60), None, 0));
        assert!(busy_dot(true, Duration::from_millis(300), None, 0));
        // shell at its prompt: never busy, however recent the echo.
        assert!(!busy_dot(false, Duration::from_millis(100), None, 0));
        // boundary: silence at the window flips to idle.
        assert!(!busy_dot(true, COMPANION_BUSY_WINDOW, None, 0));
    }

    #[test]
    fn an_instrumented_agent_overrides_the_output_guess() {
        use std::time::Duration;
        // THE BUG: claude blocked on a long silent job. Quiet for minutes,
        // so the heuristic says idle — but the agent says it is working.
        assert!(busy_dot(
            true,
            Duration::from_secs(600),
            Some((AgentState::Working, 1793)),
            1793
        ));
        // The mirror case: claude parked at its prompt having just printed
        // its answer. Output is recent, but it is waiting on the human.
        assert!(!busy_dot(
            true,
            Duration::from_millis(50),
            Some((AgentState::Idle, 1793)),
            1793
        ));
    }

    #[test]
    fn a_stale_state_file_is_ignored() {
        use std::time::Duration;
        // claude exited leaving "working" behind, and something else owns
        // the tty now: the pid no longer matches, so fall back rather than
        // pin the dot on forever.
        assert!(!busy_dot(
            true,
            Duration::from_secs(60),
            Some((AgentState::Working, 1793)),
            4242
        ));
        // And a stale file can never make a shell prompt look busy.
        assert!(!busy_dot(
            false,
            Duration::from_millis(10),
            Some((AgentState::Working, 1793)),
            1793
        ));
    }

    const LOOK: CellLook = CellLook {
        fg: 0xffffff,
        bg: None,
        bold: false,
        italic: false,
        underline: false,
    };

    fn cols_and_texts(runs: &[super::Run]) -> Vec<(usize, &str)> {
        runs.iter().map(|r| (r.col, r.text.as_str())).collect()
    }

    // Predicates standing in for the render-time advance measurement: every
    // glyph verified grid-safe, vs only ASCII trusted (all else fallback).
    fn all_safe(_: char, _: usize, _: &CellLook) -> bool {
        true
    }
    fn ascii_only(ch: char, _: usize, _: &CellLook) -> bool {
        ch.is_ascii()
    }

    #[test]
    fn ascii_same_look_coalesces_into_one_pinned_run() {
        let runs = coalesce_runs(
            [(0, 'l', 1, LOOK), (1, 's', 1, LOOK)].into_iter(),
            &ascii_only,
            0xffffff,
        );
        assert_eq!(cols_and_texts(&runs), vec![(0, "ls")]);
    }

    #[test]
    fn style_change_starts_new_run_at_its_column() {
        let red = CellLook {
            fg: 0xff0000,
            ..LOOK
        };
        let runs = coalesce_runs(
            [(0, 'a', 1, LOOK), (1, 'b', 1, red)].into_iter(),
            &all_safe,
            0xffffff,
        );
        assert_eq!(cols_and_texts(&runs), vec![(0, "a"), (1, "b")]);
    }

    #[test]
    fn unsafe_glyph_pins_at_own_column_and_rebreaks_after() {
        // '⎿' falls back to a font whose advance is ~1.7 cells; flowed after
        // "ab" it would drift every following glyph rightward. It must start
        // its own run, and the ASCII after it must re-pin at its true column.
        let cells = [
            (0, 'a', 1, LOOK),
            (1, 'b', 1, LOOK),
            (2, '⎿', 1, LOOK),
            (3, 'c', 1, LOOK),
            (4, 'd', 1, LOOK),
        ];
        let runs = coalesce_runs(cells.into_iter(), &ascii_only, 0xffffff);
        assert_eq!(cols_and_texts(&runs), vec![(0, "ab"), (2, "⎿"), (3, "cd")]);
    }

    #[test]
    fn repeated_unsafe_glyphs_never_group() {
        // Identical fallback glyphs accumulate the same per-glyph drift this
        // fix exists to kill — repetition is not proof of a one-cell advance.
        let cells = [(0, '⎿', 1, LOOK), (1, '⎿', 1, LOOK), (2, '⎿', 1, LOOK)];
        let runs = coalesce_runs(cells.into_iter(), &ascii_only, 0xffffff);
        assert_eq!(cols_and_texts(&runs), vec![(0, "⎿"), (1, "⎿"), (2, "⎿")]);
    }

    #[test]
    fn verified_glyphs_coalesce_across_identities() {
        // Mixed box-drawing measured at exactly one cell each stays one
        // element per style run, so borders don't explode the element count.
        let cells = [
            (0, '╭', 1, LOOK),
            (1, '─', 1, LOOK),
            (2, '┬', 1, LOOK),
            (3, '─', 1, LOOK),
            (4, '╮', 1, LOOK),
        ];
        let runs = coalesce_runs(cells.into_iter(), &all_safe, 0xffffff);
        assert_eq!(cols_and_texts(&runs), vec![(0, "╭─┬─╮")]);
    }

    #[test]
    fn wide_chars_span_their_cells() {
        // A wide char occupies two grid cells (the caller skips the spacer).
        // Unverified: it pins alone and the col gap is kept. Verified at two
        // cells: contiguous wide chars may merge, and the span accumulates.
        let cells = [(0, 'a', 1, LOOK), (1, '個', 2, LOOK), (3, 'b', 1, LOOK)];
        let runs = coalesce_runs(cells.into_iter(), &ascii_only, 0xffffff);
        assert_eq!(cols_and_texts(&runs), vec![(0, "a"), (1, "個"), (3, "b")]);

        let cells = [(0, '個', 2, LOOK), (2, '個', 2, LOOK)];
        let runs = coalesce_runs(cells.into_iter(), &all_safe, 0xffffff);
        assert_eq!(cols_and_texts(&runs), vec![(0, "個個")]);
        assert_eq!(runs[0].cells, 4);
    }

    #[test]
    fn trailing_default_whitespace_run_is_trimmed_but_row_never_empty() {
        let bold = CellLook { bold: true, ..LOOK };
        let runs = coalesce_runs(
            [(0, 'a', 1, bold), (1, ' ', 1, LOOK), (2, ' ', 1, LOOK)].into_iter(),
            &all_safe,
            0xffffff,
        );
        assert_eq!(cols_and_texts(&runs), vec![(0, "a")]);

        // A row of nothing but default spaces keeps its one run so the row
        // still occupies a line.
        let runs = coalesce_runs(
            [(0, ' ', 1, LOOK), (1, ' ', 1, LOOK)].into_iter(),
            &all_safe,
            0xffffff,
        );
        assert_eq!(cols_and_texts(&runs), vec![(0, "  ")]);
    }

    #[test]
    fn no_scroll_inside_pane() {
        assert_eq!(drag_scroll_lines(100.0, 50.0, 500.0), 0);
        assert_eq!(drag_scroll_lines(50.0, 50.0, 500.0), 0); // exactly at top
        assert_eq!(drag_scroll_lines(500.0, 50.0, 500.0), 0); // exactly at bottom
    }

    #[test]
    fn above_top_scrolls_toward_history() {
        assert_eq!(drag_scroll_lines(49.0, 50.0, 500.0), 1);
        assert_eq!(drag_scroll_lines(10.0, 50.0, 500.0), 2);
    }

    #[test]
    fn below_bottom_scrolls_toward_present() {
        assert_eq!(drag_scroll_lines(501.0, 50.0, 500.0), -1);
        assert_eq!(drag_scroll_lines(540.0, 50.0, 500.0), -2);
    }

    #[test]
    fn speed_caps_at_five_lines() {
        assert_eq!(drag_scroll_lines(-1000.0, 50.0, 500.0), 5);
        assert_eq!(drag_scroll_lines(5000.0, 50.0, 500.0), -5);
    }

    // -- local_paint_frame / wire_paint_frame -------------------------------
    //
    // One adapter, fed by both sources: a LOCAL snapshot resolves through
    // the viewer's theme and runs the existing coalescer; a WIRE snapshot
    // arrives already resolved and coalesced and is decoded directly. Both
    // must land on the same `PaintFrame` shape so the render loop stops
    // needing to know which kind of snapshot it started with.

    fn cell_style() -> CellStyle {
        CellStyle {
            fg: CellColor::Default,
            bg: CellColor::Default,
            bold: false,
            italic: false,
            dim: false,
            underline: false,
            inverse: false,
            hidden: false,
        }
    }

    fn plain_cell(ch: char, style: CellStyle) -> SnapshotCell {
        SnapshotCell {
            ch,
            style,
            wide_spacer: false,
        }
    }

    fn wide_spacer_cell() -> SnapshotCell {
        SnapshotCell {
            ch: '\0',
            style: cell_style(),
            wide_spacer: true,
        }
    }

    fn local_snapshot(rows: Vec<Vec<SnapshotCell>>) -> RenderableSnapshot {
        let cols = rows.first().map(|r| r.len()).unwrap_or(0);
        RenderableSnapshot {
            cols,
            lines: rows.len(),
            rows,
            cursor: SnapshotCursor {
                col: 0,
                row: None,
                style: CursorStyle::Hidden,
            },
            display_offset: 0,
            selection: Vec::new(),
            app_cursor_mode: false,
            bracketed_paste: false,
            mouse_tracking: false,
            alt_screen: false,
            focused_title: None,
            exited: None,
            selection_text: None,
            search_matches: Vec::new(),
            history_rows: Vec::new(),
        }
    }

    fn theme() -> &'static Theme {
        default_theme()
    }

    // Stand-in for the render-time advance measurement: every glyph is
    // vouched for, matching the ASCII-only content these tests use.
    fn always_safe(_: char, _: usize, _: &CellLook) -> bool {
        true
    }

    #[test]
    fn local_frame_of_an_empty_grid_has_no_rows() {
        let snap = local_snapshot(vec![]);
        let frame = local_paint_frame(&snap, theme(), &always_safe);
        assert_eq!(frame.rows, Vec::<Vec<Run>>::new());
    }

    #[test]
    fn local_frame_resolves_plain_cells_to_the_theme_default() {
        let snap = local_snapshot(vec![vec![
            plain_cell('h', cell_style()),
            plain_cell('i', cell_style()),
        ]]);
        let frame = local_paint_frame(&snap, theme(), &always_safe);
        assert_eq!(frame.rows.len(), 1);
        assert_eq!(frame.rows[0].len(), 1);
        assert_eq!(frame.rows[0][0].text, "hi");
        assert_eq!(frame.rows[0][0].fg, theme().foreground);
        assert_eq!(frame.rows[0][0].bg, None);
    }

    #[test]
    fn local_frame_carries_bold_italic_underline_into_the_run() {
        let styled = CellStyle {
            bold: true,
            italic: true,
            underline: true,
            ..cell_style()
        };
        let snap = local_snapshot(vec![vec![plain_cell('x', styled)]]);
        let frame = local_paint_frame(&snap, theme(), &always_safe);
        let run = &frame.rows[0][0];
        assert!(run.bold);
        assert!(run.italic);
        assert!(run.underline);
    }

    #[test]
    fn local_frame_selection_overrides_background_with_the_theme_selection_color() {
        let mut snap = local_snapshot(vec![vec![plain_cell('a', cell_style())]]);
        snap.selection = vec![(0, 0)];
        let frame = local_paint_frame(&snap, theme(), &always_safe);
        assert_eq!(frame.rows[0][0].bg, Some(theme().selection));
    }

    #[test]
    fn local_frame_search_hit_paints_yellow_on_the_theme_background() {
        let mut snap = local_snapshot(vec![vec![plain_cell('a', cell_style())]]);
        snap.search_matches = vec![(0, 0)];
        let frame = local_paint_frame(&snap, theme(), &always_safe);
        let run = &frame.rows[0][0];
        assert_eq!(run.bg, Some(theme().yellow));
        assert_eq!(run.fg, theme().background);
    }

    #[test]
    fn local_frame_inverse_swaps_fg_and_bg() {
        let style = CellStyle {
            fg: CellColor::Rgb(0x11, 0x22, 0x33),
            bg: CellColor::Rgb(0x44, 0x55, 0x66),
            inverse: true,
            ..cell_style()
        };
        let snap = local_snapshot(vec![vec![plain_cell('a', style)]]);
        let frame = local_paint_frame(&snap, theme(), &always_safe);
        let run = &frame.rows[0][0];
        assert_eq!(run.fg, 0x445566);
        assert_eq!(run.bg, Some(0x112233));
    }

    #[test]
    fn local_frame_hidden_cell_paints_its_own_background_as_foreground() {
        let style = CellStyle {
            fg: CellColor::Rgb(0xaa, 0xbb, 0xcc),
            bg: CellColor::Rgb(0x10, 0x20, 0x30),
            hidden: true,
            ..cell_style()
        };
        let snap = local_snapshot(vec![vec![plain_cell('a', style)]]);
        let frame = local_paint_frame(&snap, theme(), &always_safe);
        let run = &frame.rows[0][0];
        assert_eq!(run.fg, 0x102030);
        assert_eq!(run.bg, Some(0x102030));
    }

    #[test]
    fn local_frame_wide_character_spans_two_cells_and_skips_its_spacer() {
        let snap = local_snapshot(vec![vec![
            plain_cell('個', cell_style()),
            wide_spacer_cell(),
        ]]);
        let frame = local_paint_frame(&snap, theme(), &always_safe);
        assert_eq!(frame.rows[0].len(), 1);
        assert_eq!(frame.rows[0][0].text, "個");
        assert_eq!(frame.rows[0][0].cells, 2);
    }

    #[test]
    fn local_frame_cursor_present_reports_its_viewport_position() {
        let mut snap = local_snapshot(vec![vec![plain_cell(' ', cell_style())]]);
        snap.cursor = SnapshotCursor {
            col: 3,
            row: Some(2),
            style: CursorStyle::Block,
        };
        let frame = local_paint_frame(&snap, theme(), &always_safe);
        assert_eq!(frame.cursor, Some((3, 2)));
    }

    #[test]
    fn local_frame_cursor_hidden_or_scrolled_out_reports_absent() {
        let mut snap = local_snapshot(vec![vec![plain_cell(' ', cell_style())]]);
        snap.cursor = SnapshotCursor {
            col: 3,
            row: Some(2),
            style: CursorStyle::Hidden,
        };
        assert_eq!(local_paint_frame(&snap, theme(), &always_safe).cursor, None);

        snap.cursor = SnapshotCursor {
            col: 3,
            row: None,
            style: CursorStyle::Block,
        };
        assert_eq!(local_paint_frame(&snap, theme(), &always_safe).cursor, None);
    }

    #[test]
    fn local_frame_background_is_the_viewer_theme_background() {
        let snap = local_snapshot(vec![]);
        let frame = local_paint_frame(&snap, theme(), &always_safe);
        assert_eq!(frame.background, theme().background);
    }

    fn wire_run(col: u16, width: u16, text: &str, fg: &str, bg: Option<&str>) -> WireRun {
        WireRun {
            col,
            width,
            text: text.to_string(),
            fg: fg.to_string(),
            bg: bg.map(str::to_string),
            b: false,
            i: false,
            u: false,
        }
    }

    fn wire_snapshot(rows: Vec<Vec<WireRun>>, background: &str) -> WireSnapshot {
        WireSnapshot {
            cols: rows.first().map(|r| r.len() as u16).unwrap_or(0),
            lines: rows.len() as u16,
            cursor: None,
            app_cursor: false,
            rows,
            history: Vec::new(),
            bracketed_paste: false,
            mouse_tracking: false,
            background: background.to_string(),
        }
    }

    #[test]
    fn wire_frame_of_an_empty_grid_has_no_rows() {
        let wire = wire_snapshot(vec![], "#000000");
        let frame = wire_paint_frame(&wire);
        assert_eq!(frame.rows, Vec::<Vec<Run>>::new());
    }

    #[test]
    fn wire_frame_decodes_hex_colors_and_width_directly() {
        let wire = wire_snapshot(
            vec![vec![wire_run(0, 2, "hi", "#ff8800", Some("#001122"))]],
            "#000000",
        );
        let frame = wire_paint_frame(&wire);
        let run = &frame.rows[0][0];
        assert_eq!(run.col, 0);
        assert_eq!(run.text, "hi");
        assert_eq!(run.cells, 2);
        assert_eq!(run.fg, 0xff8800);
        assert_eq!(run.bg, Some(0x001122));
    }

    #[test]
    fn wire_frame_carries_bold_italic_underline() {
        let mut run = wire_run(0, 1, "x", "#ffffff", None);
        run.b = true;
        run.i = true;
        run.u = true;
        let wire = wire_snapshot(vec![vec![run]], "#000000");
        let frame = wire_paint_frame(&wire);
        let out = &frame.rows[0][0];
        assert!(out.bold);
        assert!(out.italic);
        assert!(out.underline);
    }

    #[test]
    fn wire_frame_none_background_picks_up_the_snapshot_background() {
        let wire = wire_snapshot(vec![vec![wire_run(0, 1, "x", "#ffffff", None)]], "#123456");
        let frame = wire_paint_frame(&wire);
        assert_eq!(frame.rows[0][0].bg, Some(0x123456));
        assert_eq!(frame.background, 0x123456);
    }

    #[test]
    fn wire_frame_explicit_background_is_kept_over_the_snapshot_background() {
        let wire = wire_snapshot(
            vec![vec![wire_run(0, 1, "x", "#ffffff", Some("#abcdef"))]],
            "#123456",
        );
        let frame = wire_paint_frame(&wire);
        assert_eq!(frame.rows[0][0].bg, Some(0xabcdef));
    }

    #[test]
    fn wire_frame_cursor_present_reports_its_position() {
        let mut wire = wire_snapshot(vec![], "#000000");
        wire.cursor = Some(WireCursor {
            col: 5,
            row: 1,
            shape: "bar".into(),
        });
        assert_eq!(wire_paint_frame(&wire).cursor, Some((5, 1)));
    }

    #[test]
    fn wire_frame_cursor_absent_reports_none() {
        let wire = wire_snapshot(vec![], "#000000");
        assert_eq!(wire_paint_frame(&wire).cursor, None);
    }

    #[test]
    fn wire_frame_a_wide_character_and_its_spacer_arrive_pre_merged_as_one_run() {
        // The broadcaster already folded the spacer into the run's width
        // (wire.rs's row_runs) — the adapter must not try to re-derive
        // per-glyph advance from it, only copy col/width/text through.
        //
        // The run MIXES widths on purpose: "a個b" is 3 chars spanning 4
        // cells, so char count and cell count disagree and no per-glyph
        // rule can recover which glyph took the extra one. A single wide
        // glyph would not prove this — an implementation that split runs
        // per character would still produce one run for "個" and pass.
        let wire = wire_snapshot(
            vec![vec![wire_run(0, 4, "a個b", "#ffffff", None)]],
            "#000000",
        );
        let frame = wire_paint_frame(&wire);
        assert_eq!(frame.rows[0].len(), 1, "the run must not be split");
        assert_eq!(frame.rows[0][0].text, "a個b");
        assert_eq!(frame.rows[0][0].cells, 4);
        assert_ne!(
            frame.rows[0][0].cells,
            frame.rows[0][0].text.chars().count(),
            "the fixture must keep cells and chars different, or it proves nothing"
        );
    }

    #[test]
    fn parse_wire_hex_decodes_a_well_formed_color_and_refuses_everything_else() {
        // Tested directly, not only through wire_paint_frame: this is the
        // one function that reads an arbitrary string off the network, so
        // its contract deserves its own assertions.
        assert_eq!(parse_wire_hex("#a1b2c3"), 0x00a1_b2c3);
        assert_eq!(parse_wire_hex("a1b2c3"), 0x00a1_b2c3, "the # is optional");
        assert_eq!(parse_wire_hex("#FFFFFF"), 0x00ff_ffff, "uppercase decodes");
        for bad in [
            "",
            "#",
            "#abc",
            "#abcdefff",
            "#gggggg",
            "#+abcde",
            "#-abcde",
            "#  abcd",
            "#日本語",
        ] {
            assert_eq!(parse_wire_hex(bad), 0, "{bad:?} must fall back to black");
        }
    }

    #[test]
    fn parse_wire_hex_never_panics_on_hostile_input() {
        // These bytes arrive from ANOTHER MACHINE. A panic here would take
        // down the render loop, so the contract is "always returns", not
        // "returns something sensible".
        for hostile in [
            "\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}",
            "🙂🙂🙂",
            &"f".repeat(10_000),
        ] {
            let _ = parse_wire_hex(hostile);
        }
    }

    #[test]
    fn wire_frame_never_recoalesces_adjacent_same_style_runs() {
        // Proves the wire path is a straight copy, not a re-run of
        // coalesce_runs: two separately emitted runs with identical style
        // stay two runs, because the wire is authoritative about run
        // boundaries and per-glyph pinning is local-only (D5).
        let wire = wire_snapshot(
            vec![vec![
                wire_run(0, 1, "a", "#ffffff", None),
                wire_run(1, 1, "b", "#ffffff", None),
            ]],
            "#000000",
        );
        let frame = wire_paint_frame(&wire);
        assert_eq!(frame.rows[0].len(), 2);
    }

    // -- attached-pane scrollback view model --------------------------------
    //
    // The broadcaster's history tail (`term_session::HISTORY_TAIL`) is
    // relative to ITS live screen and carries no row identity, so a viewer
    // cannot hold a stable anchor on one historical row once it ages out.
    // The contract these tests pin is "stays scrolled back BY OFFSET", never
    // "keeps showing the same row forever" — see `windowed_wire_rows`'s doc.

    /// A one-run row whose text is `label`, so a test can identify exactly
    /// which source row survived into a window by reading `.text` — no
    /// numeric index bookkeeping to get wrong.
    fn labeled_row(label: &str) -> Vec<WireRun> {
        vec![wire_run(0, 1, label, "#ffffff", None)]
    }

    fn row_labels(rows: &[Vec<Run>]) -> Vec<&str> {
        rows.iter().map(|r| r[0].text.as_str()).collect()
    }

    fn wire_snapshot_with_history(
        history: Vec<Vec<WireRun>>,
        rows: Vec<Vec<WireRun>>,
        background: &str,
    ) -> WireSnapshot {
        let mut wire = wire_snapshot(rows, background);
        wire.history = history;
        wire
    }

    #[test]
    fn clamp_attached_offset_passes_a_requested_offset_through_when_history_covers_it() {
        assert_eq!(clamp_attached_offset(5, 3), 3);
    }

    #[test]
    fn clamp_attached_offset_clamps_to_the_oldest_available_row() {
        // History only has 5 rows; asking for 999 must not go negative or
        // panic, and must land exactly on the oldest row, not merely "some"
        // in-bounds value.
        assert_eq!(clamp_attached_offset(5, 999), 5);
    }

    #[test]
    fn clamp_attached_offset_is_zero_with_no_history_regardless_of_request() {
        assert_eq!(clamp_attached_offset(0, 7), 0);
    }

    fn five_history_three_live() -> (Vec<Vec<WireRun>>, Vec<Vec<WireRun>>) {
        let history = vec!["h0", "h1", "h2", "h3", "h4"]
            .into_iter()
            .map(labeled_row)
            .collect();
        let rows = vec!["r0", "r1", "r2"]
            .into_iter()
            .map(labeled_row)
            .collect();
        (history, rows)
    }

    #[test]
    fn offset_zero_window_is_exactly_the_live_rows() {
        let (history, rows) = five_history_three_live();
        let windowed = windowed_wire_rows(&history, &rows, 0);
        let texts: Vec<&str> = windowed.iter().map(|r| r[0].text.as_str()).collect();
        assert_eq!(texts, vec!["r0", "r1", "r2"]);
    }

    #[test]
    fn scrolling_back_reaches_into_history() {
        let (history, rows) = five_history_three_live();
        let windowed = windowed_wire_rows(&history, &rows, 1);
        let texts: Vec<&str> = windowed.iter().map(|r| r[0].text.as_str()).collect();
        // Drops the newest live row, prepends the newest history row — the
        // window stays the same height, anchored one row further back.
        assert_eq!(texts, vec!["h4", "r0", "r1"]);
    }

    #[test]
    fn scrolling_past_the_oldest_row_clamps_rather_than_panicking() {
        let (history, rows) = five_history_three_live();
        let windowed = windowed_wire_rows(&history, &rows, 999);
        let texts: Vec<&str> = windowed.iter().map(|r| r[0].text.as_str()).collect();
        // Clamped to the SPECIFIC oldest window, not just "didn't crash".
        assert_eq!(texts, vec!["h0", "h1", "h2"]);
    }

    #[test]
    fn a_snapshot_with_no_history_behaves_like_a_plain_grid() {
        let (_, rows) = five_history_three_live();
        let windowed = windowed_wire_rows(&[], &rows, 50);
        let texts: Vec<&str> = windowed.iter().map(|r| r[0].text.as_str()).collect();
        assert_eq!(texts, vec!["r0", "r1", "r2"]);
    }

    #[test]
    fn attached_frame_at_offset_zero_matches_a_plain_wire_frame() {
        let (history, rows) = five_history_three_live();
        let mut wire = wire_snapshot_with_history(history, rows, "#123456");
        wire.cursor = Some(WireCursor {
            col: 2,
            row: 1,
            shape: "block".into(),
        });
        let attached = attached_paint_frame(&wire, 0);
        assert_eq!(attached, wire_paint_frame(&wire));
        assert!(
            attached.cursor.is_some(),
            "fixture must carry a cursor, or this proves nothing"
        );
    }

    #[test]
    fn attached_frame_scrolled_back_hides_the_cursor() {
        // The cursor's row/col describe the LIVE screen; a historical
        // window has nothing correct to draw it at.
        let (history, rows) = five_history_three_live();
        let mut wire = wire_snapshot_with_history(history, rows, "#123456");
        wire.cursor = Some(WireCursor {
            col: 2,
            row: 1,
            shape: "block".into(),
        });
        let attached = attached_paint_frame(&wire, 1);
        assert_eq!(attached.cursor, None);
    }

    #[test]
    fn attached_frame_scrolled_back_shows_history_rows() {
        let (history, rows) = five_history_three_live();
        let wire = wire_snapshot_with_history(history, rows, "#123456");
        let attached = attached_paint_frame(&wire, 2);
        assert_eq!(row_labels(&attached.rows), vec!["h3", "h4", "r0"]);
    }

    #[test]
    fn a_slow_trackpad_gesture_accumulates_into_a_line_instead_of_vanishing() {
        // The reported bug: at the default font size a line is 20px, and
        // macOS sends 1-8px per event for a gentle two-finger scroll. Each
        // event alone rounds to zero, so the OLD code scrolled nothing no
        // matter how long the gesture ran.
        let line = 20.0;
        let mut accum = 0.0;
        let mut scrolled = 0;
        for _ in 0..5 {
            let (lines, carry) = scroll_lines_from_delta(accum, 5.0, line);
            accum = carry;
            scrolled += lines;
        }
        assert_eq!(scrolled, 1, "five 5px events are exactly one 20px line");
        // The old implementation, for contrast: every event independently
        // rounded to zero and kept nothing.
        assert_eq!((5.0f32 / line).round() as i32, 0);
    }

    #[test]
    fn the_carry_is_retained_rather_than_zeroed_after_a_line_is_emitted() {
        // If the remainder were dropped on emit, a continuous gesture would
        // lose a fraction of a line every time it crossed one, and long
        // scrolls would drift progressively short.
        let (lines, carry) = scroll_lines_from_delta(0.0, 30.0, 20.0);
        assert_eq!(lines, 1);
        assert!(
            (carry - 0.5).abs() < 1e-6,
            "half a line must survive to the next event, got {carry}"
        );
    }

    #[test]
    fn one_exact_line_leaves_no_remainder() {
        let (lines, carry) = scroll_lines_from_delta(0.0, 20.0, 20.0);
        assert_eq!(lines, 1);
        assert_eq!(carry, 0.0);
    }

    #[test]
    fn a_flick_still_scrolls_its_full_distance_in_one_event() {
        // Accumulating must not throttle a large delta: a mouse wheel notch
        // or a fast swipe still moves everything it asked for at once.
        let (lines, _) = scroll_lines_from_delta(0.0, 205.0, 20.0);
        assert_eq!(lines, 10);
    }

    #[test]
    fn reversing_direction_does_not_strand_the_previous_remainder() {
        // Scroll most of a line one way, then the same distance back: the
        // net movement is zero and no line may be emitted in either
        // direction. `round` instead of `trunc` breaks exactly this.
        let (a, carry) = scroll_lines_from_delta(0.0, 18.0, 20.0);
        assert_eq!(a, 0);
        let (b, carry) = scroll_lines_from_delta(carry, -18.0, 20.0);
        assert_eq!(b, 0, "the reversal must cancel, not emit a line");
        assert!(
            carry.abs() < 1e-6,
            "and must land back at zero, got {carry}"
        );
    }

    #[test]
    fn a_zero_line_height_is_ignored_and_never_poisons_the_carry() {
        // `line_height` is 0 before the first layout. Dividing by it gives
        // inf/NaN, and a NaN carry would make the pane unscrollable for the
        // rest of its life.
        let (lines, carry) = scroll_lines_from_delta(0.25, 12.0, 0.0);
        assert_eq!(lines, 0);
        assert!(carry.is_finite(), "carry must stay finite, got {carry}");
        assert_eq!(carry, 0.25, "an ignored event must not disturb the carry");
    }

    #[test]
    fn a_non_finite_input_cannot_wedge_scrolling_forever() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let (lines, carry) = scroll_lines_from_delta(0.5, bad, 20.0);
            assert_eq!(lines, 0);
            assert!(carry.is_finite(), "{bad} left a non-finite carry");
        }
        // And a carry that somehow already went bad recovers rather than
        // staying stuck.
        let (_, carry) = scroll_lines_from_delta(f32::NAN, 20.0, 20.0);
        assert!(
            carry.is_finite(),
            "a poisoned carry must reset, not persist"
        );
    }

    #[test]
    fn a_viewport_taller_than_the_history_straddles_the_boundary_correctly() {
        // Every other fixture here has MORE history (5) than live rows (3),
        // so the scrolled window never has to span the boundary with
        // history left over on one side only. This one inverts that: one
        // history row against three live rows, scrolled back by one, so the
        // window must take the single history row and then STOP taking
        // history and start taking live rows mid-window.
        //
        // An implementation that indexed live rows by the raw window
        // position rather than by the post-history offset returns h0,r1,r2
        // here (verified by sabotage). The 5-history/3-live fixtures catch
        // that particular slip too, so this test's value is not "the only
        // one that fails" — it is covering the INVERTED ratio, where the
        // window runs out of history mid-span, which no other fixture
        // reaches.
        let wire = wire_snapshot_with_history(
            vec![labeled_row("h0")],
            vec![labeled_row("r0"), labeled_row("r1"), labeled_row("r2")],
            "#123456",
        );
        assert_eq!(
            row_labels(&attached_paint_frame(&wire, 1).rows),
            vec!["h0", "r0", "r1"]
        );
        // And the clamp still lands on the same window, since one row of
        // history is all there is to scroll into.
        assert_eq!(
            row_labels(&attached_paint_frame(&wire, 99).rows),
            vec!["h0", "r0", "r1"]
        );
    }

    #[test]
    fn attached_frame_with_no_history_ignores_a_requested_offset() {
        let (_, rows) = five_history_three_live();
        let wire = wire_snapshot(rows, "#123456");
        assert_eq!(
            attached_paint_frame(&wire, 50),
            wire_paint_frame(&wire),
            "no history means nothing to scroll back into"
        );
    }

    #[test]
    fn scrolling_back_survives_a_new_frame_without_snapping_to_the_bottom() {
        // Simulates one tick of the broadcaster's window moving forward:
        // the oldest history row (h0) ages out, and what used to be the
        // newest live row (r0) becomes history. A viewer holding a fixed
        // offset of 2 must still see a SCROLLED-BACK view of the new frame,
        // not be silently snapped back to its live bottom.
        let history_b: Vec<Vec<WireRun>> = vec!["h1", "h2", "h3", "h4", "r0"]
            .into_iter()
            .map(labeled_row)
            .collect();
        let rows_b: Vec<Vec<WireRun>> = vec!["r1", "r2", "r3"]
            .into_iter()
            .map(labeled_row)
            .collect();
        let wire_b = wire_snapshot_with_history(history_b, rows_b, "#123456");
        let attached = attached_paint_frame(&wire_b, 2);
        assert_ne!(
            attached,
            wire_paint_frame(&wire_b),
            "a fixed offset must not collapse back to the live frame on new data"
        );
    }

    #[test]
    fn rows_ageing_out_shift_the_scrolled_back_window_without_panicking_or_misclamping() {
        // Same fixed offset (2), one frame apart: A is the older frame, B is
        // exactly what A becomes after one more row of output ages h0 out of
        // history and folds the old r0 into it. The window must shift
        // forward by exactly one row — never panic, never clamp back to A's
        // window, never jump all the way to B's live bottom.
        let (history_a, rows_a) = five_history_three_live();
        let wire_a = wire_snapshot_with_history(history_a, rows_a, "#123456");
        let history_b: Vec<Vec<WireRun>> = vec!["h1", "h2", "h3", "h4", "r0"]
            .into_iter()
            .map(labeled_row)
            .collect();
        let rows_b: Vec<Vec<WireRun>> = vec!["r1", "r2", "r3"]
            .into_iter()
            .map(labeled_row)
            .collect();
        let wire_b = wire_snapshot_with_history(history_b, rows_b, "#123456");

        let frame_a = attached_paint_frame(&wire_a, 2);
        let frame_b = attached_paint_frame(&wire_b, 2);
        assert_eq!(row_labels(&frame_a.rows), vec!["h3", "h4", "r0"]);
        assert_eq!(row_labels(&frame_b.rows), vec!["h4", "r0", "r1"]);
        assert_ne!(
            frame_a, frame_b,
            "the window must have moved, not held still"
        );
    }

    #[test]
    fn attached_container_ignores_translucency_and_paints_the_broadcasters_background() {
        // D5's ruling: an attached pane is never translucent, because the
        // foregrounds it received were chosen against the BROADCASTER's
        // background — showing a local background image through would
        // restore the unreadability Task 1 removed.
        let translucent_on = container_background(0x112233, true, true);
        let translucent_off = container_background(0x112233, false, true);
        assert_eq!(translucent_on, ContainerBg::Opaque(0x112233));
        assert_eq!(translucent_off, ContainerBg::Opaque(0x112233));
        assert_eq!(
            translucent_on, translucent_off,
            "the translucency setting must not change an attached pane's container colour"
        );
    }

    #[test]
    fn local_container_still_honors_translucency() {
        // Pins the pre-Task-3 local-pane behaviour exactly: translucent ->
        // fully transparent (the background image shows through);
        // otherwise the theme background.
        assert_eq!(
            container_background(0x112233, true, false),
            ContainerBg::Transparent
        );
        assert_eq!(
            container_background(0x112233, false, false),
            ContainerBg::Opaque(0x112233)
        );
    }
}

/// Task 4: honesty without a local session. Every predicate here answers a
/// question an ATTACHED pane is asked — one whose naive `Option`-shaped
/// answer is plausible and wrong.
#[cfg(test)]
mod attached_honesty_tests {
    use super::{
        attached_cursor_cell, attached_paint_frame, attached_scroll_after_wheel,
        clamp_attached_offset, click_gesture, companion_activity_of, cursor_color,
        drag_scroll_lines, exit_notice, input_route, may_publish_to_companion, scroll_after_frame,
        shell_is_live, ClickGesture, ExitNotice, InputRoute,
    };
    use crate::companion::wire::{WireCursor, WireRun, WireSnapshot};
    use crate::hosts::{ProfileId, Target};
    use superterminal_core::activity::Activity;

    fn run(col: u16, text: &str) -> WireRun {
        WireRun {
            col,
            width: text.chars().count() as u16,
            text: text.to_string(),
            fg: "#c0caf5".to_string(),
            bg: None,
            b: false,
            i: false,
            u: false,
        }
    }

    fn snapshot(history: usize, live: usize, cursor: Option<(u16, u16)>) -> WireSnapshot {
        WireSnapshot {
            cols: 8,
            lines: live as u16,
            cursor: cursor.map(|(col, row)| WireCursor {
                col,
                row,
                shape: "block".to_string(),
            }),
            app_cursor: false,
            rows: (0..live).map(|i| vec![run(0, &format!("r{i}"))]).collect(),
            history: (0..history)
                .map(|i| vec![run(0, &format!("h{i}"))])
                .collect(),
            bracketed_paste: false,
            mouse_tracking: false,
            background: "#1a1b26".to_string(),
        }
    }

    fn remote() -> Target {
        Target::Remote(ProfileId("peer-1".to_string()))
    }

    // --- site 3: the load-bearing None -------------------------------------

    #[test]
    fn an_attached_pane_may_never_publish_to_this_macs_companion() {
        // D4. Today this is enforced by ACCIDENT: the pump's publish arm is
        // `if let Some(session) = pane.session.as_mut()`, and an attached
        // pane has no session. That accident is what this pins — the
        // (attached, has_session) = (true, true) row is the one a later
        // "fix" that hands an attached pane a session would break, turning
        // the phone into a remote view of a remote view.
        assert!(
            !may_publish_to_companion(true, true),
            "an attached pane must not publish even if it somehow has a session"
        );
        assert!(!may_publish_to_companion(true, false));
        assert!(
            may_publish_to_companion(false, true),
            "a local pane with a session must publish exactly as before"
        );
        assert!(!may_publish_to_companion(false, false));
    }

    // --- site 13: write_self -----------------------------------------------

    #[test]
    fn input_from_an_attached_pane_never_reaches_a_local_pty() {
        // The dangerous row is (true, true): attachment must win over the
        // presence of a local session, or a keystroke meant for another
        // machine gets typed into this one.
        assert_eq!(input_route(true, true), InputRoute::Peer);
        assert_eq!(input_route(true, false), InputRoute::Peer);
        assert_eq!(input_route(false, true), InputRoute::LocalPty);
        assert_eq!(input_route(false, false), InputRoute::Nowhere);
    }

    // --- site 5: has_live_shell --------------------------------------------

    #[test]
    fn an_attached_panes_shell_liveness_is_the_attachments_not_the_local_sessions() {
        assert!(
            shell_is_live(true, false),
            "a remote shell is live because the attachment is, not because a local PTY exists"
        );
        assert!(
            shell_is_live(false, true),
            "unchanged for a live local pane"
        );
        assert!(
            !shell_is_live(false, false),
            "a local pane whose shell never started or has exited is still dead"
        );
    }

    // --- the phone's dot: Idle vs Unknown ----------------------------------

    #[test]
    fn a_pane_with_no_local_busy_signal_reports_unknown_to_the_phone_not_idle() {
        // `Activity::from_local_busy(false)` is `Idle`, and that is the bug:
        // the ABSENCE of a busy signal is not the observation of a prompt.
        assert_eq!(companion_activity_of(&remote(), None), Activity::Unknown);
        assert_ne!(
            companion_activity_of(&remote(), None),
            Activity::Idle,
            "the tri-state exists precisely so this is not Idle"
        );
    }

    #[test]
    fn a_local_panes_phone_dot_is_unchanged_in_all_three_cases() {
        assert_eq!(
            companion_activity_of(&Target::Local, None),
            Activity::Idle,
            "a local pane with no session was Idle before and must stay Idle"
        );
        assert_eq!(
            companion_activity_of(&Target::Local, Some(true)),
            Activity::Busy
        );
        assert_eq!(
            companion_activity_of(&Target::Local, Some(false)),
            Activity::Idle
        );
    }

    // --- site 21 + click-to-move -------------------------------------------

    #[test]
    fn a_click_on_an_attached_pane_starts_neither_gesture() {
        // These are EXACTLY the field values of the placeholder snapshot
        // `from_parts` builds and an attached pane never replaces: no mouse
        // tracking, normal buffer, offset 0, no selection. Every
        // click-to-move guard passes, so an implementation that ignored
        // `attached` would answer MoveCursor and encode arrow keys from a
        // phantom cursor.
        assert_eq!(
            click_gesture(true, false, false, 0, false),
            ClickGesture::Ignore
        );
        assert_eq!(
            click_gesture(false, false, false, 0, false),
            ClickGesture::MoveCursor,
            "the same inputs on a local pane must still move the cursor"
        );
    }

    #[test]
    fn an_attached_pane_ignores_a_click_whatever_its_local_grid_says() {
        for &mouse_tracking in &[false, true] {
            for &alt_screen in &[false, true] {
                for &offset in &[0usize, 3] {
                    for &has_selection in &[false, true] {
                        assert_eq!(
                            click_gesture(true, mouse_tracking, alt_screen, offset, has_selection),
                            ClickGesture::Ignore,
                            "attached must ignore regardless of local grid state"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_local_click_chooses_move_or_selection_exactly_as_before() {
        // Mirrors the pre-Task-4 guard: prompt row only, at the bottom, no
        // selection, no app mouse tracking, normal buffer.
        assert_eq!(
            click_gesture(false, false, false, 0, false),
            ClickGesture::MoveCursor
        );
        assert_eq!(
            click_gesture(false, true, false, 0, false),
            ClickGesture::StartSelection
        );
        assert_eq!(
            click_gesture(false, false, true, 0, false),
            ClickGesture::StartSelection
        );
        assert_eq!(
            click_gesture(false, false, false, 1, false),
            ClickGesture::StartSelection
        );
        assert_eq!(
            click_gesture(false, false, false, 0, true),
            ClickGesture::StartSelection
        );
    }

    // --- the "[process exited]" overlay ------------------------------------

    #[test]
    fn an_attached_pane_never_shows_the_local_process_exited_overlay() {
        // `snapshot.exited` describes THIS Mac. The wire carries no exit
        // signal at protocol 2, so the honest answer for an attached pane is
        // "say nothing", never "say the local thing".
        assert_eq!(exit_notice(true, true), ExitNotice::None);
        assert_eq!(exit_notice(true, false), ExitNotice::None);
    }

    #[test]
    fn a_local_pane_still_shows_the_overlay_exactly_when_its_process_exited() {
        assert_eq!(exit_notice(false, true), ExitNotice::LocalProcessExited);
        assert_eq!(exit_notice(false, false), ExitNotice::None);
    }

    // --- the cursor overlay colour -----------------------------------------

    #[test]
    fn a_local_panes_cursor_colour_is_the_theme_value_untouched() {
        // Even a theme whose own cursor barely contrasts with its own
        // background must come through verbatim: recolouring it would
        // change a LOCAL pane.
        assert_eq!(cursor_color(0x1c1c1c, 0x1a1a1a, false), 0x1c1c1c);
        assert_eq!(cursor_color(0xf0f0f0, 0x101010, false), 0xf0f0f0);
    }

    #[test]
    fn an_attached_panes_cursor_is_pushed_clear_of_the_broadcasters_background() {
        // The cursor is drawn over the BROADCASTER's canvas, so it must be
        // legible against that, not against the viewer's theme background —
        // the same reasoning as D5's translucency ruling.
        let vanishing = cursor_color(0x1c1c1c, 0x1a1a1a, true);
        assert_ne!(
            vanishing, 0x1c1c1c,
            "a cursor that would vanish into the broadcaster's background must be boosted"
        );
        // Already-legible pairs pass through, so an attached pane keeps the
        // viewer's cursor identity wherever it works.
        assert_eq!(cursor_color(0xf0f0f0, 0x101010, true), 0xf0f0f0);
    }

    // --- the IME anchor -----------------------------------------------------

    #[test]
    fn the_ime_anchor_reports_the_wire_cursor_at_the_live_bottom() {
        let wire = snapshot(4, 3, Some((5, 2)));
        assert_eq!(attached_cursor_cell(&wire, 0), Some((5, 2)));
    }

    #[test]
    fn the_ime_anchor_offers_no_cell_at_all_once_scrolled_back() {
        // Task 3 established that a scrolled-back attached frame paints NO
        // cursor. An anchor at a cursor nobody can see is a phantom.
        let wire = snapshot(4, 3, Some((5, 2)));
        assert_eq!(attached_cursor_cell(&wire, 1), None);
        assert_eq!(attached_cursor_cell(&wire, 4), None);
    }

    #[test]
    fn the_ime_anchor_ignores_an_offset_no_history_can_satisfy() {
        // No history means offset 0 after clamping, so the live cursor is
        // still the right anchor.
        let wire = snapshot(0, 3, Some((1, 1)));
        assert_eq!(attached_cursor_cell(&wire, 9), Some((1, 1)));
    }

    #[test]
    fn the_ime_anchor_never_disagrees_with_the_cursor_actually_painted() {
        // The whole point of the fix: the candidate window must sit where
        // the cursor IS. Asserting the two functions agree across every
        // reachable offset is what stops them drifting apart later.
        for history in [0usize, 1, 4] {
            for cursor in [None, Some((0u16, 0u16)), Some((5, 2))] {
                let wire = snapshot(history, 3, cursor);
                for offset in 0..=history + 2 {
                    assert_eq!(
                        attached_cursor_cell(&wire, offset),
                        attached_paint_frame(&wire, offset).cursor,
                        "history={history} cursor={cursor:?} offset={offset}"
                    );
                }
            }
        }
    }

    // --- site 19: the wheel, which is what makes scrollback reachable -------

    #[test]
    fn the_wheel_scrolls_an_attached_pane_back_into_history() {
        assert_eq!(attached_scroll_after_wheel(0, 3, 10), 3);
        assert_eq!(attached_scroll_after_wheel(3, 2, 10), 5);
    }

    #[test]
    fn the_wheel_stops_at_the_oldest_row_it_has() {
        assert_eq!(attached_scroll_after_wheel(8, 5, 10), 10);
        assert_eq!(
            attached_scroll_after_wheel(0, 7, 0),
            0,
            "no history means nothing to scroll back into"
        );
    }

    #[test]
    fn the_wheel_never_scrolls_past_the_live_bottom() {
        assert_eq!(attached_scroll_after_wheel(2, -5, 10), 0);
        assert_eq!(
            attached_scroll_after_wheel(0, -1, 10),
            0,
            "must not underflow below the live window"
        );
    }

    #[test]
    fn the_wheels_sign_convention_matches_the_drag_autoscrolls() {
        // The brief fixes the convention by pointing at `drag_scroll_lines`:
        // positive means "toward history". Feeding its own output in rather
        // than restating the sign is what makes this a check and not a
        // duplicate of the implementation.
        let above_top = drag_scroll_lines(0.0, 100.0, 300.0);
        let below_bottom = drag_scroll_lines(400.0, 100.0, 300.0);
        assert!(above_top > 0 && below_bottom < 0, "fixture sanity");
        assert!(
            attached_scroll_after_wheel(5, above_top, 20) > 5,
            "dragging above the top edge must reveal older rows"
        );
        assert!(
            attached_scroll_after_wheel(5, below_bottom, 20) < 5,
            "dragging below the bottom edge must return toward the live screen"
        );
    }

    // --- the re-clamp on frame arrival -------------------------------------

    #[test]
    fn an_arriving_frame_clamps_the_offset_against_history_only() {
        // history 3, live rows 5. An implementation that clamped against
        // `history + rows` (or against `rows`) would answer 6 or 5; only
        // "history" answers 3.
        let wire = snapshot(3, 5, None);
        assert_eq!(scroll_after_frame(6, &wire), 3);
        assert_eq!(clamp_attached_offset(wire.history.len(), 6), 3);
    }

    #[test]
    fn an_arriving_frame_leaves_a_satisfiable_offset_alone() {
        let wire = snapshot(10, 3, None);
        assert_eq!(scroll_after_frame(4, &wire), 4);
    }

    #[test]
    fn a_cleared_broadcaster_truncates_the_offset_and_it_does_not_spring_back() {
        // The decision this task owns. `clear` on the broadcaster drops
        // history to zero: the viewer lands at the live bottom. When 150
        // rows regrow, a viewer that had kept the raw offset would silently
        // jump back to 150-rows-scrolled with no user action. Persisting the
        // clamp is what prevents that.
        let mut offset = 120usize;
        offset = scroll_after_frame(offset, &snapshot(0, 24, None));
        assert_eq!(offset, 0, "a cleared screen puts the viewer at the bottom");
        offset = scroll_after_frame(offset, &snapshot(150, 24, None));
        assert_eq!(
            offset, 0,
            "regrown history must not silently restore the old scroll position"
        );
    }
}
