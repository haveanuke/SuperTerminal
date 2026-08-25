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
use alacritty_terminal::index::{Direction, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::search::RegexSearch;
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

/// Whether newly spawned shells get the tool-adapter shims on PATH. Written
/// by the workspace from settings; a process-wide flag because sessions are
/// spawned deep in pane construction. Toggling affects NEW terminals only.
static TOOL_ADAPTERS: AtomicBool = AtomicBool::new(true);

pub fn set_tool_adapters(enabled: bool) {
    TOOL_ADAPTERS.store(enabled, Ordering::Relaxed);
}

fn adapters_enabled() -> bool {
    TOOL_ADAPTERS.load(Ordering::Relaxed)
}

/// What an instrumented agent says it is doing. The pty cannot answer this:
/// `tcgetpgrp` reports only that SOME app owns the terminal, which is true
/// for the whole hours-long life of a claude session whether it is working
/// or parked at its prompt. The agent's own lifecycle hooks are the only
/// authoritative source, so the adapters make it report.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum AgentState {
    Working,
    Idle,
}

/// Parse an agent state file: `working:1793` / `idle:1793`. The pid is the
/// agent that wrote it, so a stale file left behind by an exited agent can
/// be told apart from the process that owns the tty now.
pub(crate) fn parse_agent_state(raw: &str) -> Option<(AgentState, i32)> {
    let (word, pid) = raw.trim().split_once(':')?;
    let state = match word {
        "working" => AgentState::Working,
        "idle" => AgentState::Idle,
        _ => return None,
    };
    Some((state, pid.trim().parse().ok()?))
}

/// The bundled adapter shims: Resources/adapters in the .app, with a source
/// checkout fallback for `cargo run` dev builds.
fn adapters_dir() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(contents) = exe.parent().and_then(|macos| macos.parent()) {
            let bundled = contents.join("Resources/adapters");
            if bundled.is_dir() {
                return Some(bundled);
            }
        }
    }
    let dev = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/adapters"));
    dev.is_dir().then_some(dev)
}

/// A private per-session file the agent's hooks write their state into.
/// Unique per (app process, session) so two panes never cross-talk and a
/// crashed run cannot poison the next one.
fn new_agent_state_slot() -> Option<(String, PathBuf)> {
    let dir = dirs_cache_dir()?.join("agent-state");
    std::fs::create_dir_all(&dir).ok()?;
    prune_abandoned_state_files(&dir);
    // create_new means we OWN this path: an abandoned file from a previous
    // run of this app pid can never be silently adopted and briefly trusted.
    for _ in 0..8 {
        let id = state_slot_id();
        let path = dir.join(&id);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Some((id, path)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return None,
        }
    }
    None
}

/// The opaque id handed to the adapter. Pure lowercase hex, because that is
/// exactly what the adapter's filter accepts — handing out an ID instead of
/// a path is what makes it impossible for a stray or hostile ST_PANE_ID to
/// name a file at all.
fn state_slot_id() -> String {
    format!("{:016x}{:016x}", std::process::id(), state_nonce())
}

/// Per-session nonce: a monotonic counter mixed with the clock, so neither a
/// reused app pid nor two sessions opened in the same nanosecond collide.
fn state_nonce() -> u64 {
    use std::sync::atomic::AtomicU64;
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    // FNV-1a mix, same primitive the preview store uses for revisions.
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in seq.to_le_bytes().iter().chain(nanos.to_le_bytes().iter()) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

/// Abnormal exits (crash, SIGKILL) leave state files behind; sweep anything
/// older than a day once per app run. Cheap: the directory holds one tiny
/// file per live terminal.
fn prune_abandoned_state_files(dir: &std::path::Path) {
    use std::sync::Once;
    static PRUNED: Once = Once::new();
    PRUNED.call_once(|| {
        const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
        // Never sweep through a symlink: if `agent-state` itself were one,
        // this loop would enumerate and delete files somewhere else entirely.
        // symlink_metadata does not follow it, unlike metadata().
        match std::fs::symlink_metadata(dir) {
            Ok(meta) if !meta.file_type().is_symlink() => {}
            _ => return,
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            // Same reasoning per entry: only plain files we could have
            // created are candidates for removal.
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.file_type().is_file() {
                continue;
            }
            let stale = meta
                .modified()
                .map(|m| m.elapsed().unwrap_or_default() > MAX_AGE)
                .unwrap_or(false);
            if stale {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    });
}

/// Caches live beside the thumbnail cache, under the app's own directory.
fn dirs_cache_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join("Library/Caches")
            .join(crate::settings::APP_DIR_NAME)
    })
}

