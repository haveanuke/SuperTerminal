//! Headless proof that the alacritty_terminal side of the spike works:
//! spawn a real shell on a PTY, write a command into it, and observe the
//! command's OUTPUT (not its echo) land in the Term grid.
//!
//! The command is `printf 'SPIKE_%s\n' OK` -- the typed line never contains
//! the literal "SPIKE_OK", so finding "SPIKE_OK" in the grid proves the full
//! round trip: UI-side write -> PTY -> shell -> PTY -> VTE parser -> grid.
//!
//! This mirrors exactly the wiring `src/main.rs` uses under the gpui window.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, EventListener, Notify, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, Msg, Notifier};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::tty;

#[derive(Clone)]
struct Proxy(Arc<AtomicBool>);

impl EventListener for Proxy {
    fn send_event(&self, _event: Event) {
        self.0.store(true, Ordering::Release);
    }
}

fn grid_text(term: &Term<Proxy>) -> String {
    let grid = term.grid();
    let mut text = String::new();
    for line in 0..grid.screen_lines() {
        let row = &grid[Line(line as i32)];
        for col in 0..grid.columns() {
            text.push(row[Column(col)].c);
        }
        text.push('\n');
    }
    text
}

#[test]
fn pty_roundtrip_lands_in_grid() {
    const COLS: usize = 80;
    const LINES: usize = 24;

    let proxy = Proxy(Arc::new(AtomicBool::new(false)));
    let term = Arc::new(FairMutex::new(Term::new(
        Config::default(),
        &TermSize::new(COLS, LINES),
        proxy.clone(),
    )));

    // /bin/sh for determinism (user shells may have exotic prompts/plugins).
    let options = tty::Options {
        shell: Some(tty::Shell::new("/bin/sh".into(), Vec::new())),
        env: std::collections::HashMap::from([("TERM".to_string(), "dumb".to_string())]),
        ..Default::default()
    };

    let window_size = WindowSize {
        num_lines: LINES as u16,
        num_cols: COLS as u16,
        cell_width: 8,
        cell_height: 16,
    };
    let pty = tty::new(&options, window_size, 0).expect("failed to open PTY");

    let event_loop =
        EventLoop::new(term.clone(), proxy, pty, false, false).expect("failed to build event loop");
    let sender = event_loop.channel();
    let _pty_thread = event_loop.spawn();
    let notifier = Notifier(sender.clone());

    // Type the command. PTY input is buffered, so racing shell startup is fine.
    notifier.notify(b"printf 'SPIKE_%s\\n' OK\r".to_vec());

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut found = false;
    let mut last_grid = String::new();
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        last_grid = grid_text(&term.lock());
        // Exact-line match: the output "SPIKE_OK" starts at column 0; the
        // echoed input line contains "SPIKE_%s" instead, so this cannot
        // false-positive on the echo.
        if last_grid.lines().any(|line| line.trim_end() == "SPIKE_OK") {
            found = true;
            break;
        }
    }

    let _ = sender.send(Msg::Shutdown);
    assert!(
        found,
        "SPIKE_OK never appeared in the terminal grid.\nFinal grid:\n{last_grid}"
    );
}
