//! A connection to a companion server's `/stream/<id>` route -- bounded by
//! LIVENESS, not by a single total round-trip deadline.
//!
//! `peer_client::round_trip`'s TOTAL deadline is right for the one-shot
//! routes (`/sessions`, `/peer-input/<id>`) but WRONG here: the server
//! holds `/stream/<id>` open indefinitely by design, sending snapshots plus
//! a `:hb\n\n` heartbeat every `SSE_HEARTBEAT` (2s, `server.rs`). A total
//! deadline applied to the whole connection would kill a perfectly healthy
//! stream the moment it outlived that deadline, which is precisely what a
//! long-lived stream is supposed to do. This module intentionally does NOT
//! call `round_trip` for that reason -- it also requires an explicit
//! `Content-Length` and sends `Connection: close`, neither of which fits a
//! response the server intends to keep writing to forever.
//!
//! Three bounds, three different jobs:
//! - [`CONNECT_DEADLINE`]: TOTAL, covers connect + response headers only.
//! - [`sse::MAX_LINE`] / [`sse::MAX_FRAME`]: memory safety, unchanged from
//!   `sse`.
//! - [`IDLE_GAP`]: ROLLING, covers the life of the established stream.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use crate::companion::wire::WireSnapshot;

use super::sse::{FrameError, FrameReader};
use super::{find_header_end, is_timeout, Endpoint, PeerError, MAX_HEAD, READ_CHUNK};

/// TOTAL budget for connect + reading the response headers, re-applied as
/// a shrinking remainder before every read exactly like
/// `peer_client::round_trip` -- see [`open`]. A peer that accepts the TCP
/// connection and then sends nothing must fail here, not on the stream:
/// this is the hang the one-shot client's deadline exists to catch, and it
/// belongs at connect time, not smeared across the whole stream's life.
#[cfg_attr(not(test), allow(dead_code))]
pub const CONNECT_DEADLINE: Duration = Duration::from_secs(5);

/// ROLLING budget for the established stream: healthy as long as
/// SOMETHING -- a frame or a heartbeat -- arrives within the gap. Six
/// seconds is three `SSE_HEARTBEAT` (2s) intervals: tight enough to notice
/// a dead peer quickly, loose enough that one dropped heartbeat on a slow
/// link does not flap the connection. Exceeding it is
/// `PeerError::Timeout`, never a silent stall.
#[cfg_attr(not(test), allow(dead_code))]
pub const IDLE_GAP: Duration = Duration::from_secs(6);

/// An open `/stream/<id>` connection, past the response headers and ready
/// to read snapshot frames via [`StreamConn::next_frame`].
#[derive(Debug)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct StreamConn {
    frames: FrameReader<TcpStream>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl StreamConn {
    /// Blocks until one complete snapshot arrives, silently consuming any
    /// number of heartbeats along the way (`sse::FrameReader` already skips
    /// them). `idle_gap` is applied as the socket's read timeout for this
    /// call: because a per-syscall socket timeout restarts on every
    /// individual `read()` regardless of whether that read returns data,
    /// this alone makes the gap ROLLING with no extra bookkeeping -- a
    /// byte arriving of any kind (a full frame, a heartbeat, even a
    /// partial line) resets it for free, unlike `round_trip`'s shrinking
    /// TOTAL remainder.
    ///
    /// Exceeding the gap with nothing arriving is `PeerError::Timeout`. A
    /// peer that actively closes the connection is `PeerError::BadResponse`
    /// instead -- deliberately a different variant, so a caller (and a
    /// test) can tell "nothing arrived in time" apart from "the peer told
    /// us it is done" rather than treating both as the same generic error.
    pub fn next_frame(&mut self, idle_gap: Duration) -> Result<WireSnapshot, PeerError> {
        self.frames
            .get_mut()
            .set_read_timeout(Some(idle_gap))
            .map_err(PeerError::Io)?;
        match self.frames.next_frame() {
            Ok(Some(payload)) => serde_json::from_slice(&payload)
                .map_err(|_| PeerError::BadResponse("frame was not a valid snapshot")),
            Ok(None) => Err(PeerError::BadResponse(
                "stream closed before a frame arrived",
            )),
            Err(FrameError::TooLarge) => Err(PeerError::TooLarge),
            Err(FrameError::Io(e)) if is_timeout(&e) => Err(PeerError::Timeout),
            Err(FrameError::Io(e)) => Err(PeerError::Io(e)),
        }
    }
}

