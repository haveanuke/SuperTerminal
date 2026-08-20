//! Headless terminal session: alacritty_terminal PTY + grid, no gpui.
//!
//! One `TermSession` per pane. The UI layer wraps this in a gpui entity and
//! calls [`TermSession::sync_and_snapshot`] once per frame; everything else
//! is deferred through queues so the `FairMutex<Term>` is locked exactly once
//! per frame from the UI thread (contract rev 2 §1).
//!
//! Wake model (contract rev 2 §2, spike-proven): the PTY thread flips a dirty
//! flag; the UI polls it on a short timer. The flag is bounded by construction
//! — output floods cannot grow any queue. Discrete events (title, child exit,
//! bell) are rare and go through a small mutex-guarded vector drained on sync.

use std::collections::HashMap;
use std::os::fd::AsRawFd;
use std::os::raw::c_int;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event as AlacEvent, EventListener, Notify, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg, Notifier, State as IoState};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor};

extern "C" {
    fn tcgetpgrp(fd: c_int) -> c_int;
    fn kill(pid: c_int, sig: c_int) -> c_int;
    fn dup2(from: c_int, to: c_int) -> c_int;
}
const SIGKILL: c_int = 9;

/// Standard xterm color for OSC 4/10/11 queries (0-15 classic, cube, gray).
fn default_palette_color(index: usize) -> alacritty_terminal::vte::ansi::Rgb {
    use alacritty_terminal::vte::ansi::Rgb;
    const BASE: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (205, 49, 49),
        (13, 188, 121),
        (229, 229, 16),
        (36, 114, 200),
        (188, 63, 188),
        (17, 168, 205),
        (229, 229, 229),
        (102, 102, 102),
        (241, 76, 76),
        (35, 209, 139),
        (245, 245, 67),
        (59, 142, 234),
        (214, 112, 214),
        (41, 184, 219),
        (255, 255, 255),
    ];
    match index {
        0..=15 => {
            let (r, g, b) = BASE[index];
            Rgb { r, g, b }
        }
        16..=231 => {
            let i = index - 16;
            let level = |c: usize| if c == 0 { 0u8 } else { (55 + 40 * c) as u8 };
            Rgb {
                r: level(i / 36),
                g: level((i / 6) % 6),
                b: level(i % 6),
            }
        }
        232..=255 => {
            let g = (8 + 10 * (index - 232)) as u8;
            Rgb { r: g, g, b: g }
        }
        _ => Rgb { r: 0, g: 0, b: 0 },
    }
}

/// Cell colors resolved against the theme by the renderer, not here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellColor {
    /// Theme default foreground/background (which one is positional).
    Default,
    /// ANSI palette index 0-255 (0-15 map to the theme's named colors).
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, Debug)]
pub struct CellStyle {
    pub fg: CellColor,
    pub bg: CellColor,
    pub bold: bool,
    pub italic: bool,
    pub dim: bool,
    pub underline: bool,
    pub inverse: bool,
    pub hidden: bool,
}

