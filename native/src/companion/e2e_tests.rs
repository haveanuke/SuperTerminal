//! End-to-end proof: a REAL shell session, published through the real hub,
//! served over the real server — phone-side input round-trips to visible
//! grid content over SSE. The test thread plays the pane pump's role.

#![cfg(test)]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::hub::Hub;
use super::server::{start, ServerConfig, INPUT_CONTENT_TYPE};
use crate::term_session::TermSession;

const TOKEN: &str = "e2e0e2e0e2e0e2e0e2e0e2e0e2e0e2e0";

fn post_text(host: &str, id: &str, text: &str) -> String {
    let body = serde_json::json!({ "text": text }).to_string();
    let mut stream = TcpStream::connect(host).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .write_all(
            format!(
                "POST /input/{id} HTTP/1.1\r\nHost: {host}\r\nX-Companion-Token: {TOKEN}\r\nContent-Type: {INPUT_CONTENT_TYPE}\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .unwrap();
    let mut out = String::new();
    let _ = std::io::Read::read_to_string(&mut stream, &mut out);
    out
}

#[test]
fn phone_input_round_trips_to_sse_snapshot() {
    let mut session = TermSession::spawn(80, 24, 8, 16, None).expect("session spawns");
    let hub = Arc::new(Hub::new());
    hub.register("t1", "e2e", session.input_sender());
    let handle = start(
        Arc::clone(&hub),
        crate::themes::default_theme(),
        ServerConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            token: TOKEN.into(),
            page: "<title>e2e</title>",
        },
    )
    .expect("server starts");
    let host = handle
        .url
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string();

    // Session list carries our label.
    {
        let mut stream = TcpStream::connect(&host).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .write_all(
                format!("GET /sessions?t={TOKEN} HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes(),
            )
            .unwrap();
        let mut out = String::new();
        let _ = std::io::Read::read_to_string(&mut stream, &mut out);
        assert!(out.contains("\"label\":\"e2e\""), "{out}");
    }

    // SSE reader thread collects data frames.
    let (frames_tx, frames_rx) = mpsc::channel::<String>();
    let sse_host = host.clone();
    std::thread::spawn(move || {
        let stream = TcpStream::connect(&sse_host).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        let mut writer = stream.try_clone().unwrap();
        writer
            .write_all(
                format!("GET /stream/t1?t={TOKEN} HTTP/1.1\r\nHost: {sse_host}\r\n\r\n").as_bytes(),
            )
            .unwrap();
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if line.starts_with("data:") && frames_tx.send(line).is_err() {
                break;
            }
        }
    });

    // The phone types a command whose OUTPUT (not echo) proves the trip.
    let response = post_text(&host, "t1", "printf 'E2E_%s\\n' OK\r");
    assert!(response.starts_with("HTTP/1.1 204"), "{response}");

    // Play the pane pump: drain dirty -> publish, while watching frames.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut seen = false;
    while Instant::now() < deadline && !seen {
        if session.take_dirty() {
            let snapshot = session.sync_and_snapshot();
            hub.publish_snapshot("t1", Arc::new(snapshot));
        }
        while let Ok(frame) = frames_rx.try_recv() {
            if frame.contains("E2E_OK") {
                seen = true;
            }
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    assert!(seen, "E2E_OK never arrived over SSE");

    // Pane closure path: retire flips input to 410 while the entry lives...
    hub.retire("t1");
    let gone = post_text(&host, "t1", "x");
    assert!(gone.starts_with("HTTP/1.1 410"), "{gone}");
    // ...and the workspace sweep's unregister ends the live stream: the SSE
    // reader thread sees EOF and drops its channel sender.
    hub.unregister("t1");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match frames_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(_) => continue, // drain frames already in flight
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                assert!(Instant::now() < deadline, "stream never terminated");
            }
        }
    }

    handle.stop();
    session
        .shutdown()
        .join_with_deadline(Duration::from_secs(5));
}