/// Opens `/stream/<session_id>` on `endpoint`. `connect_deadline` bounds
/// connect + response headers ONLY, as a TOTAL budget -- see
/// [`CONNECT_DEADLINE`]. Once this returns `Ok`, the stream's liveness is
/// governed by [`StreamConn::next_frame`]'s `idle_gap` instead; this
/// function does not touch that bound at all.
#[cfg_attr(not(test), allow(dead_code))]
pub fn open(
    endpoint: &Endpoint,
    session_id: &str,
    connect_deadline: Duration,
) -> Result<StreamConn, PeerError> {
    let deadline_at = Instant::now() + connect_deadline;
    let remaining = |now: Instant| -> Result<Duration, PeerError> {
        let left = deadline_at.saturating_duration_since(now);
        if left.is_zero() {
            Err(PeerError::Timeout)
        } else {
            Ok(left)
        }
    };

    let connect_budget = remaining(Instant::now())?;
    let mut stream =
        TcpStream::connect_timeout(&endpoint.addr, connect_budget).map_err(PeerError::Connect)?;

    let write_budget = remaining(Instant::now())?;
    stream
        .set_write_timeout(Some(write_budget))
        .map_err(PeerError::Io)?;

    let head = format!(
        "GET /stream/{session_id} HTTP/1.1\r\nHost: {}\r\nX-Companion-Token: {}\r\n\r\n",
        endpoint.addr, endpoint.secret
    );
    stream.write_all(head.as_bytes()).map_err(PeerError::Io)?;

    // Headers only -- deliberately not `round_trip`'s loop, which keeps
    // going to read a `Content-Length` body a stream response never has.
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; READ_CHUNK];
    let header_end = loop {
        if let Some(end) = find_header_end(&buf) {
            break end;
        }
        if buf.len() > MAX_HEAD {
            return Err(PeerError::BadResponse("response headers too large"));
        }
        let left = remaining(Instant::now())?;
        stream
            .set_read_timeout(Some(left.max(Duration::from_millis(10))))
            .map_err(PeerError::Io)?;
        match stream.read(&mut chunk) {
            Ok(0) => return Err(PeerError::BadResponse("closed before headers completed")),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if is_timeout(&e) => return Err(PeerError::Timeout),
            Err(e) => return Err(PeerError::Io(e)),
        }
    };

    let head_str = std::str::from_utf8(&buf[..header_end - 4])
        .map_err(|_| PeerError::BadResponse("headers not utf-8"))?;
    let status_line = head_str
        .split("\r\n")
        .next()
        .ok_or(PeerError::BadResponse("empty response"))?;
    let status: u16 = status_line
        .splitn(3, ' ')
        .nth(1)
        .ok_or(PeerError::BadResponse("no status code"))?
        .parse()
        .map_err(|_| PeerError::BadResponse("status code not numeric"))?;
    if !(200..300).contains(&status) {
        return Err(PeerError::Status(status));
    }

    // Bytes past the header block, if any arrived in the same read, are
    // the start of the SSE body -- seed the frame reader with them rather
    // than dropping them on the floor.
    let mut frames = FrameReader::new(stream);
    frames.feed(&buf[header_end..]);
    Ok(StreamConn { frames })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::companion::hub::tests::RegisterLocalPty;
    use crate::companion::hub::Hub;
    use crate::companion::server::{start, ServerConfig};
    use crate::companion::wire::WireRun;
    use crate::term_session::{
        CellColor, CellStyle, CursorStyle, RenderableSnapshot, SnapshotCell, SnapshotCursor,
        TermSession,
    };
    use std::net::TcpListener;
    use std::sync::Arc;

    /// A single-row, single-style snapshot with `text`'s characters as its
    /// live screen -- enough to prove a real frame's content survived the
    /// round trip, without dragging in a real PTY's actual grid.
    fn seeded_snapshot(text: &str) -> RenderableSnapshot {
        let cols = text.chars().count().max(1) as usize;
        let cell = |ch: char| SnapshotCell {
            ch,
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
        RenderableSnapshot {
            cols,
            lines: 1,
            rows: vec![text.chars().map(cell).collect()],
            cursor: SnapshotCursor {
                col: 0,
                row: Some(0),
                style: CursorStyle::Block,
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

    fn row_text(row: &[WireRun]) -> String {
        row.iter().map(|r| r.text.as_str()).collect()
    }

    const MINIMAL_FRAME: &[u8] = b"data: {\"cols\":1,\"lines\":1,\"cursor\":null,\"appCursor\":false,\"rows\":[],\"bracketedPaste\":false,\"mouseTracking\":false}\n\n";

    fn respond_sse_head(stream: &mut TcpStream) {
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n");
    }

    #[test]
    fn a_peer_that_accepts_and_sends_nothing_fails_at_connect_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            std::thread::sleep(Duration::from_secs(5));
            drop(stream);
        });
        let endpoint = Endpoint {
            addr,
            secret: "whatever".into(),
        };
        let started = Instant::now();
        let result = open(&endpoint, "t1", Duration::from_millis(300));
        assert!(matches!(result, Err(PeerError::Timeout)), "{result:?}");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "an accepted but silent peer must not outlive CONNECT_DEADLINE"
        );
    }

    #[test]
    fn an_open_stream_survives_a_real_heartbeat_and_delivers_a_second_published_snapshot() {
        let session = TermSession::spawn(80, 24, 8, 16, None).expect("session spawns");
        let hub = Arc::new(Hub::new());
        hub.register("t1", "peer-stream-one", session.input_sender());
        // THE TRAP: publish before opening the stream or asserting
        // anything about liveness. A session that is registered but never
        // published starts with `revision: 0` and `snapshot: None`
        // (hub.rs); `serve_stream` (server.rs) then sits in its
        // fresh-but-no-snapshot branch WITHOUT ever emitting a heartbeat,
        // because `fresh` never goes false and the heartbeat `else if` is
        // unreachable. That is silence dressed up as a healthy quiet
        // server -- it would make the liveness assertion below pass or
        // fail for the wrong reason. Publishing first makes this a real
        // "healthy quiet server, heartbeats only" scenario.
        hub.publish_snapshot("t1", Arc::new(seeded_snapshot("hello")));

        const TOKEN: &str = "streamtokenstreamtokenstreamtok1";
        let handle = start(
            Arc::clone(&hub),
            crate::themes::default_theme(),
            ServerConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                token: TOKEN.into(),
                page: "<title>peer-stream-test</title>",
                previews: Arc::new(crate::companion::previews::PreviewStore::new(None)),
                thumbs: crate::companion::thumbs::Thumbnailer::new(
                    std::env::temp_dir()
                        .join(format!("st-thumbcache-peerstream-{}", std::process::id())),
                ),
                peers: Vec::new(),
            },
        )
        .expect("server starts");
        let endpoint = Endpoint {
            addr: handle.addr(),
            secret: TOKEN.into(),
        };

        let mut conn = open(&endpoint, "t1", CONNECT_DEADLINE).expect("stream opens");
        // The real production idle gap: heartbeats alone must carry the
        // connection through it, comfortably above SSE_HEARTBEAT (2s).
        let idle_gap = IDLE_GAP;
        let first = conn
            .next_frame(idle_gap)
            .expect("first published snapshot arrives");
        assert_eq!(row_text(&first.rows[0]), "hello", "{first:?}");

        // Sit quiet past one real heartbeat interval before publishing
        // again -- if `next_frame` were bounded by a TOTAL deadline
        // instead of a rolling one, or didn't treat heartbeats as
        // liveness, this would already have failed by the time the second
        // publish happens.
        let hub2 = Arc::clone(&hub);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(2500));
            hub2.publish_snapshot("t1", Arc::new(seeded_snapshot("world")));
        });
        let started = Instant::now();
        let second = conn
            .next_frame(idle_gap)
            .expect("second snapshot arrives after surviving a real heartbeat");
        assert!(
            started.elapsed() >= Duration::from_millis(2000),
            "should have waited out the real heartbeat for the second publish, not returned instantly: {:?}",
            started.elapsed()
        );
        assert_eq!(row_text(&second.rows[0]), "world", "{second:?}");

        handle.stop();
        session
            .shutdown()
            .join_with_deadline(Duration::from_secs(5));
    }

    #[test]
    fn a_stream_that_delivers_a_frame_then_goes_silent_fails_after_idle_gap_and_not_before() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            respond_sse_head(&mut stream);
            let _ = stream.write_all(MINIMAL_FRAME);
            // Genuinely silent from here -- the socket is kept OPEN (never
            // dropped) for the rest of the test, so a failure here can
            // only be the idle gap firing, never EOF from a closed peer.
            std::thread::sleep(Duration::from_secs(3));
        });
        let endpoint = Endpoint {
            addr,
            secret: "whatever".into(),
        };
        let mut conn = open(&endpoint, "t1", CONNECT_DEADLINE).expect("stream opens");
        let idle_gap = Duration::from_millis(300);
        let first = conn.next_frame(idle_gap);
        assert!(first.is_ok(), "{first:?}");

        let started = Instant::now();
        let second = conn.next_frame(idle_gap);
        let elapsed = started.elapsed();
        assert!(matches!(second, Err(PeerError::Timeout)), "{second:?}");
        assert!(
            elapsed >= idle_gap,
            "must not fire before the gap: {elapsed:?}"
        );
        assert!(
            elapsed < idle_gap + Duration::from_millis(700),
            "must fire close to the gap itself, not some other bound: {elapsed:?}"
        );
    }

    #[test]
    fn a_frame_arriving_resets_the_gap() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            respond_sse_head(&mut stream);
            for _ in 0..6 {
                std::thread::sleep(Duration::from_millis(150));
                if stream.write_all(MINIMAL_FRAME).is_err() {
                    return;
                }
            }
        });
        let endpoint = Endpoint {
            addr,
            secret: "whatever".into(),
        };
        let mut conn = open(&endpoint, "t1", CONNECT_DEADLINE).expect("stream opens");
        // Each individual gap between frames (150ms) stays under
        // idle_gap, but the RUN as a whole (~900ms) far exceeds it -- a
        // TOTAL-deadline implementation would have already failed this by
        // the last iteration; a rolling one must not.
        let idle_gap = Duration::from_millis(300);
        let started = Instant::now();
        for i in 0..6 {
            let result = conn.next_frame(idle_gap);
            assert!(result.is_ok(), "frame {i}: {result:?}");
        }
        assert!(
            started.elapsed() > idle_gap,
            "the run must genuinely have outlasted one idle_gap window for this to prove anything: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn heartbeats_alone_keep_a_stream_alive_for_at_least_two_idle_gaps() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            respond_sse_head(&mut stream);
            // Heartbeats only, comfortably inside idle_gap each, spanning
            // more than two full gaps before the one real frame lands.
            for _ in 0..14 {
                std::thread::sleep(Duration::from_millis(100));
                if stream.write_all(b":hb\n\n").is_err() {
                    return;
                }
            }
            let _ = stream.write_all(MINIMAL_FRAME);
        });
        let endpoint = Endpoint {
            addr,
            secret: "whatever".into(),
        };
        let mut conn = open(&endpoint, "t1", CONNECT_DEADLINE).expect("stream opens");
        let idle_gap = Duration::from_millis(300);
        let started = Instant::now();
        let result = conn.next_frame(idle_gap);
        let elapsed = started.elapsed();
        assert!(result.is_ok(), "{result:?}");
        assert!(
            elapsed >= idle_gap * 2,
            "must have survived at least two idle_gap windows on heartbeats alone: {elapsed:?}"
        );
    }

    #[test]
    fn a_peer_that_closes_the_socket_after_a_frame_is_distinguishable_from_an_idle_gap_timeout() {
        // Same shape as the "goes silent" test above, EXCEPT the peer
        // actively closes the connection instead of leaving it open and
        // quiet. If a test only checked "some error came back" it could
        // not tell this apart from the idle gap firing; this one asserts
        // both the error VARIANT and the elapsed time, which the two
        // scenarios must disagree on.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            respond_sse_head(&mut stream);
            let _ = stream.write_all(MINIMAL_FRAME);
            // Dropped here: the OS sends FIN, so the next read observes a
            // clean close (Ok(0)), not a timeout.
        });
        let endpoint = Endpoint {
            addr,
            secret: "whatever".into(),
        };
        let mut conn = open(&endpoint, "t1", CONNECT_DEADLINE).expect("stream opens");
        let idle_gap = Duration::from_secs(2);
        let first = conn.next_frame(idle_gap);
        assert!(first.is_ok(), "{first:?}");

        let started = Instant::now();
        let second = conn.next_frame(idle_gap);
        let elapsed = started.elapsed();
        assert!(
            !matches!(second, Err(PeerError::Timeout)),
            "a closed socket must not be reported as an idle-gap timeout: {second:?}"
        );
        assert!(
            elapsed < idle_gap / 2,
            "a closed socket should be detected near-instantly, not after waiting out the idle gap: {elapsed:?}"
        );
    }
}