#[derive(Clone, Debug)]
pub struct SnapshotCell {
    pub ch: char,
    pub style: CellStyle,
    /// True for the spacer cell after a wide (double-width) glyph.
    pub wide_spacer: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorStyle {
    Bar,
    Block,
    Underline,
    Hidden,
}

#[derive(Clone, Debug)]
pub struct SnapshotCursor {
    pub col: usize,
    /// Row in VIEWPORT coordinates (0 = top visible row); None when the
    /// cursor is scrolled out of view.
    pub row: Option<usize>,
    pub style: CursorStyle,
}

/// Everything the renderer needs for one frame, copied out under one lock.
#[derive(Clone, Debug)]
pub struct RenderableSnapshot {
    pub cols: usize,
    pub lines: usize,
    /// Viewport rows, top to bottom; each row has exactly `cols` cells.
    pub rows: Vec<Vec<SnapshotCell>>,
    pub cursor: SnapshotCursor,
    /// How far the viewport is scrolled back into history (0 = at bottom).
    pub display_offset: usize,
    /// Cells covered by the active selection, as (col, viewport_row) pairs.
    pub selection: Vec<(usize, usize)>,
    pub app_cursor_mode: bool,
    pub mouse_tracking: bool,
    /// True while the alternate screen buffer is active (vim, htop, ...).
    pub alt_screen: bool,
    pub focused_title: Option<String>,
    /// Exit code once the shell has terminated.
    pub exited: Option<i32>,
    /// Text covered by the active selection (captured under the same lock).
    pub selection_text: Option<String>,
}

/// Deferred UI -> terminal operations, applied at the next sync.
enum TermOp {
    Resize { size: TermSize, window: WindowSize },
    Scroll(i32),
    StartSelection { col: usize, row: usize },
    UpdateSelection { col: usize, row: usize },
    ClearSelection,
}

/// Discrete events surfaced to the UI (drained on sync).
#[derive(Clone, Debug, PartialEq)]
pub enum SessionEvent {
    TitleChanged(String),
    Bell,
    Exited(i32),
}

#[derive(Clone)]
struct EventProxy {
    dirty: Arc<AtomicBool>,
    events: Arc<Mutex<Vec<SessionEvent>>>,
    writer: Arc<Mutex<Option<EventLoopSender>>>,
    /// Kept current by resizes; answers TextAreaSizeRequest inline.
    window_size: Arc<Mutex<WindowSize>>,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: AlacEvent) {
        match event {
            AlacEvent::PtyWrite(text) => {
                // Terminal query responses; answered inline on the PTY thread.
                if let Some(sender) = self.writer.lock().unwrap().as_ref() {
                    let _ = sender.send(Msg::Input(text.into_bytes().into()));
                }
                return;
            }
            AlacEvent::Title(title) => {
                self.events
                    .lock()
                    .unwrap()
                    .push(SessionEvent::TitleChanged(title));
            }
            AlacEvent::Bell => {
                self.events.lock().unwrap().push(SessionEvent::Bell);
            }
            AlacEvent::ChildExit(status) => {
                self.events
                    .lock()
                    .unwrap()
                    .push(SessionEvent::Exited(status.code().unwrap_or(-1)));
            }
            AlacEvent::ColorRequest(index, format) => {
                // Answer with the standard xterm palette value; themed color
                // reporting is v1.x, but silence would hang querying programs.
                let rgb = default_palette_color(index);
                if let Some(sender) = self.writer.lock().unwrap().as_ref() {
                    let _ = sender.send(Msg::Input(format(rgb).into_bytes().into()));
                }
                return;
            }
            AlacEvent::TextAreaSizeRequest(format) => {
                let size = *self.window_size.lock().unwrap();
                if let Some(sender) = self.writer.lock().unwrap().as_ref() {
                    let _ = sender.send(Msg::Input(format(size).into_bytes().into()));
                }
                return;
            }
            // Wakeup / damage / clipboard: redraw covers them in v1 (OSC 52
            // clipboard is v1.x).
            _ => {}
        }
        self.dirty.store(true, Ordering::Release);
    }
}

pub struct TermSession {
    term: Arc<FairMutex<Term<EventProxy>>>,
    notifier: Notifier,
    sender: EventLoopSender,
    io_thread: Option<JoinHandle<(EventLoop<tty::Pty, EventProxy>, IoState)>>,
    dirty: Arc<AtomicBool>,
    events: Arc<Mutex<Vec<SessionEvent>>>,
    deferred: Vec<TermOp>,
    /// Spawned shell pid (for cwd lookup fallback) and PTY master fd (for
    /// foreground-process lookup via tcgetpgrp).
    shell_pid: i32,
    master_fd: c_int,
    title: Option<String>,
    exited: Option<i32>,
    cols: usize,
    lines: usize,
    shared_window_size: Arc<Mutex<WindowSize>>,
}

impl TermSession {
    /// Spawn the user's shell on a fresh PTY. MUST be called on the main
    /// thread (contract rev 2 §4: signal-mask correctness without sigmask
    /// juggling).
    pub fn spawn(
        cols: usize,
        lines: usize,
        cell_width: u16,
        cell_height: u16,
        working_directory: Option<PathBuf>,
    ) -> Result<Self, String> {
        // $SHELL explicitly (not the /usr/bin/login route): login can block in
        // headless contexts, and $SHELL matches the previous app's behavior.
        let shell = std::env::var("SHELL")
            .ok()
            .map(|sh| tty::Shell::new(sh, Vec::new()));
        Self::spawn_with_shell(
            cols,
            lines,
            cell_width,
            cell_height,
            working_directory,
            shell,
        )
    }