#[test]
fn scrolled_back_host_still_publishes_the_live_screen() {
    let mut session = TermSession::spawn(80, 24, 8, 16, None).expect("session spawns");
    // 200 tagged lines so the viewport is deep in scrollback territory.
    session.write(b"printf 'L_%s\\n' $(seq 1 200)\r".to_vec());
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut latest = None;
    while Instant::now() < deadline {
        if session.take_dirty() {
            let snapshot = session.sync_and_snapshot();
            let text: String = snapshot
                .rows
                .iter()
                .flat_map(|row| row.iter().map(|cell| cell.ch))
                .collect();
            let done = text.contains("L_200");
            latest = Some(text);
            if done {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    assert!(
        latest.as_deref().is_some_and(|text| text.contains("L_200")),
        "output never arrived"
    );

    // Host scrolls back; the display shows history, the live copy must not.
    session.queue_scroll(60);
    let (display, live) = session.sync_and_snapshot_with_live();
    assert!(display.display_offset > 0, "scrollback did not engage");
    let display_text: String = display
        .rows
        .iter()
        .flat_map(|row| row.iter().map(|cell| cell.ch))
        .collect();
    assert!(
        !display_text.contains("L_200"),
        "display should be showing history"
    );
    let live = live.expect("scrolled-back sync must supply the live screen");
    assert_eq!(live.display_offset, 0);
    let live_text: String = live
        .rows
        .iter()
        .flat_map(|row| row.iter().map(|cell| cell.ch))
        .collect();
    assert!(
        live_text.contains("L_200"),
        "live screen must keep the latest output"
    );

    session
        .shutdown()
        .join_with_deadline(Duration::from_secs(5));
}

#[test]
fn history_tail_rides_the_companion_snapshot() {
    let mut session = TermSession::spawn(80, 24, 8, 16, None).expect("session spawns");
    // 200 tagged lines: ~176 land in scrollback above the 24-line viewport.
    session.write(b"printf 'L_%s\\n' $(seq 1 200)\r".to_vec());
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut display = None;
    while Instant::now() < deadline {
        if session.take_dirty() {
            let (snap, live) = session.sync_and_snapshot_with_live();
            assert!(live.is_none(), "host never scrolled back");
            let text: String = snap
                .rows
                .iter()
                .flat_map(|row| row.iter().map(|cell| cell.ch))
                .collect();
            let done = text.contains("L_200");
            display = Some(snap);
            if done {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    let display = display.expect("output never arrived");
    let hist_text: String = display
        .history_rows
        .iter()
        .flat_map(|row| row.iter().map(|cell| cell.ch))
        .collect();
    assert!(display.history_rows.len() <= 150, "tail is capped at 150");
    assert!(
        hist_text.contains("L_100"),
        "lines above the viewport must ride the tail"
    );

    // The Mac-only sync never pays for the tail.
    let plain = session.sync_and_snapshot();
    assert!(plain.history_rows.is_empty(), "renderer path stays lean");

    // Scrolled back, the tail rides the LIVE snapshot the phone publishes.
    session.queue_scroll(60);
    let (display2, live2) = session.sync_and_snapshot_with_live();
    assert!(display2.history_rows.is_empty());
    let live2 = live2.expect("scrolled-back sync must supply the live screen");
    assert!(
        !live2.history_rows.is_empty(),
        "tail follows the live screen"
    );

    session
        .shutdown()
        .join_with_deadline(Duration::from_secs(5));
}

#[test]
fn page_renders_the_history_tail_above_the_live_screen() {
    let page = include_str!("page.html");
    // The page consumes the wire's history rows...
    assert!(page.contains("snap.history"), "history rows are consumed");
    // ...grows row containers in place instead of resetting the grid as
    // history fills...
    assert!(
        page.contains("function ensureRows"),
        "row containers grow without a full reset"
    );
    // ...and renders the tail visually receded from the live screen.
    assert!(page.contains("HIST_DIM"), "history rows render dimmed");
}

#[test]
fn page_gives_the_grid_the_whole_viewport() {
    let page = include_str!("page.html");
    // Full-height flex shell: the terminal screen is a 100dvh column where
    // the grid area flexes and header/key pad stay fixed-size.
    assert!(
        page.contains("100dvh"),
        "term screen is a full-height column"
    );
    // The grid pane owns its own scrolling and takes every spare pixel.
    assert!(page.contains("#gridwrap { flex:1"), "gridwrap flexes");
    // Sticky chrome paints above the grid's absolutely-positioned spans —
    // without a z-index the back chevron gets covered once scrolled.
    assert!(page.contains("z-index"), "header stacks above grid spans");
    // Fit-to-width font sizing with a readable floor.
    assert!(
        page.contains("function fitSize"),
        "fit-to-width sizing exists"
    );
    // Follow-the-tail: new output scrolls into view unless the user is
    // actively looking elsewhere.
    assert!(page.contains("function follow"), "tail-follow exists");
    // The key rows collapse to reclaim vertical space; the toggle survives.
    assert!(page.contains("id=\"keys\""), "key rows are wrapped");
    assert!(page.contains("id=\"padtoggle\""), "key pad toggle exists");
}
