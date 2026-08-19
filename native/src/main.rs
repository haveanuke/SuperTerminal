//! SPIKE: gpui (0.2.2) + alacritty_terminal (0.26.0) native terminal proof.
//!
//! What this proves:
//!   * gpui opens a native macOS window.
//!   * alacritty_terminal spawns $SHELL on a real PTY and parses its output
//!     into a grid on a background thread.
//!   * The grid is rendered as monospace rows of text in the gpui window and
//!     updates live.
//!   * Keyboard input in the window is translated to bytes and written to the
//!     PTY (type `ls`, press enter, see output).
//!
//! Threading model:
//!   * alacritty's `EventLoop::spawn()` runs the PTY reader + VTE parser on
//!     its own OS thread ("PTY reader"). The `Term` grid lives in an
//!     `Arc<FairMutex<Term>>` shared between that thread and the UI.
//!   * The `EventProxy` (alacritty `EventListener`) is invoked on the PTY
//!     thread; it just flips an `Arc<AtomicBool>` dirty flag (and answers
//!     `PtyWrite` requests, e.g. terminal query responses).
//!   * On the gpui side a foreground task polls the dirty flag every 16ms via
//!     `cx.background_executor().timer(..)` and calls `cx.notify()` when set,
//!     which re-renders the view. The render pass takes the FairMutex briefly
//!     to snapshot the grid into plain `String` rows.
//!   * Key input flows main thread -> `Notifier` (mpsc + poller wakeup) ->
//!     PTY writer on the reader thread.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alacritty_terminal::event::{Event as AlacEvent, EventListener, Notify, WindowSize};
use alacritty_terminal::event_loop::{EventLoop as PtyEventLoop, EventLoopSender, Msg, Notifier};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::tty;

use gpui::{
    div, prelude::*, px, rgb, size, App, Application, Bounds, Context, FocusHandle, KeyDownEvent,
    Keystroke, SharedString, TitlebarOptions, Window, WindowBounds, WindowOptions,
};

/// Fixed grid for the spike; the full app must derive this from the window
/// size and measured cell metrics, and resize the Term/PTY on window resize.
const COLS: usize = 100;
const LINES: usize = 30;

/// Approximate Menlo 13px cell metrics, used only to size the window and to
/// tell the PTY its pixel size. Row alignment does NOT depend on these being
/// exact because every row is one monospace text run.
const CELL_WIDTH: f32 = 7.9;
const CELL_HEIGHT: f32 = 18.0;
const PADDING: f32 = 8.0;

/// alacritty `EventListener` -- called on the PTY reader thread.
#[derive(Clone)]
struct EventProxy {
    dirty: Arc<AtomicBool>,
    /// Filled in right after the event loop is constructed; lets us answer
    /// PtyWrite requests (terminal responding to queries from programs).
    writer: Arc<Mutex<Option<EventLoopSender>>>,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: AlacEvent) {
        if let AlacEvent::PtyWrite(text) = event {
            if let Some(sender) = self.writer.lock().unwrap().as_ref() {
                let _ = sender.send(Msg::Input(text.into_bytes().into()));
            }
            return;
        }
        // Wakeup, Title, Bell, ChildExit, ... -> just ask the UI to redraw.
        self.dirty.store(true, Ordering::Release);
    }
}

struct TerminalView {
    term: Arc<FairMutex<Term<EventProxy>>>,
    notifier: Notifier,
    focus_handle: FocusHandle,
}

impl TerminalView {
    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(bytes) = keystroke_to_bytes(&event.keystroke) {
            self.notifier.notify(bytes);
            cx.notify();
        }
    }
}

impl Drop for TerminalView {
    fn drop(&mut self) {
        let _ = self.notifier.0.send(Msg::Shutdown);
    }
}

/// Translate a gpui keystroke into the bytes a terminal expects.
/// Deliberately minimal for the spike: printable text via `key_char`
/// (which already accounts for shift/option layers), plus the handful of
/// control keys needed to drive a shell.
fn keystroke_to_bytes(ks: &Keystroke) -> Option<Vec<u8>> {
    let m = &ks.modifiers;

    // Ctrl-<key> combinations (ctrl-c, ctrl-d, ctrl-l, ...).
    if m.control {
        let mut chars = ks.key.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            if c.is_ascii_alphabetic() {
                return Some(vec![(c.to_ascii_lowercase() as u8) & 0x1f]);
            }
        }
        return match ks.key.as_str() {
            "space" => Some(vec![0x00]),
            "[" => Some(vec![0x1b]),
            _ => None,
        };
    }

    // Named keys.
    match ks.key.as_str() {
        "enter" => return Some(b"\r".to_vec()),
        "backspace" => return Some(vec![0x7f]),
        "delete" => return Some(b"\x1b[3~".to_vec()),
        "escape" => return Some(vec![0x1b]),
        "tab" => return Some(b"\t".to_vec()),
        "up" => return Some(b"\x1b[A".to_vec()),
        "down" => return Some(b"\x1b[B".to_vec()),
        "right" => return Some(b"\x1b[C".to_vec()),
        "left" => return Some(b"\x1b[D".to_vec()),
        "home" => return Some(b"\x1b[H".to_vec()),
        "end" => return Some(b"\x1b[F".to_vec()),
        "space" => return Some(b" ".to_vec()),
        _ => {}
    }

    // Printable input. `key_char` is the typed character (handles shift and
    // option layers); cmd-shortcuts arrive with key_char == None.
    if !m.platform && !m.function {
        if let Some(key_char) = &ks.key_char {
            if !key_char.is_empty() {
                return Some(key_char.as_bytes().to_vec());
            }
        }
    }
    None
}