    fn spawn_with_shell(
        cols: usize,
        lines: usize,
        cell_width: u16,
        cell_height: u16,
        working_directory: Option<PathBuf>,
        shell: Option<tty::Shell>,
    ) -> Result<Self, String> {
        let dirty = Arc::new(AtomicBool::new(false));
        let events = Arc::new(Mutex::new(Vec::new()));
        let writer = Arc::new(Mutex::new(None));
        let shared_window_size = Arc::new(Mutex::new(WindowSize {
            num_cols: cols as u16,
            num_lines: lines as u16,
            cell_width,
            cell_height,
        }));
        let proxy = EventProxy {
            dirty: Arc::clone(&dirty),
            events: Arc::clone(&events),
            writer: Arc::clone(&writer),
            window_size: Arc::clone(&shared_window_size),
        };

        let size = TermSize::new(cols, lines);
        let term = Arc::new(FairMutex::new(Term::new(
            TermConfig {
                scrolling_history: 10_000,
                default_cursor_style: alacritty_terminal::vte::ansi::CursorStyle {
                    shape: CursorShape::Beam,
                    blinking: true,
                },
                ..Default::default()
            },
            &size,
            proxy.clone(),
        )));

        let window_size = WindowSize {
            num_cols: cols as u16,
            num_lines: lines as u16,
            cell_width,
            cell_height,
        };
        // Finder-launched apps get a sparse GUI environment; give shells the
        // captured login-shell env (PATH etc.), with TERM pinned to a
        // universally-known terminfo entry (we don't ship alacritty's).
        let mut env: HashMap<String, String> = superterminal_core::shell_env::shell_env()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        env.insert("TERM".to_string(), "xterm-256color".to_string());
        let options = tty::Options {
            shell,
            working_directory,
            drain_on_exit: true,
            env,
        };
        let pty = tty::new(&options, window_size, 0).map_err(|e| e.to_string())?;
        let shell_pid = pty.child().id() as i32;
        let master_fd = pty.file().as_raw_fd();

        let event_loop =
            EventLoop::new(term.clone(), proxy, pty, true, false).map_err(|e| e.to_string())?;
        let sender = event_loop.channel();
        *writer.lock().unwrap() = Some(sender.clone());
        let io_thread = Some(event_loop.spawn());

        Ok(Self {
            term,
            notifier: Notifier(sender.clone()),
            sender,
            io_thread,
            dirty,
            events,
            deferred: Vec::new(),
            shell_pid,
            master_fd,
            title: None,
            exited: None,
            cols,
            lines,
            shared_window_size,
        })
    }

    /// Write input bytes to the PTY (never blocks; never touches the lock).
    pub fn write(&self, bytes: Vec<u8>) {
        self.notifier.notify(bytes);
    }