/// Owns a freshly created state file until the session takes it. PTY and
/// event-loop construction can both fail after the file exists; without this
/// the file survives until the 24h sweep.
struct StateFileGuard(Option<PathBuf>);

impl StateFileGuard {
    fn take(&mut self) -> Option<PathBuf> {
        self.0.take()
    }
}

impl Drop for StateFileGuard {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

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
    /// DECSET 2004: the foreground app wants pastes framed in ESC[200~/201~.
    pub bracketed_paste: bool,
    pub mouse_tracking: bool,
    /// True while the alternate screen buffer is active (vim, htop, ...).
    pub alt_screen: bool,
    pub focused_title: Option<String>,
    /// Exit code once the shell has terminated.
    pub exited: Option<i32>,
    /// Text covered by the active selection (captured under the same lock).
    pub selection_text: Option<String>,
    /// Cells covered by search matches in the viewport, as (col, row) pairs.
    pub search_matches: Vec<(usize, usize)>,
    /// Scrollback tail for the phone: up to [`HISTORY_TAIL`] lines just above
    /// the LIVE screen, oldest first. Populated only on the companion sync
    /// path, and only on the snapshot the phone publishes — the Mac renderer
    /// never pays for it.
    pub history_rows: Vec<Vec<SnapshotCell>>,
}

