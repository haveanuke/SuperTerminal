//! End-to-end proof: a REAL shell session, published through the real hub,
//! served over the real server — phone-side input round-trips to visible
//! grid content over SSE. The test thread plays the pane pump's role.

#![cfg(test)]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::hub::tests::RegisterLocalPty;
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
            previews: Arc::new(crate::companion::previews::PreviewStore::new(None)),
            thumbs: crate::companion::thumbs::Thumbnailer::new(
                std::env::temp_dir().join(format!("st-thumbcache-e2e-{}", std::process::id())),
            ),
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
fn the_peer_byte_sink_rejects_the_phone_token() {
    // Route admission, not just authentication. The phone authenticates
    // fine; it must still be refused this route, because it has its own
    // symbolic endpoint. This is what proves Task 1's table is ENFORCED
    // and not merely defined.
    let session = TermSession::spawn(80, 24, 8, 16, None).expect("session spawns");
    let hub = Arc::new(Hub::new());
    hub.register("t1", "e2e", session.input_sender());
    let handle = start(
        Arc::clone(&hub),
        crate::themes::default_theme(),
        ServerConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            token: TOKEN.into(),
            page: "<title>e2e</title>",
            previews: Arc::new(crate::companion::previews::PreviewStore::new(None)),
            thumbs: crate::companion::thumbs::Thumbnailer::new(
                std::env::temp_dir().join(format!("st-thumbcache-e2e-{}", std::process::id())),
            ),
        },
    )
    .expect("server starts");
    let host = handle
        .url
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string();

    let body = serde_json::json!({ "bytes": [104, 105] }).to_string();
    let mut stream = TcpStream::connect(&host).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .write_all(
            format!(
                "POST /peer-input/t1 HTTP/1.1\r\nHost: {host}\r\nX-Companion-Token: {TOKEN}\r\nContent-Type: {INPUT_CONTENT_TYPE}\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .unwrap();
    let mut out = String::new();
    let _ = std::io::Read::read_to_string(&mut stream, &mut out);
    assert!(
        out.starts_with("HTTP/1.1 404"),
        "phone token reached the peer sink: {out}"
    );

    handle.stop();
    session
        .shutdown()
        .join_with_deadline(Duration::from_secs(5));
}

#[test]
fn the_peer_byte_sink_rejects_a_payload_over_max_body() {
    // A peer is authenticated but still untrusted input: an oversized
    // payload must be refused by the generic body cap before anything
    // tries to interpret it as a byte array.
    let session = TermSession::spawn(80, 24, 8, 16, None).expect("session spawns");
    let hub = Arc::new(Hub::new());
    hub.register("t1", "e2e", session.input_sender());
    let handle = start(
        Arc::clone(&hub),
        crate::themes::default_theme(),
        ServerConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            token: TOKEN.into(),
            page: "<title>e2e</title>",
            previews: Arc::new(crate::companion::previews::PreviewStore::new(None)),
            thumbs: crate::companion::thumbs::Thumbnailer::new(
                std::env::temp_dir().join(format!("st-thumbcache-e2e-{}", std::process::id())),
            ),
        },
    )
    .expect("server starts");
    let host = handle
        .url
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string();

    let body = serde_json::json!({ "bytes": vec![7u8; 3000] }).to_string();
    assert!(
        body.len() > super::http::MAX_BODY,
        "test body must exceed MAX_BODY to exercise the cap"
    );
    let mut stream = TcpStream::connect(&host).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .write_all(
            format!(
                "POST /peer-input/t1 HTTP/1.1\r\nHost: {host}\r\nX-Companion-Token: {TOKEN}\r\nContent-Type: {INPUT_CONTENT_TYPE}\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .unwrap();
    let mut out = String::new();
    let _ = std::io::Read::read_to_string(&mut stream, &mut out);
    assert!(out.starts_with("HTTP/1.1 413"), "{out}");

    handle.stop();
    session
        .shutdown()
        .join_with_deadline(Duration::from_secs(5));
}

#[test]
fn version_advertises_a_protocol_and_capabilities() {
    let session = TermSession::spawn(80, 24, 8, 16, None).expect("session spawns");
    let hub = Arc::new(Hub::new());
    hub.register("t1", "e2e", session.input_sender());
    let handle = start(
        Arc::clone(&hub),
        crate::themes::default_theme(),
        ServerConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            token: TOKEN.into(),
            page: "<title>e2e</title>",
            previews: Arc::new(crate::companion::previews::PreviewStore::new(None)),
            thumbs: crate::companion::thumbs::Thumbnailer::new(
                std::env::temp_dir().join(format!("st-thumbcache-e2e-{}", std::process::id())),
            ),
        },
    )
    .expect("server starts");
    let host = handle
        .url
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string();

    let mut stream = TcpStream::connect(&host).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .write_all(format!("GET /version?t={TOKEN} HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes())
        .unwrap();
    let mut body = String::new();
    let _ = std::io::Read::read_to_string(&mut stream, &mut body);
    assert!(body.contains("\"protocol\":1"), "{body}");
    assert!(body.contains("\"capabilities\""), "{body}");
    assert!(body.contains("peer-input"), "{body}");

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
fn the_page_reads_activity_by_value_not_truthiness() {
    // `s.busy ? ...` treats the string "idle" as busy. The new page must
    // compare against a value, so a page that DOES get the new field can
    // never mispaint.
    let page = include_str!("page.html");
    assert!(page.contains("s.activity"), "page ignores the new field");
    assert!(
        !page.contains(r#"(s.activity ? "#),
        "page must not test s.activity for truthiness"
    );
    assert!(
        page.contains(r#"act === "busy""#),
        "page must compare activity by value"
    );
    assert!(
        page.contains(r#"act === "unknown""#),
        "page must render the unknown state"
    );
}

#[test]
fn page_treats_the_live_viewport_tile_specially() {
    let page = include_str!("page.html");
    assert!(
        page.contains("kind !== \"viewport\""),
        "viewport tiles skip the thumb variant"
    );
    assert!(page.contains("LIVE"), "live tile is labeled");
    assert!(
        page.contains("pfullEntry"),
        "an open full view follows new live revisions"
    );
}

#[test]
fn page_flags_rooms_that_finished() {
    let page = include_str!("page.html");
    assert!(
        page.contains("function markAttention"),
        "finish transitions tracked"
    );
    assert!(
        page.contains("s.finished"),
        "attention diffs the server's finish counter, not sampled busy"
    );
    assert!(page.contains("attnpulse"), "attention dots pulse");
    assert!(
        page.contains("id=\"attnbanner\""),
        "in-terminal banner announces other rooms finishing"
    );
}

#[test]
fn page_has_a_jump_to_live_button() {
    let page = include_str!("page.html");
    assert!(page.contains("id=\"jumplive\""), "chevron exists");
    assert!(
        page.contains("function updateJumpLive"),
        "visibility follows scroll position"
    );
}

#[test]
fn page_offers_terminal_rename() {
    let page = include_str!("page.html");
    assert!(
        page.contains("function promptRename"),
        "rename prompt exists"
    );
    assert!(page.contains("/rename/"), "rename posts to the route");
    // Rename is reached through the row's action menu now, not a dedicated
    // pencil — see page_collapses_row_actions_into_one_menu.
    assert!(
        page.contains(r#"$("rowrename").addEventListener"#),
        "rename is reachable from the row menu"
    );
    // The terminal view's own title stays directly tappable.
    assert!(
        page.contains("if (current) promptRename(current);"),
        "the open terminal's title still renames"
    );
}

#[test]
fn page_has_a_previews_screen() {
    let page = include_str!("page.html");
    assert!(page.contains("id=\"previews\""), "previews screen exists");
    assert!(page.contains("id=\"previewsbtn\""), "list header opens it");
    assert!(page.contains("id=\"pgrid\""), "thumbnail grid exists");
    assert!(page.contains("id=\"pfull\""), "full-size view exists");
    assert!(
        page.contains("function refreshPreviews"),
        "5s polling loop exists"
    );
    assert!(
        page.contains("unavailable"),
        "distinct unavailable notice is rendered"
    );
    assert!(
        page.contains("thumb=1"),
        "tiles load the downscaled variant"
    );
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

#[test]
fn page_shows_the_running_build() {
    // The phone needs its own version readout: "did my build land?" must be
    // answerable without walking to the Mac.
    let page = include_str!("page.html");
    assert!(page.contains(r#"id="buildtag""#), "build tag element");
    assert!(page.contains("/version"), "page fetches the version route");
}

#[test]
fn page_collapses_row_actions_into_one_menu() {
    // Rename and close both cost two taps: open the menu, choose. Neither
    // destructive nor renaming actions sit under a single stray tap.
    let page = include_str!("page.html");
    assert!(page.contains(r#"id="rowmenu""#), "row action sheet");
    assert!(
        page.contains(r#"id="rowrename""#),
        "rename lives in the menu"
    );
    assert!(page.contains(r#"id="rowclose""#), "close lives in the menu");
    assert!(
        page.contains("openRowMenu"),
        "the row button opens the menu"
    );
    assert!(
        page.contains(r#"setAttribute("aria-label", "actions")"#),
        "the row button is the menu, not a bare rename"
    );
    // The busy warning is in the menu itself, not a second confirm step.
    assert!(page.contains(r#"id="rowmenubusy""#), "busy warning");
    assert!(
        page.contains(r#"classList.toggle("hidden", !session.busy)"#),
        "the warning must be driven by the room's reported busy state"
    );
    // Closing posts with the same content type every mutating route uses.
    assert!(page.contains(r#"fetch("/close/""#), "close route");
    assert!(page.contains("application/companion-input"));
}

#[test]
fn page_keeps_the_open_row_menu_in_sync() {
    // A room that starts working WHILE the menu sits open must update its
    // warning, or the two-tap close becomes blind after all.
    let page = include_str!("page.html");
    assert!(page.contains("function syncRowMenu"), "menu reconciliation");
    assert!(
        page.contains("syncRowMenu(sessions)"),
        "reconciliation must run on every /sessions refresh"
    );
    assert!(
        page.contains("if (!live) { closeRowMenu(); return; }"),
        "a room that disappeared must dismiss its menu"
    );
}

#[test]
fn page_only_treats_202_as_a_queued_close() {
    // Treating every response as success would dismiss the terminal view on
    // a 429 or 403 while nothing was actually closed.
    let page = include_str!("page.html");
    assert!(
        page.contains("r.status === 202 || r.status === 404 || r.status === 410"),
        "only accepted/gone responses may dismiss the view"
    );
    assert!(page.contains(r#"id="rowmenuerr""#), "failures are surfaced");
    assert!(
        page.contains("Too many pending closes"),
        "429 gets its own explanation rather than a bare code"
    );
}

#[test]
fn a_late_close_response_cannot_hijack_another_rooms_menu() {
    // Closes are answered asynchronously. If room A's response lands after
    // the sheet was cancelled or reopened on room B, it must not dismiss
    // B's menu nor show A's error inside it.
    let page = include_str!("page.html");
    assert!(page.contains("function ownsMenu"), "ownership guard");
    assert!(
        page.contains("return !!menuSession && menuSession.id === session.id;"),
        "ownership is decided by id, not by the menu merely being open"
    );
    assert!(
        page.contains("if (ownsMenu(session)) closeRowMenu();"),
        "success may only dismiss the menu it belongs to"
    );
    assert!(
        page.contains("if (!ownsMenu(session)) return;"),
        "errors may only paint into the menu they belong to"
    );
}

#[test]
fn a_late_input_response_cannot_close_a_different_room() {
    // Same class as the menu race, on the input path: a 410 for room A must
    // not dismiss room B just because you switched while it was in flight
    // (the attention banner switches rooms in one tap).
    let page = include_str!("page.html");
    assert!(
        page.contains(
            "if ((r.status === 410 || r.status === 404) && current && current.id === session.id)"
        ),
        "input responses must be scoped to the room that sent them"
    );
}

#[test]
fn the_full_view_does_not_blame_size_for_every_failure() {
    // onerror fires for a stale revision, a dropped connection or a decode
    // failure too. Reporting all of those as "too large" sent people to the
    // Mac for problems that a refresh fixes.
    let page = include_str!("page.html");
    assert!(
        page.contains("function diagnoseFull"),
        "failures are diagnosed"
    );
    assert!(
        page.contains("r.status === 413"),
        "413 is the only size verdict"
    );
    assert!(
        page.contains("file changed - go back and reopen"),
        "a stale revision says so"
    );
    assert!(
        page.contains("could not reach the Mac"),
        "a network failure before headers says so"
    );
    // Cancelling the diagnostic body to avoid a second full transfer means
    // a drop AFTER headers is indistinguishable from an undecodable image,
    // so the 200 copy must not claim to know which it was.
    assert!(
        page.contains("could not load this image - try again"),
        "the 200 case stays honest about not knowing the cause"
    );
    assert!(
        !page.contains("could not display this image"),
        "that wording claimed knowledge the status-only design cannot have"
    );
    // A late diagnosis must not paint over whatever is open now.
    assert!(
        page.contains("if (pfullEntry !== entry) return;"),
        "diagnosis is scoped to the entry that failed"
    );
}

#[test]
fn oversized_files_are_known_before_the_attempt() {
    // The catalog carries `bytes`, so an oversized file never costs a
    // transfer and the message can name the actual size.
    let page = include_str!("page.html");
    assert!(
        page.contains("if (entry.bytes > MAX_FULL_BYTES)"),
        "size is checked up front"
    );
    assert!(page.contains("megabytes(entry.bytes)"), "the size is shown");
    // Pinned to the server's cap; drifting apart would resurrect the guess.
    let expected = crate::companion::previews::MAX_FULL_BYTES;
    assert_eq!(expected, 64 * 1024 * 1024);
    assert!(
        page.contains("var MAX_FULL_BYTES = 64 * 1024 * 1024;"),
        "page cap must equal previews::MAX_FULL_BYTES ({expected})"
    );
}

#[test]
fn the_full_image_src_and_its_error_handler_move_together() {
    // Setting src without rebinding onerror leaves the handler blaming the
    // PREVIOUS entry; the identity guard then swallows the result and the
    // user sees no message at all. The live viewport advancing frames is
    // the path that hits this.
    let page = include_str!("page.html");
    assert!(page.contains("function loadFull"), "one helper owns both");
    // Exactly one place assigns the full image's src, and it is that helper.
    assert_eq!(
        page.matches("img.src = imgSrc(entry, false);").count(),
        1,
        "src must be assigned in loadFull and nowhere else"
    );
    assert!(
        !page.contains(r#"$("pfullimg").src = imgSrc"#),
        "the live-frame path must go through loadFull, not assign src directly"
    );
    assert!(
        page.contains("pfullEntry = entry;\n        loadFull(entry);"),
        "a new live revision reloads through the helper"
    );
}

#[test]
fn closing_the_full_view_cannot_start_a_diagnostic_request() {
    // An empty src can itself fire onerror; with the handler still attached
    // that would fetch on the way out, and the identity guard only stops
    // the painting, not the request.
    let page = include_str!("page.html");
    assert!(page.contains("function clearFullImage"), "shared teardown");
    assert!(
        page.contains("img.onerror = null;\n    img.removeAttribute(\"src\");"),
        "handler is dropped BEFORE the src, and src is removed not emptied"
    );
    assert!(
        !page.contains(r#"$("pfullimg").src = "";"#),
        "an empty src assignment is exactly what fires the stray error"
    );
}

#[test]
fn a_failed_image_is_not_transferred_twice() {
    // A decode failure on a 60 MiB file must not pull the body down again;
    // only the status is wanted.
    let page = include_str!("page.html");
    assert!(
        page.contains("if (r.body && r.body.cancel)"),
        "the diagnostic body is cancelled once the status is known"
    );
    // cancel() returns a promise; a bare try/catch leaves its rejection
    // unhandled.
    assert!(
        page.contains("cancelled.catch(function () {})"),
        "the cancellation promise's rejection is consumed"
    );
}

#[test]
fn page_groups_previews_by_project() {
    let page = include_str!("page.html");
    assert!(
        page.contains(r#"id="pchips""#),
        "the project chip row exists"
    );
    assert!(
        page.contains("categoryOf"),
        "entries are bucketed by their category"
    );
    assert!(
        page.contains("ALL_PROJECTS"),
        "an explicit all-projects filter, not a magic empty string"
    );
}

#[test]
fn a_flat_gallery_shows_no_chip_row() {
    // Tomas's gallery is one flat folder today; categorizing must not put a
    // pointless one-button filter on his screen until he makes a folder.
    let page = include_str!("page.html");
    assert!(
        page.contains(r#"$("pchips").classList.toggle("hidden", names.length < 2)"#),
        "the chip row hides itself until there are at least two projects"
    );
}

#[test]
fn the_live_tile_is_pinned_above_every_project_filter() {
    // The Blender viewport belongs to no project; filtering it away would
    // silently break the live preview Tomas actually watches.
    let page = include_str!("page.html");
    assert!(
        page.contains(r#"entry.kind === "viewport" || project === ALL_PROJECTS"#),
        "the viewport bypasses the project filter"
    );
}

#[test]
fn the_selected_project_survives_a_reload() {
    let page = include_str!("page.html");
    assert!(
        page.contains(r#"loadPref("st-project")"#),
        "selection is read back"
    );
    assert!(
        page.contains(r#"storePref("st-project""#),
        "selection is stored"
    );
}

#[test]
fn a_vanished_project_falls_back_to_all() {
    // Renaming or deleting a folder on the Mac must not leave the phone
    // filtered to a project that no longer exists, showing nothing forever.
    let page = include_str!("page.html");
    assert!(
        page.contains("names.indexOf(project) < 0"),
        "an unknown stored project resets the filter"
    );
}

#[test]
fn the_pad_can_clear_a_typed_but_unsent_message() {
    let page = include_str!("page.html");
    assert!(
        page.contains(r#"data-key="ctrl-u""#),
        "a kill-line key exists on the pad"
    );
    assert!(
        page.contains(">Clear<"),
        "and it is labelled for what it does, not for its byte"
    );
}

#[test]
fn switching_terminals_never_shows_the_previous_ones_output() {
    // render() only resets the grid when its SHAPE changes, and two
    // terminals of the same width share a shape — so on a switch the old
    // session's rows stay painted until the new stream's first frame
    // lands, under the new session's name.
    let page = include_str!("page.html");
    let body = page
        .split("function open(session) {")
        .nth(1)
        .and_then(|rest| rest.split("\n  }").next())
        .expect("open() body");
    assert!(body.contains("resetGrid()"), "open() blanks the grid");
    assert!(
        body.contains(r#"gridShape = """#),
        "and forgets the shape, so the next frame rebuilds rather than patching"
    );
    assert!(
        body.find("resetGrid()") < body.find("new EventSource"),
        "the grid must be blank BEFORE the new stream connects, not after"
    );
}

#[test]
fn a_stale_reconnect_cannot_paint_over_a_newer_stream() {
    // Reconnecting on a room-id match alone is not enough: back out and
    // reopen the SAME room inside the retry window and the stale timer
    // opens a THIRD stream, orphaning the second one with live handlers
    // still able to render. That orphan paints its room's output under
    // whatever room you moved to next — the very bug being fixed.
    let page = include_str!("page.html");
    let body = page
        .split("function open(session) {")
        .nth(1)
        .and_then(|rest| rest.split("\n  }").next())
        .expect("open() body");
    assert!(
        body.contains("cancelReconnect()"),
        "opening a room cancels any pending retry"
    );
    assert!(
        body.find("source.close()") < body.find("new EventSource"),
        "and closes the previous stream before creating another"
    );
    // Every handler must prove it still owns the shared source.
    let guards = page.matches("source !== mine").count();
    assert!(
        guards >= 4,
        "onopen, onerror, its async continuation and onmessage must each \
         confirm ownership before touching shared state; found {guards}"
    );
    assert!(
        page.contains("function close()")
            && page
                .split("function close() {")
                .nth(1)
                .is_some_and(|rest| rest.starts_with("\n    cancelReconnect();")),
        "leaving a room cancels the retry too"
    );
}

#[test]
fn a_stream_refused_by_the_slot_cap_reconnects_itself() {
    // MAX_SSE is 4 and a slot lingers up to one heartbeat after you switch
    // away, so switching between terminals quickly can be refused with 503.
    // A non-200 response is FATAL to an EventSource — the browser never
    // retries it — so without this the view stays dead until you back out.
    let page = include_str!("page.html");
    assert!(
        page.contains("mine.readyState !== 2"),
        "a fatally-closed stream is told apart from a transient blip the \
         browser will retry on its own"
    );
    assert!(
        page.contains("RECONNECT_MS"),
        "the retry delay is named, not a bare magic number"
    );
    assert!(
        page.contains("current.id === session.id"),
        "and it only reconnects the room still on screen"
    );
}