    /// True when new output arrived since the last sync (cleared by sync).
    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }

    pub fn queue_resize(&mut self, cols: usize, lines: usize, cell_width: u16, cell_height: u16) {
        if cols == 0 || lines == 0 {
            return;
        }
        self.deferred.push(TermOp::Resize {
            size: TermSize::new(cols, lines),
            window: WindowSize {
                num_cols: cols as u16,
                num_lines: lines as u16,
                cell_width,
                cell_height,
            },
        });
    }

    /// Positive = toward history (scroll up), negative = toward bottom.
    pub fn queue_scroll(&mut self, delta: i32) {
        self.deferred.push(TermOp::Scroll(delta));
    }

    pub fn queue_selection_start(&mut self, col: usize, viewport_row: usize) {
        self.deferred.push(TermOp::StartSelection {
            col,
            row: viewport_row,
        });
    }

    pub fn queue_selection_update(&mut self, col: usize, viewport_row: usize) {
        self.deferred.push(TermOp::UpdateSelection {
            col,
            row: viewport_row,
        });
    }

    pub fn queue_selection_clear(&mut self) {
        self.deferred.push(TermOp::ClearSelection);
    }

    /// Drain pending events (title changes, bell, exit). Cheap; no term lock.
    pub fn drain_events(&mut self) -> Vec<SessionEvent> {
        let drained: Vec<SessionEvent> = std::mem::take(&mut *self.events.lock().unwrap());
        for event in &drained {
            match event {
                SessionEvent::TitleChanged(title) => self.title = Some(title.clone()),
                SessionEvent::Exited(code) => self.exited = Some(*code),
                SessionEvent::Bell => {}
            }
        }
        drained
    }

    /// Current working directory of the foreground process in this terminal
    /// (falls back to the shell process; best-effort).
    pub fn cwd(&self) -> Option<String> {
        if self.exited.is_some() {
            return None;
        }
        let fg = unsafe { tcgetpgrp(self.master_fd) };
        let pid = if fg > 0 { fg } else { self.shell_pid };
        superterminal_core::proc_cwd::pid_cwd(pid)
            .or_else(|| superterminal_core::proc_cwd::pid_cwd(self.shell_pid))
    }

    /// Apply all deferred ops and copy out a render snapshot under ONE lock.
    pub fn sync_and_snapshot(&mut self) -> RenderableSnapshot {
        let mut term = self.term.lock();

        for op in self.deferred.drain(..) {
            match op {
                TermOp::Resize { size, window } => {
                    let (new_cols, new_lines) = (size.columns, size.screen_lines);
                    if new_cols != self.cols || new_lines != self.lines {
                        term.resize(size);
                        *self.shared_window_size.lock().unwrap() = window;
                        let _ = self.sender.send(Msg::Resize(window));
                        self.cols = new_cols;
                        self.lines = new_lines;
                    }
                }
                TermOp::Scroll(delta) => term.scroll_display(Scroll::Delta(delta)),
                TermOp::StartSelection { col, row } => {
                    let point = viewport_to_buffer(&term, col, row);
                    term.selection = Some(Selection::new(
                        SelectionType::Simple,
                        point,
                        alacritty_terminal::index::Side::Left,
                    ));
                }
                TermOp::UpdateSelection { col, row } => {
                    let point = viewport_to_buffer(&term, col, row);
                    if let Some(selection) = term.selection.as_mut() {
                        selection.update(point, alacritty_terminal::index::Side::Right);
                    }
                }
                TermOp::ClearSelection => term.selection = None,
            }
        }

        let cols = term.columns();
        let lines = term.screen_lines();
        let display_offset = term.grid().display_offset();
        let mode = *term.mode();

        let mut rows = vec![
            vec![
                SnapshotCell {
                    ch: ' ',
                    style: CellStyle {
                        fg: CellColor::Default,
                        bg: CellColor::Default,
                        bold: false,
                        italic: false,
                        dim: false,
                        underline: false,
                        inverse: false,
                        hidden: false,
                    },
                    wide_spacer: false,
                };
                cols
            ];
            lines
        ];
        let mut selection_cells = Vec::new();

        {
            let selection_text = term.selection_to_string();
            let content = term.renderable_content();
            let selection_range = content.selection;
            for indexed in content.display_iter {
                // Buffer line -> viewport row.
                let viewport_row = (indexed.point.line.0 + display_offset as i32) as usize;
                let col = indexed.point.column.0;
                if viewport_row >= lines || col >= cols {
                    continue;
                }
                let flags = indexed.cell.flags;
                rows[viewport_row][col] = SnapshotCell {
                    ch: indexed.cell.c,
                    style: CellStyle {
                        fg: convert_color(indexed.cell.fg),
                        bg: convert_color(indexed.cell.bg),
                        bold: flags.contains(Flags::BOLD),
                        italic: flags.contains(Flags::ITALIC),
                        dim: flags.contains(Flags::DIM),
                        underline: flags.intersects(Flags::ALL_UNDERLINES),
                        inverse: flags.contains(Flags::INVERSE),
                        hidden: flags.contains(Flags::HIDDEN),
                    },
                    wide_spacer: flags.contains(Flags::WIDE_CHAR_SPACER),
                };
                if selection_range.is_some_and(|r| r.contains(indexed.point)) {
                    selection_cells.push((col, viewport_row));
                }
            }

            let cursor_line = content.cursor.point.line.0 + display_offset as i32;
            let cursor = SnapshotCursor {
                col: content.cursor.point.column.0,
                row: (cursor_line >= 0 && (cursor_line as usize) < lines)
                    .then_some(cursor_line as usize),
                style: match content.cursor.shape {
                    CursorShape::Hidden => CursorStyle::Hidden,
                    CursorShape::Block | CursorShape::HollowBlock => CursorStyle::Block,
                    CursorShape::Underline => CursorStyle::Underline,
                    CursorShape::Beam => CursorStyle::Bar,
                },
            };

            RenderableSnapshot {
                cols,
                lines,
                rows,
                cursor,
                display_offset,
                selection: selection_cells,
                app_cursor_mode: mode.contains(TermMode::APP_CURSOR),
                mouse_tracking: mode.intersects(TermMode::MOUSE_MODE),
                alt_screen: mode.contains(TermMode::ALT_SCREEN),
                focused_title: self.title.clone(),
                exited: self.exited,
                selection_text,
            }
        }
    }

    /// Contract rev 2 §3: send shutdown, then join off the UI thread with a
    /// bounded deadline; SIGKILL the shell on expiry.
    pub fn shutdown(mut self) -> ShutdownHandle {
        let _ = self.sender.send(Msg::Shutdown);
        ShutdownHandle {
            io_thread: self.io_thread.take(),
            shell_pid: self.shell_pid,
            master_fd: self.master_fd,
        }
    }
}

