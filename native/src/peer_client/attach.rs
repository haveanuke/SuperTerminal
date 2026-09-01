//! Attaching to one shared session on a paired peer: a stream connection
//! plus the state a later phase renders, kept alive for as long as an
//! [`Attachment`] handle exists.
//!
//! **What this reports, and why not activity.** An earlier draft of this
//! phase's plan gave `Attachment` an `activity()` method. That is
//! IMPOSSIBLE from the data available here: [`WireSnapshot`] carries
//! geometry, rows, cursor and two mode flags -- never activity. Activity is
//! reported by `/sessions` as a string (`server.rs`), a DIFFERENT endpoint,
//! polled once per PEER by the owner of a pane rather than once per
//! attachment -- polling `/sessions` per attachment would multiply requests
//! by the number of open panes for data that is identical across all of
//! them.
//!
//! So this module reports [`Freshness`] instead: are frames still arriving.
//! **The combining rule the next phase (C2b) inherits: stale attachment
//! wins.** If frames have stopped, the pane must report `Activity::Unknown`
//! regardless of what the last `/sessions` poll said the peer was doing --
//! a cached "busy" from thirty seconds ago is exactly the stale signal
//! `Unknown` exists to represent. Do not let a fresher activity poll
//! override a stale attachment.

use std::net::TcpStream;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use crate::companion::wire::WireSnapshot;

use super::stream;
use super::{Endpoint, PeerError};

/// Wait between reconnect attempts on an unexpected drop. Fixed, not
/// exponential: five attempts over ten seconds rides out both a wifi blip
/// and the companion restart a peer-settings edit forces on the OTHER end,
/// without polling a peer that is simply off forever.
#[cfg_attr(not(test), allow(dead_code))]
pub const RECONNECT_DELAY: Duration = Duration::from_secs(2);
/// How many reconnect attempts follow an unexpected drop before settling on
/// [`Status::Unavailable`] for good. See [`RECONNECT_DELAY`].
#[cfg_attr(not(test), allow(dead_code))]
pub const MAX_RECONNECTS: u32 = 5;
/// TOTAL round-trip budget for one [`Attachment::send`] call. `send` rides
/// `peer_client::post`, a ONE-SHOT request (`/peer-input/<id>`), so a TOTAL
/// deadline is correct here the same way it is for `round_trip` generally
/// -- matches [`stream::CONNECT_DEADLINE`]'s magnitude since both bound a
/// single request/response, not a held-open stream.
const SEND_DEADLINE: Duration = Duration::from_secs(5);

/// Are frames still arriving. Pure function of wall-clock time since the
/// last snapshot actually parsed off the wire -- NOT of whether the
/// underlying socket is currently open, still connecting, or has an error
/// pending. A perfectly healthy connection that has not published a new
/// frame in a while is exactly as stale as one whose socket died, because
/// from a renderer's point of view they look identical: nothing new to
/// show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub enum Freshness {
    Fresh,
    Stale,
}

/// Connection status, each variant with exactly one cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub enum Status {
    /// Spawned; no successful stream yet (including while waiting out a
    /// reconnect delay after a prior successful stream dropped).
    Connecting,
    /// A stream is open and has produced at least one frame within
    /// `stream::IDLE_GAP`.
    Live,
    /// The peer answered 404: the session is not shared with us, or does
    /// not exist -- the server deliberately does not distinguish those, so
    /// neither can we. TERMINAL: never reconnected.
    Refused,
    /// The peer answered 410, or the stream ended cleanly. TERMINAL: never
    /// reconnected. This is also what MID-STREAM REVOCATION looks like
    /// from here -- when sharing is withdrawn, the server's stream handler
    /// returns and the connection closes cleanly, indistinguishable from
    /// any other clean end of stream, so an un-shared session surfaces as
    /// `Gone`, never `Refused`.
    Gone,
    /// Connect failed, or reconnect attempts were exhausted
    /// (`MAX_RECONNECTS`). Terminal in practice (the background thread has
    /// stopped trying) but not a statement about the peer's session --
    /// unlike `Refused`/`Gone`, this is about OUR ability to reach it.
    Unavailable,
}

struct AttachState {
    status: Status,
    latest: Option<Arc<WireSnapshot>>,
    /// `None` until a frame has actually been parsed off the wire --
    /// distinct from "a frame just arrived," which a seeded `Instant::now()`
    /// would be indistinguishable from. See [`Attachment::freshness`].
    last_frame_at: Option<Instant>,
}