/// How many scrollback lines ride the companion snapshot ("a few screens").
pub const HISTORY_TAIL: usize = 150;

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
    /// Active search: compiled regex (escaped literal) + the raw needle.
    search: Option<(RegexSearch, String)>,
    /// Where this session's instrumented agent reports working/idle. Removed
    /// when the session shuts down so a dead pane leaves nothing behind.
    agent_state_path: Option<PathBuf>,
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
        env.insert("TERM_PROGRAM".to_string(), "SuperTerminal".to_string());
        // Tool adapters: shims that make claude/codex ring the terminal bell
        // with zero user setup. The pre-shim PATH rides along so an adapter
        // resolves the REAL binary and can never recurse into itself. New
        // sessions only — an already-running shell keeps its PATH.
        // The adapters' hooks report agent working/idle here. Only set when
        // adapters are on: without the shims nothing writes it, and a stale
        // variable would point hooks at a file nobody reads.
        // Clear any INHERITED values first, unconditionally. The PTY env
        // starts from the login shell's, so stale values would otherwise
        // ride along — including when slot creation below fails.
        env.remove("ST_PANE_STATE");
        env.remove("ST_PANE_ID");
        // Guarded: every `?` between here and the constructed session drops
        // it, removing the file.
        let mut agent_state_path = StateFileGuard(None);
        if adapters_enabled() {
            if let Some(dir) = adapters_dir() {
                let orig = env.get("PATH").cloned().unwrap_or_default();
                env.insert("ST_ORIG_PATH".to_string(), orig.clone());
                env.insert("PATH".to_string(), format!("{}:{orig}", dir.display()));
            }
            if let Some((id, path)) = new_agent_state_slot() {
                env.insert("ST_PANE_ID".to_string(), id);
                agent_state_path = StateFileGuard(Some(path));
            }
        }
        // App bundles launch with cwd "/" — a shell must never start there.
        // No explicit directory means the user's home.
        let working_directory =
            working_directory.or_else(|| std::env::var_os("HOME").map(PathBuf::from));
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
            search: None,
            agent_state_path: agent_state_path.take(),
        })
    }

    /// Write input bytes to the PTY (never blocks; never touches the lock).
    pub fn write(&self, bytes: Vec<u8>) {
        self.notifier.notify(bytes);
    }

    /// A cloned input handle for broadcast fan-out.
    pub fn input_sender(&self) -> EventLoopSender {
        self.sender.clone()
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

    /// True once the shell process has exited.
    pub fn is_exited(&self) -> bool {
        self.exited.is_some()
    }

    /// Busy probe WITHOUT the cwd lookup: one tcgetpgrp, nothing else —
    /// cheap enough for an always-on poll.
    pub fn foreground_busy(&self) -> bool {
        if self.exited.is_some() {
            return false;
        }
        let fg = unsafe { tcgetpgrp(self.master_fd) };
        fg > 0 && fg != self.shell_pid
    }

    /// What the instrumented agent in this session last reported, if any.
    /// None means "nothing has reported" — the caller falls back to its
    /// heuristic rather than assuming either state.
    pub fn agent_state(&self) -> Option<(AgentState, i32)> {
        let path = self.agent_state_path.as_ref()?;
        // The shell owns the tty: no agent is running, so whatever the file
        // says is a leftover. Dropping it here means a later process that
        // happens to reuse the old pid can never inherit its state — a
        // reused pid must pass through this prompt first.
        if !self.foreground_busy() {
            let _ = std::fs::remove_file(path);
            return None;
        }
        let raw = std::fs::read_to_string(path).ok()?;
        parse_agent_state(&raw)
    }

    /// The process group that currently owns the tty (0 when none does).
    pub fn foreground_pgid(&self) -> i32 {
        if self.exited.is_some() {
            return 0;
        }
        let fg = unsafe { tcgetpgrp(self.master_fd) };
        fg.max(0)
    }

    /// One ioctl for both answers: (cwd, foreground-job-owns-terminal).
    /// A failed tcgetpgrp reads as "no job" (ready) — indistinguishable
    /// from the prompt without shell integration.
    pub fn status(&self) -> (Option<String>, bool) {
        if self.exited.is_some() {
            return (None, false);
        }
        let fg = unsafe { tcgetpgrp(self.master_fd) };
        let busy = fg > 0 && fg != self.shell_pid;
        let pid = if fg > 0 { fg } else { self.shell_pid };
        let cwd = superterminal_core::proc_cwd::pid_cwd(pid)
            .or_else(|| superterminal_core::proc_cwd::pid_cwd(self.shell_pid));
        (cwd, busy)
    }

    /// Where the focused terminal is (foreground process's directory,
    /// falling back to the shell's).
    pub fn cwd(&self) -> Option<String> {
        self.status().0
    }

    /// Whether UI->terminal ops (resize, scroll, selection) await the next
    /// sync — render must sync itself then, even when a companion publish
    /// already refreshed the snapshot this frame.
    pub fn has_pending_ops(&self) -> bool {
        !self.deferred.is_empty()
    }

    /// Apply all deferred ops and copy out a render snapshot under ONE lock.
    pub fn sync_and_snapshot(&mut self) -> RenderableSnapshot {
        self.sync_impl(false).0
    }

    /// Same single per-frame lock as [`Self::sync_and_snapshot`]; the second
    /// value is the LIVE screen (display offset ignored) whenever the host
    /// is scrolled back — the phone companion publishes that, never the
    /// Mac's scrollback viewport. None means the display IS live. Only this
    /// path pays for the history tail, and it lands on whichever snapshot
    /// the phone publishes.
    pub fn sync_and_snapshot_with_live(
        &mut self,
    ) -> (RenderableSnapshot, Option<RenderableSnapshot>) {
        self.sync_impl(true)
    }

    fn sync_impl(
        &mut self,
        with_history: bool,
    ) -> (RenderableSnapshot, Option<RenderableSnapshot>) {
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

            let cursor_raw_line = content.cursor.point.line.0;
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

            let mut search_matches = Vec::new();
            if let Some((_, needle)) = &self.search {
                // Char-wise case-folded scan: columns must map 1:1 to grid
                // cells, so never lowercase the whole row (one-to-many
                // expansions like 'İ' would shift every column after them).
                let needle_chars: Vec<char> = needle.chars().collect();
                if !needle_chars.is_empty() {
                    for (row_index, row) in rows.iter().enumerate() {
                        let row_chars: Vec<char> = row.iter().map(|cell| cell.ch).collect();
                        let mut col = 0;
                        while col + needle_chars.len() <= row_chars.len() {
                            let matched = needle_chars
                                .iter()
                                .zip(&row_chars[col..])
                                .all(|(n, c)| n == c || n.to_lowercase().eq(c.to_lowercase()));
                            if matched {
                                for offset in 0..needle_chars.len() {
                                    search_matches.push((col + offset, row_index));
                                }
                                col += needle_chars.len();
                            } else {
                                col += 1;
                            }
                        }
                    }
                }
            }

            let live_cursor_style = cursor.style;
            let mut display = RenderableSnapshot {
                cols,
                lines,
                rows,
                cursor,
                display_offset,
                selection: selection_cells,
                app_cursor_mode: mode.contains(TermMode::APP_CURSOR),
                bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
                mouse_tracking: mode.intersects(TermMode::MOUSE_MODE),
                alt_screen: mode.contains(TermMode::ALT_SCREEN),
                focused_title: self.title.clone(),
                exited: self.exited,
                selection_text,
                search_matches: search_matches.clone(),
                history_rows: Vec::new(),
            };
            // Live screen for the companion: direct grid indexing (Line 0..
            // lines are the live rows regardless of scrollback position).
            let mut live = (display_offset > 0).then(|| {
                let grid = term.grid();
                let live_rows = (0..lines)
                    .map(|line| {
                        let row = &grid[Line(line as i32)];
                        (0..cols)
                            .map(|col| snapshot_cell(&row[Column(col)]))
                            .collect()
                    })
                    .collect();
                RenderableSnapshot {
                    cols,
                    lines,
                    rows: live_rows,
                    cursor: SnapshotCursor {
                        col: display.cursor.col,
                        row: (cursor_raw_line >= 0 && (cursor_raw_line as usize) < lines)
                            .then_some(cursor_raw_line as usize),
                        style: live_cursor_style,
                    },
                    display_offset: 0,
                    selection: Vec::new(),
                    app_cursor_mode: mode.contains(TermMode::APP_CURSOR),
                    bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
                    mouse_tracking: mode.intersects(TermMode::MOUSE_MODE),
                    alt_screen: mode.contains(TermMode::ALT_SCREEN),
                    focused_title: self.title.clone(),
                    exited: self.exited,
                    selection_text: None,
                    search_matches: Vec::new(),
                    history_rows: Vec::new(),
                }
            });
            if with_history {
                // The tail is always relative to the LIVE screen (negative
                // grid lines), so Mac-side scrollback never shifts it.
                let grid = term.grid();
                let avail = grid.history_size().min(HISTORY_TAIL);
                let tail: Vec<Vec<SnapshotCell>> = (1..=avail)
                    .rev()
                    .map(|back| {
                        let row = &grid[Line(-(back as i32))];
                        (0..cols)
                            .map(|col| snapshot_cell(&row[Column(col)]))
                            .collect()
                    })
                    .collect();
                match live.as_mut() {
                    Some(live) => live.history_rows = tail,
                    None => display.history_rows = tail,
                }
            }
            (display, live)
        }
    }

    /// Set (or clear) the search needle. Case-insensitive literal match.
    pub fn set_search(&mut self, needle: Option<&str>) {
        self.search = needle.filter(|n| !n.is_empty()).and_then(|needle| {
            let escaped: String = needle
                .chars()
                .flat_map(|c| {
                    let escape = "\\.+*?()|[]{}^$#&-~".contains(c);
                    escape.then_some('\\').into_iter().chain(std::iter::once(c))
                })
                .collect();
            RegexSearch::new(&format!("(?i){escaped}"))
                .ok()
                .map(|regex| (regex, needle.to_string()))
        });
    }

    /// Jump the viewport to the next match above the current view (wrapping
    /// to the bottom of history when none is found).
    pub fn search_jump_next(&mut self) {
        let Some((regex, _)) = self.search.as_mut() else {
            return;
        };
        let mut term = self.term.lock();
        let display_offset = term.grid().display_offset() as i32;
        let origin = alacritty_terminal::index::Point::new(Line(-display_offset - 1), Column(0));
        if let Some(matched) = term.search_next(regex, origin, Direction::Left, Side::Left, None) {
            let target_line = matched.start().line.0;
            let delta = -target_line - display_offset;
            term.scroll_display(Scroll::Delta(delta));
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
            // Removed only AFTER the join below: a hook still running during
            // shutdown would otherwise recreate the file we just deleted.
            agent_state_path: self.agent_state_path.take(),
        }
    }
}