pub struct ShutdownHandle {
    io_thread: Option<JoinHandle<(EventLoop<tty::Pty, EventProxy>, IoState)>>,
    shell_pid: i32,
    master_fd: c_int,
}

impl ShutdownHandle {
    /// Join with a deadline. The shell is ALWAYS SIGKILLed before the final
    /// join: dropping alacritty's `EventLoop` runs `Pty::drop`, which SIGHUPs
    /// the child and then waits unboundedly — and shells can ignore SIGHUP.
    /// SIGKILL cannot be ignored, so the drop-side wait is guaranteed to
    /// return. Call OFF the UI thread.
    pub fn join_with_deadline(self, deadline: Duration) {
        let Some(handle) = self.io_thread else { return };
        let start = Instant::now();
        while !handle.is_finished() && start.elapsed() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        // SIGKILL the shell (its group first, if it leads one), then release
        // the PTY master by dup2'ing /dev/null over it. A session leader
        // SIGKILLed on a PTY wedges in exit (observed state `Es`) until the
        // master side is released, and alacritty's Pty::drop waits for the
        // child BEFORE dropping the master — so joining would deadlock.
        // dup2 repoints the descriptor without closing it, so Pty keeps sole
        // ownership of the fd number and its eventual close() is the only
        // one (no double-close / IO-safety violation). The IO thread has
        // already stopped (or blown its deadline), so nothing else reads it.
        if let Ok(devnull) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/null")
        {
            unsafe {
                let _ = kill(-self.shell_pid, SIGKILL);
                let _ = kill(self.shell_pid, SIGKILL);
                let _ = dup2(std::os::fd::AsRawFd::as_raw_fd(&devnull), self.master_fd);
            }
        } else {
            unsafe {
                let _ = kill(-self.shell_pid, SIGKILL);
                let _ = kill(self.shell_pid, SIGKILL);
            }
        }
        let _ = handle.join();
    }
}

fn viewport_to_buffer<T>(
    term: &Term<T>,
    col: usize,
    viewport_row: usize,
) -> alacritty_terminal::index::Point {
    let display_offset = term.grid().display_offset();
    let line = Line(viewport_row as i32 - display_offset as i32);
    let col = Column(col.min(term.columns().saturating_sub(1)));
    alacritty_terminal::index::Point::new(line, col)
}