/// One attachment to a single session on a single peer. Owns a background
/// thread (one per attachment) that connects, reconnects on an unexpected
/// drop up to [`MAX_RECONNECTS`], and stops for good on a terminal
/// [`Status`]. The thread holds only a [`Weak`] reference back to this
/// struct -- see [`spawn`] -- but a `Weak` alone is not enough to exit
/// promptly: `StreamConn::next_frame`'s read timeout is per-syscall and
/// resets on every heartbeat, so on a quiet peer that never stops sending
/// them the thread can be blocked in a single read for as long as the peer
/// keeps talking -- far longer than any one `weak.upgrade()` check would
/// notice. `Drop` (below) closes that gap by shutting down
/// [`Attachment::live_socket`], a clone of whatever connection is
/// currently open; that is what actually wakes the blocked read, not the
/// `Weak` by itself.
#[cfg_attr(not(test), allow(dead_code))]
pub struct Attachment {
    endpoint: Endpoint,
    session_id: String,
    state: Mutex<AttachState>,
    /// Count of connection attempts (`stream::open` calls) made so far.
    /// Not part of the documented interface; exists so tests can prove a
    /// terminal status really did stop the thread from trying again,
    /// rather than inferring it from status alone.
    attempts: AtomicU32,
    /// A clone of the socket backing whichever `/stream/<id>` connection
    /// the background thread most recently opened, registered right after
    /// `stream::open` succeeds. `shutdown` on a clone affects the SAME
    /// underlying kernel socket the other clone may be blocked reading --
    /// see `Drop`, the only thing that ever reads this field.
    live_socket: Mutex<Option<TcpStream>>,
}

