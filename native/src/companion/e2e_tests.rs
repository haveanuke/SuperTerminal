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