pub struct ShutdownHandle {
    io_thread: Option<JoinHandle<(EventLoop<tty::Pty, EventProxy>, IoState)>>,
    shell_pid: i32,
    master_fd: c_int,
    agent_state_path: Option<PathBuf>,
}

impl ShutdownHandle {
    /// Join with a deadline. The shell is ALWAYS SIGKILLed before the final
    /// join: dropping alacritty's `EventLoop` runs `Pty::drop`, which SIGHUPs
    /// the child and then waits unboundedly — and shells can ignore SIGHUP.
    /// SIGKILL cannot be ignored, so the drop-side wait is guaranteed to
    /// return. Call OFF the UI thread.
    pub fn join_with_deadline(mut self, deadline: Duration) {
        let state_path = self.agent_state_path.take();
        let remove_state = || {
            if let Some(path) = &state_path {
                let _ = std::fs::remove_file(path);
            }
        };
        let Some(handle) = self.io_thread else {
            remove_state();
            return;
        };
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
        // Now that the shell and every hook it spawned are dead, nothing can
        // recreate the file.
        remove_state();
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

/// One grid cell -> snapshot cell, shared by the live-screen and
/// history-tail copies (the display path maps through `renderable_content`).
fn snapshot_cell(cell: &alacritty_terminal::term::cell::Cell) -> SnapshotCell {
    let flags = cell.flags;
    SnapshotCell {
        ch: cell.c,
        style: CellStyle {
            fg: convert_color(cell.fg),
            bg: convert_color(cell.bg),
            bold: flags.contains(Flags::BOLD),
            italic: flags.contains(Flags::ITALIC),
            dim: flags.contains(Flags::DIM),
            underline: flags.intersects(Flags::ALL_UNDERLINES),
            inverse: flags.contains(Flags::INVERSE),
            hidden: flags.contains(Flags::HIDDEN),
        },
        wide_spacer: flags.contains(Flags::WIDE_CHAR_SPACER),
    }
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
mod agent_state_tests {
    use super::*;

    #[test]
    fn parses_both_states_with_the_owning_pid() {
        assert_eq!(
            parse_agent_state("working:1793"),
            Some((AgentState::Working, 1793))
        );
        assert_eq!(
            parse_agent_state("idle:1793"),
            Some((AgentState::Idle, 1793))
        );
        // Hooks write with echo as often as printf; a trailing newline is
        // not a parse failure.
        assert_eq!(
            parse_agent_state("working:42\n"),
            Some((AgentState::Working, 42))
        );
    }

    /// Run the real adapter against a stub `claude` that records its argv,
    /// and hand back what the adapter actually passed. This is the only way
    /// to test the shell quoting: the hook command must survive the adapter
    /// UNEXPANDED and still be valid JSON.
    fn run_adapter(pane_id: Option<&str>, extra: &[&str]) -> Vec<String> {
        let home = fake_home();
        let argv = run_adapter_in(&home, pane_id, extra).0;
        // The fake HOME is this call's alone; leaving it behind litters the
        // system temp dir a few entries per test run.
        let _ = std::fs::remove_dir_all(&home);
        argv
    }

    /// A throwaway HOME with the app's cache layout already in place. Tests
    /// never touch the real `~/Library/Caches`.
    fn fake_home() -> PathBuf {
        let home =
            std::env::temp_dir().join(format!("st-home-{}-{}", std::process::id(), state_nonce()));
        std::fs::create_dir_all(state_dir_under(&home)).unwrap();
        home
    }

    fn state_dir_under(home: &std::path::Path) -> PathBuf {
        home.join("Library/Caches")
            .join(crate::settings::APP_DIR_NAME)
            .join("agent-state")
    }

    fn run_adapter_in(
        home: &std::path::Path,
        pane_id: Option<&str>,
        extra: &[&str],
    ) -> (Vec<String>, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "st-adapter-test-{}-{}",
            std::process::id(),
            state_nonce()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let argv_log = dir.join("argv");
        let stub = dir.join("claude");
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > {}\n",
                argv_log.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let adapter = adapters_dir().expect("adapters dir").join("claude");
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg(&adapter)
            .args(extra)
            // ST_ORIG_PATH is what the adapter resolves `claude` through, so
            // it points at the stub alone; PATH still needs the system tools
            // the adapter script itself runs.
            .env("ST_ORIG_PATH", &dir)
            .env("HOME", home)
            // A collating locale, deliberately: POSIX range expressions are
            // collation-dependent, so running the hostile-id cases under
            // this is what proves the adapter's character class is spelled
            // out rather than written as [0-9a-f].
            .env("LC_ALL", "en_US.UTF-8")
            .env("LC_COLLATE", "en_US.UTF-8")
            .env("PATH", format!("{}:/usr/bin:/bin", dir.display()));
        match pane_id {
            Some(value) => cmd.env("ST_PANE_ID", value),
            None => cmd.env_remove("ST_PANE_ID"),
        };
        assert!(cmd.status().unwrap().success(), "adapter should exec stub");
        let recorded = std::fs::read_to_string(&argv_log).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        let state = pane_id
            .map(|id| state_dir_under(home).join(id))
            .unwrap_or_default();
        (recorded.lines().map(str::to_string).collect(), state)
    }

    /// A unique path inside the real state directory — the only place the
    /// adapter will honor, by design.
    fn settings_of(argv: &[String]) -> serde_json::Value {
        let idx = argv
            .iter()
            .position(|a| a == "--settings")
            .expect("--settings");
        serde_json::from_str(&argv[idx + 1]).expect("settings must be valid JSON")
    }

    #[test]
    fn adapter_injects_hooks_that_stay_unexpanded() {
        let settings = settings_of(&run_adapter(Some("abc123"), &[]));
        assert_eq!(settings["preferredNotifChannel"], "terminal_bell");
        for event in ["UserPromptSubmit", "Stop", "StopFailure", "SessionEnd"] {
            let command = settings["hooks"][event][0]["hooks"][0]["command"]
                .as_str()
                .unwrap_or_else(|| panic!("{event} hook command"));
            // The adapter must NOT expand these: the hook shell resolves them
            // at run time, in the agent's process.
            assert!(command.contains("$PPID"), "{event}: {command}");
            assert!(command.contains("$ST_PANE_STATE"), "{event}: {command}");
        }
        assert!(
            settings["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains("working:")
        );
        // Every terminal event must CLEAR the state. Missing one strands the
        // dot on "working" while claude waits at its prompt — the exact bug
        // this feature exists to fix, in reverse.
        for event in ["Stop", "StopFailure", "SessionEnd"] {
            assert!(
                settings["hooks"][event][0]["hooks"][0]["command"]
                    .as_str()
                    .unwrap()
                    .contains("idle:"),
                "{event} must clear the state"
            );
        }
        // Notification stays out: it fires mid-turn for permission prompts.
        assert!(settings["hooks"].get("Notification").is_none());
    }

    #[test]
    fn adapter_clears_stale_state_before_launching() {
        // A previous claude interrupted with Ctrl-C runs no Stop hook and
        // leaves "working" behind; the next launch must not inherit it.
        let home = fake_home();
        let stale = state_dir_under(&home).join("beef01");
        std::fs::write(&stale, "working:999").unwrap();
        let _ = run_adapter_in(&home, Some("beef01"), &[]);
        let left = std::fs::read_to_string(&stale).unwrap_or_default();
        assert!(
            parse_agent_state(&left).is_none(),
            "stale state survived the launch: {left:?}"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_passthrough_still_clears_stale_state() {
        // The --settings passthrough execs early; if it skipped the clear,
        // an interrupted predecessor's state would survive that launch.
        let home = fake_home();
        let stale = state_dir_under(&home).join("beef02");
        std::fs::write(&stale, "working:999").unwrap();
        let _ = run_adapter_in(&home, Some("beef02"), &["--settings", "{\"mine\":1}"]);
        let left = std::fs::read_to_string(&stale).unwrap_or_default();
        assert!(
            parse_agent_state(&left).is_none(),
            "passthrough skipped the clear: {left:?}"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_adapters_confinement_path_matches_the_app() {
        // The adapter hardcodes the cache directory it will honor. If
        // APP_DIR_NAME ever changes, reporting would silently stop instead
        // of failing loudly, so pin them together here.
        let script = std::fs::read_to_string(adapters_dir().unwrap().join("claude")).unwrap();
        assert!(script.contains(&format!(
            "/Library/Caches/{}/agent-state",
            crate::settings::APP_DIR_NAME
        )));
    }

    #[test]
    fn the_app_hands_out_hex_ids_the_adapter_accepts() {
        // The two halves of the contract, proven end to end and WITHOUT
        // touching the real cache: minting is a pure function, and the
        // adapter must actually honor what it produces.
        let id = state_slot_id();
        assert!(
            !id.is_empty()
                && id
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "id must be pure lowercase hex, got {id:?}"
        );
        let home = fake_home();
        let settings = settings_of(&run_adapter_in(&home, Some(&id), &[]).0);
        assert!(
            settings["hooks"].is_object(),
            "the adapter must accept an id the app minted: {id}"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_non_hex_id_can_never_name_a_file() {
        // THE point of handing out an id instead of a path: traversal is
        // impossible because the id cannot contain a separator and survive.
        let home = fake_home();
        let victim = home.join("precious.txt");
        std::fs::write(&victim, "precious").unwrap();
        for hostile in [
            "../../../precious.txt",
            "../../precious",
            "/etc/passwd",
            "beef; rm -rf /",
            "BEEF",
            "",
        ] {
            let settings = settings_of(&run_adapter_in(&home, Some(hostile), &[]).0);
            assert!(
                settings.get("hooks").is_none(),
                "hostile id {hostile:?} must disable reporting"
            );
        }
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "precious",
            "no hostile id may reach a file"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_symlinked_slot_is_refused() {
        // Inside the cache dir a planted link would redirect the write.
        let home = fake_home();
        let victim = home.join("precious.txt");
        std::fs::write(&victim, "precious").unwrap();
        let link = state_dir_under(&home).join("beef03");
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        let settings = settings_of(&run_adapter_in(&home, Some("beef03"), &[]).0);
        assert!(settings.get("hooks").is_none(), "symlink must be refused");
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn adapter_without_an_id_injects_no_hooks() {
        let settings = settings_of(&run_adapter(None, &[]));
        assert_eq!(settings["preferredNotifChannel"], "terminal_bell");
        assert!(settings.get("hooks").is_none(), "nothing writes the state");
    }

    #[test]
    fn a_user_supplied_settings_is_never_fought() {
        let argv = run_adapter(Some("beef04"), &["--settings", "{\"mine\":1}"]);
        assert_eq!(
            argv.iter().filter(|a| *a == "--settings").count(),
            1,
            "must not append a second --settings: {argv:?}"
        );
        assert_eq!(settings_of(&argv)["mine"], 1);
    }

    #[test]
    fn refuses_anything_it_cannot_trust() {
        // Garbage must fall back to the heuristic, never guess a state.
        for raw in [
            "",
            "working",
            "1793",
            "busy:1793",
            "working:",
            "working:abc",
        ] {
            assert_eq!(parse_agent_state(raw), None, "should refuse {raw:?}");
        }
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