fn convert_color(color: AnsiColor) -> CellColor {
    match color {
        AnsiColor::Named(named) => match named {
            NamedColor::Foreground
            | NamedColor::Background
            | NamedColor::Cursor
            | NamedColor::DimForeground
            | NamedColor::BrightForeground => CellColor::Default,
            NamedColor::Black => CellColor::Indexed(0),
            NamedColor::Red => CellColor::Indexed(1),
            NamedColor::Green => CellColor::Indexed(2),
            NamedColor::Yellow => CellColor::Indexed(3),
            NamedColor::Blue => CellColor::Indexed(4),
            NamedColor::Magenta => CellColor::Indexed(5),
            NamedColor::Cyan => CellColor::Indexed(6),
            NamedColor::White => CellColor::Indexed(7),
            NamedColor::BrightBlack => CellColor::Indexed(8),
            NamedColor::BrightRed => CellColor::Indexed(9),
            NamedColor::BrightGreen => CellColor::Indexed(10),
            NamedColor::BrightYellow => CellColor::Indexed(11),
            NamedColor::BrightBlue => CellColor::Indexed(12),
            NamedColor::BrightMagenta => CellColor::Indexed(13),
            NamedColor::BrightCyan => CellColor::Indexed(14),
            NamedColor::BrightWhite => CellColor::Indexed(15),
            NamedColor::DimBlack => CellColor::Indexed(0),
            NamedColor::DimRed => CellColor::Indexed(1),
            NamedColor::DimGreen => CellColor::Indexed(2),
            NamedColor::DimYellow => CellColor::Indexed(3),
            NamedColor::DimBlue => CellColor::Indexed(4),
            NamedColor::DimMagenta => CellColor::Indexed(5),
            NamedColor::DimCyan => CellColor::Indexed(6),
            NamedColor::DimWhite => CellColor::Indexed(7),
        },
        AnsiColor::Indexed(i) => CellColor::Indexed(i),
        AnsiColor::Spec(rgb) => CellColor::Rgb(rgb.r, rgb.g, rgb.b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PTY spawn/teardown uses process-global signal handling (signal-hook
    /// SIGCHLD registration inside alacritty's tty layer); concurrent
    /// registration/unregistration across test threads can deadlock teardown.
    /// Serialize every PTY test.
    static PTY_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn test_session(cols: usize, lines: usize, cwd: Option<PathBuf>) -> TermSession {
        TermSession::spawn_with_shell(
            cols,
            lines,
            8,
            16,
            cwd,
            Some(tty::Shell::new("/bin/sh".to_string(), Vec::new())),
        )
        .expect("spawn")
    }

    fn wait_for<F: Fn(&RenderableSnapshot) -> bool>(
        session: &mut TermSession,
        predicate: F,
        secs: u64,
    ) -> RenderableSnapshot {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let snapshot = session.sync_and_snapshot();
            if predicate(&snapshot) || Instant::now() > deadline {
                return snapshot;
            }
            std::thread::sleep(Duration::from_millis(40));
        }
    }

    fn row_text(snapshot: &RenderableSnapshot, row: usize) -> String {
        snapshot.rows[row].iter().map(|c| c.ch).collect()
    }

    fn grid_contains(snapshot: &RenderableSnapshot, needle: &str) -> bool {
        (0..snapshot.lines).any(|r| row_text(snapshot, r).contains(needle))
    }

    #[test]
    fn round_trip_output_lands_in_snapshot() {
        let _serial = PTY_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut session = test_session(80, 24, None);
        session.write(b"printf 'NATIVE_%s\\n' OK\r".to_vec());
        let snapshot = wait_for(&mut session, |s| grid_contains(s, "NATIVE_OK"), 15);
        assert!(
            grid_contains(&snapshot, "NATIVE_OK"),
            "grid:\n{}",
            (0..snapshot.lines)
                .map(|r| row_text(&snapshot, r))
                .collect::<Vec<_>>()
                .join("\n")
        );
        assert!(snapshot.cursor.row.is_some());
        session
            .shutdown()
            .join_with_deadline(Duration::from_secs(3));
    }

    #[test]
    fn exit_surfaces_event_and_snapshot_flag() {
        let _serial = PTY_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut session = test_session(80, 24, None);
        session.write(b"exit 7\r".to_vec());
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut exited = None;
        while exited.is_none() && Instant::now() < deadline {
            for event in session.drain_events() {
                if let SessionEvent::Exited(code) = event {
                    exited = Some(code);
                }
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        // /usr/bin/login intermediates the shell, so the observed exit code
        // may be normalized; the event itself is the contract.
        assert!(exited.is_some(), "no exit event within deadline");
        assert!(session.sync_and_snapshot().exited.is_some());
        session
            .shutdown()
            .join_with_deadline(Duration::from_secs(3));
    }

    #[test]
    fn resize_applies_on_sync() {
        let _serial = PTY_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut session = test_session(80, 24, None);
        session.queue_resize(100, 30, 8, 16);
        let snapshot = session.sync_and_snapshot();
        assert_eq!(snapshot.cols, 100);
        assert_eq!(snapshot.lines, 30);
        session
            .shutdown()
            .join_with_deadline(Duration::from_secs(3));
    }

    #[test]
    fn cwd_reports_working_directory() {
        let _serial = PTY_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut session = test_session(80, 24, Some(PathBuf::from("/private/tmp")));
        // Wait for the shell to actually start before asking for its cwd.
        let _ = wait_for(
            &mut session,
            |s| grid_contains(s, "$") || grid_contains(s, "%"),
            10,
        );
        let cwd = session.cwd();
        assert!(
            cwd.as_deref()
                .is_some_and(|c| c.starts_with("/private/tmp") || c.starts_with("/tmp")),
            "cwd: {cwd:?}"
        );
        session
            .shutdown()
            .join_with_deadline(Duration::from_secs(3));
    }
}