/// Snapshot the visible grid as one String per row (trailing blanks trimmed)
/// plus a crude block cursor overlay.
fn snapshot_rows(term: &Term<EventProxy>) -> Vec<String> {
    let grid = term.grid();
    let cursor = grid.cursor.point;
    let mut rows = Vec::with_capacity(grid.screen_lines());

    for line in 0..grid.screen_lines() {
        let row = &grid[Line(line as i32)];
        let mut text = String::with_capacity(grid.columns());
        for col in 0..grid.columns() {
            text.push(row[Column(col)].c);
        }
        while text.ends_with(' ') {
            text.pop();
        }
        rows.push(text);
    }

    // Overlay a block cursor when it sits on a blank cell (keeps the spike
    // simple; real cursor rendering is a paint-layer concern in the full app).
    let (cur_line, cur_col) = (cursor.line.0 as usize, cursor.column.0);
    if let Some(row) = rows.get_mut(cur_line) {
        let len = row.chars().count();
        if cur_col >= len {
            row.extend(std::iter::repeat(' ').take(cur_col - len));
            row.push('\u{2588}'); // full block
        }
    }

    // Zero-height divs would break row alignment; keep every row one cell tall.
    for row in rows.iter_mut() {
        if row.is_empty() {
            row.push(' ');
        }
    }
    rows
}

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = snapshot_rows(&self.term.lock());

        div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .bg(rgb(0x14151a))
            .p(px(PADDING))
            .overflow_hidden()
            .flex()
            .flex_col()
            .font_family("Menlo")
            .text_size(px(13.0))
            .line_height(px(CELL_HEIGHT))
            .text_color(rgb(0xd8dee9))
            .children(
                rows.into_iter()
                    .map(|row| div().whitespace_nowrap().child(SharedString::from(row))),
            )
    }
}

fn main() {
    // --- Terminal core setup (before gpui takes over the main thread). ---
    let dirty = Arc::new(AtomicBool::new(true));
    let writer = Arc::new(Mutex::new(None));
    let proxy = EventProxy {
        dirty: dirty.clone(),
        writer: writer.clone(),
    };

    let term_size = TermSize::new(COLS, LINES);
    let term = Arc::new(FairMutex::new(Term::new(
        TermConfig::default(),
        &term_size,
        proxy.clone(),
    )));

    let mut pty_options = tty::Options::default();
    // Spawn the user's shell explicitly; fall back to the passwd default.
    if let Ok(shell) = std::env::var("SHELL") {
        pty_options.shell = Some(tty::Shell::new(shell, Vec::new()));
    }
    // The spike does not ship terminfo, so claim a universally-known terminal.
    pty_options
        .env
        .insert("TERM".into(), "xterm-256color".into());

    let window_size = WindowSize {
        num_lines: LINES as u16,
        num_cols: COLS as u16,
        cell_width: CELL_WIDTH as u16,
        cell_height: CELL_HEIGHT as u16,
    };
    let pty = tty::new(&pty_options, window_size, 0).expect("failed to spawn PTY");

    let event_loop = PtyEventLoop::new(term.clone(), proxy, pty, false, false)
        .expect("failed to create PTY event loop");
    let sender = event_loop.channel();
    *writer.lock().unwrap() = Some(sender.clone());
    let _pty_thread = event_loop.spawn();
    let notifier = Notifier(sender);

    // --- UI. ---
    Application::new().run(move |cx: &mut App| {
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let window_bounds = Bounds::centered(
            None,
            size(
                px(COLS as f32 * CELL_WIDTH + PADDING * 2.0),
                px(LINES as f32 * CELL_HEIGHT + PADDING * 2.0),
            ),
            cx,
        );

        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(window_bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("SuperTerminal spike".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                cx.new(|cx| {
                    let focus_handle = cx.focus_handle();
                    focus_handle.focus(window);

                    // Bridge: PTY thread sets `dirty`; this foreground task
                    // turns it into cx.notify() -> re-render.
                    let dirty = dirty.clone();
                    cx.spawn(async move |view, cx| {
                        loop {
                            cx.background_executor()
                                .timer(Duration::from_millis(16))
                                .await;
                            if dirty.swap(false, Ordering::AcqRel)
                                && view.update(cx, |_, cx| cx.notify()).is_err()
                            {
                                break; // view dropped; stop polling
                            }
                        }
                    })
                    .detach();

                    TerminalView {
                        term,
                        notifier,
                        focus_handle,
                    }
                })
            },
        )
        .expect("failed to open window");

        cx.activate(true);
    });
}