impl Drop for Attachment {
    /// Interrupts a background thread that may be blocked reading the
    /// most recently opened stream connection -- see this struct's doc
    /// comment for why a `Weak` reference alone cannot make that happen on
    /// a heartbeat-only stream. Shutting down the socket forces the
    /// blocked read to return (as a clean EOF), so the thread's very next
    /// `weak.upgrade()` observes this drop and exits instead of sitting on
    /// the read forever.
    fn drop(&mut self) {
        if let Some(sock) = self.live_socket.lock().unwrap().take() {
            let _ = sock.shutdown(std::net::Shutdown::Both);
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
impl Attachment {
    /// The most recent snapshot successfully parsed off the wire, if any
    /// has arrived yet.
    pub fn latest(&self) -> Option<Arc<WireSnapshot>> {
        self.state.lock().unwrap().latest.clone()
    }

    /// Whether a frame has arrived within `stream::IDLE_GAP` of `now`.
    /// `Stale` before any frame has ever arrived, not just once one goes
    /// quiet -- an attachment with `latest() == None` must never read as
    /// fresh. Deliberately takes `now` as a parameter rather than reading
    /// the clock itself: freshness must be `Stale` at the same instant
    /// regardless of when the CALLER happens to ask, and a pure function
    /// of an injected clock is what makes that provable without sleeping
    /// in a test.
    pub fn freshness(&self, now: Instant) -> Freshness {
        match self.state.lock().unwrap().last_frame_at {
            Some(last_frame_at)
                if now.saturating_duration_since(last_frame_at) < stream::IDLE_GAP =>
            {
                Freshness::Fresh
            }
            _ => Freshness::Stale,
        }
    }

    /// Current connection status.
    pub fn status(&self) -> Status {
        self.state.lock().unwrap().status
    }

    /// Sends `bytes` to the peer's `/peer-input/<session_id>` as a single
    /// bounded round trip, using the server's required envelope
    /// (`{"bytes":[...]}` with `Content-Type: application/companion-input`
    /// -- `peer_client::post` sets the latter; this builds the former).
    /// Independent of the background stream's state: `send` succeeding or
    /// failing says nothing about whether the stream is `Live`, and vice
    /// versa. Returns whether the peer accepted it.
    ///
    /// `session_id` rides straight into the request path with no
    /// escaping, so it is validated first (I2, mirroring `stream::open`) --
    /// a `session_id` that fails the check is rejected outright rather
    /// than sanitised, same reasoning as `stream::is_valid_session_id`'s
    /// doc comment.
    pub fn send(&self, bytes: &[u8]) -> bool {
        if !stream::is_valid_session_id(&self.session_id) {
            return false;
        }
        let body = serde_json::json!({ "bytes": bytes }).to_string();
        let path = format!("/peer-input/{}", self.session_id);
        super::post(&self.endpoint, &path, body.as_bytes(), SEND_DEADLINE).is_ok()
    }

    #[cfg(test)]
    fn attempts(&self) -> u32 {
        self.attempts.load(Ordering::Acquire)
    }

    fn set_status(&self, status: Status) {
        self.state.lock().unwrap().status = status;
    }

    /// The only place `Status::Live` and a `Some` `last_frame_at` are ever
    /// set -- see the variant's own doc: `Live` means a frame has actually
    /// arrived, not merely that a connection is open (I1).
    fn record_frame(&self, snapshot: WireSnapshot) {
        let mut state = self.state.lock().unwrap();
        state.latest = Some(Arc::new(snapshot));
        state.last_frame_at = Some(Instant::now());
        state.status = Status::Live;
    }
}

/// Spawns the background thread and returns the handle it reports back
/// through. `endpoint` and `session_id` are fixed for the attachment's
/// whole life -- reattaching to a different session means dropping this
/// handle and calling `spawn` again, not mutating one in place.
///
/// Not called anywhere outside tests yet -- wiring an attachment into a
/// pane is a later phase (C2b). The `cfg_attr(not(test), ...)` markers
/// below follow the module's existing convention (see `Endpoint`).
#[cfg_attr(not(test), allow(dead_code))]
pub fn spawn(endpoint: Endpoint, session_id: impl Into<String>) -> Arc<Attachment> {
    let attachment = Arc::new(Attachment {
        endpoint,
        session_id: session_id.into(),
        state: Mutex::new(AttachState {
            status: Status::Connecting,
            latest: None,
            last_frame_at: None,
        }),
        attempts: AtomicU32::new(0),
        live_socket: Mutex::new(None),
    });
    let weak = Arc::downgrade(&attachment);
    let _ = std::thread::Builder::new()
        .name("peer-attach".into())
        .spawn(move || run(weak));
    attachment
}

/// What a [`PeerError`] means for the background loop: either the failure
/// is retryable (try again after [`RECONNECT_DELAY`]), or it names a
/// terminal [`Status`] the thread must settle on and stop.
#[cfg_attr(not(test), allow(dead_code))]
enum Outcome {
    Retry,
    Terminal(Status),
}

/// Classifies a failure from EITHER `stream::open` (connect time) or
/// `StreamConn::next_frame` (mid-stream) -- both return the same
/// `PeerError`, so one function covers both call sites. Only an explicit
/// 404/410 status or a clean end of stream is terminal; everything else
/// (a dropped connection, a timeout, a malformed frame) is retried, up to
/// `MAX_RECONNECTS`, exactly per the brief: retrying a 404 in a loop is how
/// a revoked share becomes a request flood, but a wifi blip must not be
/// mistaken for one.
#[cfg_attr(not(test), allow(dead_code))]
fn classify(err: &PeerError) -> Outcome {
    match err {
        PeerError::Status(404) => Outcome::Terminal(Status::Refused),
        PeerError::Status(410) => Outcome::Terminal(Status::Gone),
        PeerError::BadResponse(msg) if *msg == stream::STREAM_CLOSED_CLEANLY => {
            Outcome::Terminal(Status::Gone)
        }
        _ => Outcome::Retry,
    }
}

/// The background loop: connect, stream frames into `Attachment::state`
/// until something goes wrong, then either stop for good (a terminal
/// `Outcome`) or wait `RECONNECT_DELAY` and try again, up to
/// `MAX_RECONNECTS` consecutive failures.
///
/// Holds `weak` -- never a strong `Arc` -- across every blocking call
/// (`stream::open`, `StreamConn::next_frame`, `thread::sleep`): each
/// iteration upgrades just long enough to read the fixed endpoint/session,
/// register the live socket, or publish a result, then drops it before
/// blocking again -- the lifecycle pattern named in the brief
/// (`blender.rs:30`). `stream::open` and the reconnect sleep are genuinely
/// bounded (`CONNECT_DEADLINE`, `RECONNECT_DELAY`), so `weak.upgrade()`
/// alone is enough to make the thread notice a drop within one of those.
/// `StreamConn::next_frame` is NOT bounded the same way: its read timeout
/// is per-syscall and a heartbeat resets it, so a quiet peer that only
/// ever sends `:hb` never lets that call return on its own. Registering
/// `Attachment::live_socket` right after `stream::open` succeeds is what
/// closes that gap -- `Attachment`'s `Drop` impl shuts it down, forcing
/// the blocked read to return so this loop's very next `weak.upgrade()`
/// can observe the drop.
#[cfg_attr(not(test), allow(dead_code))]
fn run(weak: Weak<Attachment>) {
    let mut reconnects: u32 = 0;
    loop {
        let Some(attachment) = weak.upgrade() else {
            return;
        };
        let endpoint = attachment.endpoint.clone();
        let session_id = attachment.session_id.clone();
        attachment.attempts.fetch_add(1, Ordering::AcqRel);
        drop(attachment);

        let outcome = match stream::open(&endpoint, &session_id, stream::CONNECT_DEADLINE) {
            Ok(mut conn) => {
                // Deliberately NOT `reconnects = 0` here (C3): a peer that
                // accepts and answers with valid SSE headers has not
                // proven anything yet -- only a real parsed frame below
                // does. Resetting on mere connection success would let a
                // peer that always connects but never sends a valid frame
                // defeat `MAX_RECONNECTS` by construction. Also
                // deliberately NOT `Status::Live` here (I1): that variant
                // means a frame has arrived, not merely that a socket is
                // open -- see `record_frame`, the only place it is set.
                let Some(attachment) = weak.upgrade() else {
                    return;
                };
                // INVARIANT: a stream this loop proceeds to read from must
                // ALWAYS have a registered interrupt handle -- unlike other
                // secondary I/O errors in this module, a failed clone here
                // is not merely tolerated. If `try_clone_socket` fails (fd
                // exhaustion is the realistic cause) and we pressed on
                // anyway, `live_socket` would still hold the PREVIOUS,
                // already-dead connection's clone (or nothing at all): a
                // reconnect later, `Drop` shuts down a corpse (or nothing),
                // the new connection's blocked read has no interrupt path,
                // and the thread hangs on a heartbeat-only stream forever
                // -- C1 again. Clearing `live_socket` instead would not fix
                // this: `Drop` would just no-op. So a clone failure is
                // treated as a connection failure and takes the retry path
                // -- and since the failure mode under fd pressure is
                // itself "leak a thread and a socket," refusing to
                // proceed also avoids making that pressure worse.
                let clone_outcome = match conn.try_clone_socket() {
                    Ok(sock) => {
                        *attachment.live_socket.lock().unwrap() = Some(sock);
                        None
                    }
                    Err(e) => Some(classify(&PeerError::Io(e))),
                };
                drop(attachment);

                if let Some(outcome) = clone_outcome {
                    outcome
                } else {
                    loop {
                        match conn.next_frame(stream::IDLE_GAP) {
                            Ok(snapshot) => {
                                reconnects = 0;
                                let Some(attachment) = weak.upgrade() else {
                                    return;
                                };
                                attachment.record_frame(snapshot);
                                drop(attachment);
                            }
                            Err(err) => break classify(&err),
                        }
                    }
                }
            }
            Err(err) => classify(&err),
        };

        match outcome {
            Outcome::Terminal(status) => {
                let Some(attachment) = weak.upgrade() else {
                    return;
                };
                attachment.set_status(status);
                return;
            }
            Outcome::Retry => {}
        }

        reconnects += 1;
        if reconnects > MAX_RECONNECTS {
            let Some(attachment) = weak.upgrade() else {
                return;
            };
            attachment.set_status(Status::Unavailable);
            return;
        }
        let Some(attachment) = weak.upgrade() else {
            return;
        };
        attachment.set_status(Status::Connecting);
        drop(attachment);
        std::thread::sleep(RECONNECT_DELAY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::companion::auth::PeerId;
    use crate::companion::hub::tests::RegisterLocalPty;
    use crate::companion::hub::{CompanionHub, Hub};
    use crate::companion::server::{start, ServerConfig};
    use crate::companion::wire::WireRun;
    use crate::peers::{Grants, PeerRecord};
    use crate::term_session::{
        CellColor, CellStyle, CursorStyle, RenderableSnapshot, SnapshotCell, SnapshotCursor,
        TermSession,
    };
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::sync::Arc;

    fn full_grants() -> Grants {
        Grants {
            view: true,
            type_: true,
            spawn: true,
        }
    }

    fn thumbs() -> Arc<crate::companion::thumbs::Thumbnailer> {
        crate::companion::thumbs::Thumbnailer::new(
            std::env::temp_dir().join(format!("st-thumbcache-attach-{}", std::process::id())),
        )
    }

    fn previews() -> Arc<crate::companion::previews::PreviewStore> {
        Arc::new(crate::companion::previews::PreviewStore::new(None))
    }

    /// A single-row, single-style snapshot whose live screen is `text`'s
    /// characters -- enough to prove a real frame's content survived the
    /// round trip, without a real PTY's actual grid. Same shape as
    /// `stream::tests::seeded_snapshot`.
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

    /// Polls `cond` until it is true or `timeout` elapses, returning
    /// whichever came first -- used instead of a fixed sleep so a passing
    /// run is as fast as the real work allows, while a hung/broken run
    /// still fails instead of blocking the suite.
    fn wait_until(mut cond: impl FnMut() -> bool, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if cond() {
                return true;
            }
            if Instant::now() >= deadline {
                return cond();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn attaching_to_a_shared_session_reaches_live_and_receives_a_snapshot() {
        let session = TermSession::spawn(80, 24, 8, 16, None).expect("session spawns");
        let hub = Arc::new(Hub::new());
        hub.register("t1", "attach-live", session.input_sender());
        let peer_id = PeerId("peerLive".into());
        hub.set_visible_to("t1", &peer_id, true);
        // THE TRAP (see `stream::tests`): a session that is registered but
        // never published emits no heartbeats at all -- publish before
        // attaching, or this test would pass or fail for the wrong reason.
        hub.publish_snapshot("t1", Arc::new(seeded_snapshot("hello")));

        const SECRET: &str = "livelivelivelivelivelivelivelive";
        let handle = start(
            Arc::clone(&hub),
            crate::themes::default_theme(),
            ServerConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                token: "phonephonephonephonephonephoneph".into(),
                page: "<title>attach-live-test</title>",
                previews: previews(),
                thumbs: thumbs(),
                peers: vec![PeerRecord {
                    id: peer_id,
                    host: "peer.local".into(),
                    label: "peer".into(),
                    secret: SECRET.into(),
                    grants: full_grants(),
                }],
            },
        )
        .expect("server starts");
        let endpoint = Endpoint {
            addr: handle.addr(),
            secret: SECRET.into(),
        };

        let attachment = spawn(endpoint, "t1");
        assert!(
            wait_until(
                || attachment.status() == Status::Live && attachment.latest().is_some(),
                Duration::from_secs(5)
            ),
            "status: {:?}, latest present: {}",
            attachment.status(),
            attachment.latest().is_some()
        );
        let snap = attachment.latest().expect("a snapshot arrived");
        assert_eq!(row_text(&snap.rows[0]), "hello", "{snap:?}");

        handle.stop();
        session
            .shutdown()
            .join_with_deadline(Duration::from_secs(5));
    }

    #[test]
    fn freshness_tracks_the_gap_since_the_last_frame_not_socket_health() {
        let session = TermSession::spawn(80, 24, 8, 16, None).expect("session spawns");
        let hub = Arc::new(Hub::new());
        hub.register("t1", "attach-fresh", session.input_sender());
        let peer_id = PeerId("peerFresh".into());
        hub.set_visible_to("t1", &peer_id, true);
        hub.publish_snapshot("t1", Arc::new(seeded_snapshot("hello")));

        const SECRET: &str = "freshfreshfreshfreshfreshfreshfr";
        let handle = start(
            Arc::clone(&hub),
            crate::themes::default_theme(),
            ServerConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                token: "phonephonephonephonephonephoneph".into(),
                page: "<title>attach-fresh-test</title>",
                previews: previews(),
                thumbs: thumbs(),
                peers: vec![PeerRecord {
                    id: peer_id,
                    host: "peer.local".into(),
                    label: "peer".into(),
                    secret: SECRET.into(),
                    grants: full_grants(),
                }],
            },
        )
        .expect("server starts");
        let endpoint = Endpoint {
            addr: handle.addr(),
            secret: SECRET.into(),
        };

        let attachment = spawn(endpoint, "t1");
        assert!(
            wait_until(|| attachment.latest().is_some(), Duration::from_secs(5)),
            "first snapshot never arrived"
        );
        let after_first = Instant::now();

        // Right after the frame, and shortly later: still within the gap.
        assert_eq!(attachment.freshness(after_first), Freshness::Fresh);
        assert_eq!(
            attachment.freshness(after_first + Duration::from_millis(500)),
            Freshness::Fresh
        );

        // A purely SYNTHETIC clock jump past `stream::IDLE_GAP` -- no
        // sleep, and nothing happens to the real connection between these
        // two calls: the server, hub and session are all left running.
        // This is what proves freshness measures "did a NEW SNAPSHOT
        // arrive," not "is the socket alive" -- an implementation that
        // (wrongly) derived freshness from connection health would show
        // `Fresh` here forever, since nothing ever closes this socket.
        let far_future = after_first + stream::IDLE_GAP + Duration::from_secs(1);
        assert_eq!(attachment.freshness(far_future), Freshness::Stale);
        // Does not self-heal from more time passing alone.
        assert_eq!(
            attachment.freshness(far_future + Duration::from_secs(100)),
            Freshness::Stale
        );

        // A genuine new frame revives it.
        hub.publish_snapshot("t1", Arc::new(seeded_snapshot("world")));
        assert!(
            wait_until(
                || attachment
                    .latest()
                    .is_some_and(|s| row_text(&s.rows[0]) == "world"),
                Duration::from_secs(5)
            ),
            "second snapshot never arrived"
        );
        let after_second = Instant::now();
        assert_eq!(attachment.freshness(after_second), Freshness::Fresh);

        handle.stop();
        session
            .shutdown()
            .join_with_deadline(Duration::from_secs(5));
    }

    #[test]
    fn a_clean_stream_end_reaches_gone_quickly_not_by_waiting_out_the_idle_gap() {
        // The mirror of the freshness test above: THIS is "the socket
        // died" (well, cleanly ended), and it must be visible fast, via a
        // completely different mechanism than the elapsed-time gap check.
        // Driven by revocation (spec D3c) rather than `handle.stop()`
        // because it is explicitly the same code path a real peer-settings
        // edit takes -- and because the task brief calls out this exact
        // scenario: "an un-shared session surfaces as Gone rather than
        // Refused." A test asserting `Refused` here would be wrong.
        let session = TermSession::spawn(80, 24, 8, 16, None).expect("session spawns");
        let hub = Arc::new(Hub::new());
        hub.register("t1", "attach-gone", session.input_sender());
        let peer_id = PeerId("peerGone".into());
        hub.set_visible_to("t1", &peer_id, true);
        hub.publish_snapshot("t1", Arc::new(seeded_snapshot("hello")));

        const SECRET: &str = "gonegonegonegonegonegonegonegone";
        let handle = start(
            Arc::clone(&hub),
            crate::themes::default_theme(),
            ServerConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                token: "phonephonephonephonephonephoneph".into(),
                page: "<title>attach-gone-test</title>",
                previews: previews(),
                thumbs: thumbs(),
                peers: vec![PeerRecord {
                    id: peer_id.clone(),
                    host: "peer.local".into(),
                    label: "peer".into(),
                    secret: SECRET.into(),
                    grants: full_grants(),
                }],
            },
        )
        .expect("server starts");
        let endpoint = Endpoint {
            addr: handle.addr(),
            secret: SECRET.into(),
        };

        let attachment = spawn(endpoint, "t1");
        assert!(
            wait_until(
                || attachment.status() == Status::Live && attachment.latest().is_some(),
                Duration::from_secs(5)
            ),
            "never reached Live"
        );

        let started = Instant::now();
        hub.set_visible_to("t1", &peer_id, false);
        assert!(
            wait_until(
                || attachment.status() == Status::Gone,
                Duration::from_secs(3)
            ),
            "status: {:?}",
            attachment.status()
        );
        let elapsed = started.elapsed();
        assert!(
            elapsed < stream::IDLE_GAP,
            "must notice the clean close directly, not by waiting out the idle gap: {elapsed:?}"
        );
        assert_eq!(
            attachment.attempts(),
            1,
            "Gone is terminal: no reconnect attempt"
        );

        handle.stop();
        session
            .shutdown()
            .join_with_deadline(Duration::from_secs(5));
    }

    #[test]
    fn attaching_to_an_unshared_session_reaches_refused_and_does_not_reconnect() {
        let session = TermSession::spawn(80, 24, 8, 16, None).expect("session spawns");
        let hub = Arc::new(Hub::new());
        hub.register("t1", "attach-refused", session.input_sender());
        let peer_id = PeerId("peerRefused".into());
        // Deliberately never shared: `hub.set_visible_to` is never called.

        const SECRET: &str = "refusedrefusedrefusedrefusedrefu";
        let handle = start(
            Arc::clone(&hub),
            crate::themes::default_theme(),
            ServerConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                token: "phonephonephonephonephonephoneph".into(),
                page: "<title>attach-refused-test</title>",
                previews: previews(),
                thumbs: thumbs(),
                peers: vec![PeerRecord {
                    id: peer_id,
                    host: "peer.local".into(),
                    label: "peer".into(),
                    secret: SECRET.into(),
                    grants: full_grants(),
                }],
            },
        )
        .expect("server starts");
        let endpoint = Endpoint {
            addr: handle.addr(),
            secret: SECRET.into(),
        };

        let attachment = spawn(endpoint, "t1");
        assert!(
            wait_until(
                || attachment.status() == Status::Refused,
                Duration::from_secs(5)
            ),
            "status: {:?}",
            attachment.status()
        );
        let attempts_at_refusal = attachment.attempts();
        assert_eq!(attempts_at_refusal, 1);

        // Give a wrongly-reconnecting implementation real room to prove
        // itself: several RECONNECT_DELAYs, not a token instant.
        std::thread::sleep(RECONNECT_DELAY * 3);
        assert_eq!(
            attachment.attempts(),
            attempts_at_refusal,
            "Refused must be terminal: no further connection attempts"
        );

        handle.stop();
        session
            .shutdown()
            .join_with_deadline(Duration::from_secs(5));
    }

    #[test]
    fn send_delivers_bytes_the_hub_actually_receives() {
        // `InputSink for mpsc::Sender<Vec<u8>>` is already implemented
        // crate-wide by `companion::server::tests` (compiled into every
        // test binary alongside this module) -- redeclaring it here would
        // conflict, so this just reuses it.
        type SendTestHub = CompanionHub<mpsc::Sender<Vec<u8>>>;

        let hub = Arc::new(SendTestHub::new());
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        hub.register("t1", "attach-send", tx);
        let peer_id = PeerId("peerSend".into());
        hub.set_visible_to("t1", &peer_id, true);

        const SECRET: &str = "sendsendsendsendsendsendsendsend";
        let handle = start(
            Arc::clone(&hub),
            crate::themes::default_theme(),
            ServerConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                token: "phonephonephonephonephonephoneph".into(),
                page: "<title>attach-send-test</title>",
                previews: previews(),
                thumbs: thumbs(),
                peers: vec![PeerRecord {
                    id: peer_id,
                    host: "peer.local".into(),
                    label: "peer".into(),
                    secret: SECRET.into(),
                    grants: full_grants(),
                }],
            },
        )
        .expect("server starts");
        let endpoint = Endpoint {
            addr: handle.addr(),
            secret: SECRET.into(),
        };

        // `send` is a one-shot HTTP call, independent of the background
        // stream -- deliberately not waiting for `Live` first.
        let attachment = spawn(endpoint, "t1");
        let ok = attachment.send(&[104, 105]);
        assert!(ok, "send should report success");
        let received = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the hub's input sender receives the bytes");
        assert_eq!(received, vec![104, 105]);

        handle.stop();
    }

    #[test]
    fn dropping_the_attachment_during_a_heartbeat_only_stream_still_ends_its_thread() {
        // The exact case C1 broke: a peer that never sends a real frame,
        // only `:hb` heartbeats, forever -- what `serve_stream` does for
        // an idle session (`server.rs:930-935`). `FrameReader::next_frame`
        // silently consumes each one and loops straight back into another
        // blocking read with a FRESH per-syscall timeout, so that call
        // never returns on its own; only `Attachment`'s `Drop` shutting
        // down the socket can unblock it.
        //
        // This deliberately does NOT go through `weak.upgrade().is_some()`
        // the way the test this replaces did (C2): that reflects the
        // `Arc`'s refcount, which already hits zero the instant
        // `drop(attachment)` below runs, regardless of whether the
        // background thread ever notices and returns -- an assertion
        // gated on it is unreachable by construction. `JoinHandle::join`
        // completing is the real, external-to-the-thread proof this needs.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (ready_tx, ready_rx) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n");
            // Heartbeats only, never a frame -- generously long so the
            // mock server cannot run out first; the client is expected to
            // disconnect long before this loop finishes.
            for i in 0..400 {
                if stream.write_all(b":hb\n\n").is_err() {
                    return;
                }
                if i == 0 {
                    let _ = ready_tx.send(());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let endpoint = Endpoint {
            addr,
            secret: "whatever-secret-32-chars-long!!".into(),
        };

        // Spawned by hand (mirroring `spawn`) rather than via `spawn`
        // itself, purely so this test can keep the `JoinHandle` `spawn`
        // deliberately discards -- `run` is private but visible here as a
        // child module of `attach`.
        let attachment = Arc::new(Attachment {
            endpoint: endpoint.clone(),
            session_id: "t1".into(),
            state: Mutex::new(AttachState {
                status: Status::Connecting,
                latest: None,
                last_frame_at: None,
            }),
            attempts: AtomicU32::new(0),
            live_socket: Mutex::new(None),
        });
        let weak = Arc::downgrade(&attachment);
        let handle = std::thread::Builder::new()
            .name("peer-attach-test".into())
            .spawn(move || run(weak))
            .expect("thread spawns");

        ready_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("mock peer accepted and sent its first heartbeat");
        // Let the client consume that heartbeat and loop back into the
        // next blocking read, so the drop below races a thread that is
        // GENUINELY stuck inside `next_frame`, not merely still connecting.
        std::thread::sleep(Duration::from_millis(150));

        drop(attachment);

        let (done_tx, done_rx) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            let _ = handle.join();
            let _ = done_tx.send(());
        });
        assert!(
            done_rx.recv_timeout(Duration::from_secs(3)).is_ok(),
            "attachment thread must exit even on a heartbeat-only stream, not leak past the drop"
        );
    }

    #[test]
    fn reconnect_gives_up_at_max_reconnects_and_settles_on_unavailable() {
        // A closed port: every connect attempt fails fast (refused), so
        // the whole test's wall time is dominated by the reconnect delays
        // themselves rather than any connect timeout.
        let addr = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap()
        };
        let endpoint = Endpoint {
            addr,
            secret: "whatever-secret-32-chars-long!!".into(),
        };

        let started = Instant::now();
        let attachment = spawn(endpoint, "ghost");
        assert!(
            wait_until(
                || attachment.status() == Status::Unavailable,
                Duration::from_secs(20)
            ),
            "status: {:?}",
            attachment.status()
        );
        let elapsed = started.elapsed();

        assert_eq!(
            attachment.attempts(),
            MAX_RECONNECTS + 1,
            "one initial attempt plus MAX_RECONNECTS retries, no more and no fewer"
        );
        assert!(
            elapsed >= RECONNECT_DELAY * MAX_RECONNECTS,
            "must actually have waited out every retry delay, not skipped them: {elapsed:?}"
        );
        assert!(
            elapsed < RECONNECT_DELAY * (MAX_RECONNECTS + 3),
            "must not be exponential backoff or a stuck loop: {elapsed:?}"
        );
    }

    #[test]
    fn reconnects_are_bounded_even_when_every_attempt_connects_and_then_misbehaves() {
        // C3: unlike the closed-port test above (where `stream::open`
        // always fails, so the buggy `reconnects = 0` right after a
        // successful `open` never even runs), this peer ACCEPTS every
        // connection and answers with a genuinely valid 200 SSE header --
        // `stream::open` succeeds every single time. What it never does is
        // send a frame that actually parses, so this is the one case that
        // can tell the fix apart from the bug: with the reset left on
        // "connected" instead of moved to "a frame arrived," every
        // successful `open` would zero the counter and this would loop
        // forever, never reaching `Unavailable` -- a full connect-and-
        // authenticate cycle against the peer every `RECONNECT_DELAY`.
        // `wait_until`'s bound is what makes a regression here FAIL rather
        // than hang the suite.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for _ in 0..(MAX_RECONNECTS as usize + 4) {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let _ =
                    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n");
                // Valid SSE framing, invalid payload -- "sends garbage
                // JSON" from the brief. `serde_json::from_slice` fails,
                // which `classify` routes to `Outcome::Retry`, not
                // `Terminal`.
                let _ = stream.write_all(b"data: not valid json\n\n");
                // Dropped here: the connection closes, forcing the client
                // back into a reconnect instead of sitting on a live
                // stream that just never produces anything parseable.
            }
        });
        let endpoint = Endpoint {
            addr,
            secret: "whatever-secret-32-chars-long!!".into(),
        };

        let attachment = spawn(endpoint, "t1");
        assert!(
            wait_until(
                || attachment.status() == Status::Unavailable,
                Duration::from_secs(20)
            ),
            "status: {:?} -- MAX_RECONNECTS must still bound the loop even though \
             every attempt successfully connects",
            attachment.status()
        );
        assert_eq!(
            attachment.attempts(),
            MAX_RECONNECTS + 1,
            "one initial attempt plus MAX_RECONNECTS retries, no more and no fewer, \
             even though every one of them connected"
        );
    }

    #[test]
    fn send_rejects_a_session_id_that_could_split_the_request_line() {
        // Constructed directly rather than via `spawn` so this exercises
        // exactly `send`'s own validation (I2), independent of the
        // background thread's separate `stream::open` call --
        // `stream::is_valid_session_id`'s doc covers why this must be a
        // strict charset rather than an escaping scheme.
        let attachment = Attachment {
            endpoint: Endpoint {
                addr: "127.0.0.1:1".parse().unwrap(),
                secret: "whatever-secret-32-chars-long!!".into(),
            },
            session_id: "term-1\r\nX-Injected: yes".into(),
            state: Mutex::new(AttachState {
                status: Status::Connecting,
                latest: None,
                last_frame_at: None,
            }),
            attempts: AtomicU32::new(0),
            live_socket: Mutex::new(None),
        };
        assert!(
            !attachment.send(&[1, 2, 3]),
            "send must refuse a session id that could split the request line, \
             not attempt the request"
        );
    }
}
