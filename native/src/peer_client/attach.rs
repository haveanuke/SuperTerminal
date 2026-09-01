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
    last_frame_at: Instant,
}

/// One attachment to a single session on a single peer. Owns a background
/// thread (one per attachment) that connects, reconnects on an unexpected
/// drop up to [`MAX_RECONNECTS`], and stops for good on a terminal
/// [`Status`]. The thread holds only a [`Weak`] reference back to this
/// struct -- see [`spawn`] -- so it exits within one blocking call of the
/// last `Arc<Attachment>` being dropped, rather than leaking per pane.
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
}

#[cfg_attr(not(test), allow(dead_code))]
impl Attachment {
    /// The most recent snapshot successfully parsed off the wire, if any
    /// has arrived yet.
    pub fn latest(&self) -> Option<Arc<WireSnapshot>> {
        self.state.lock().unwrap().latest.clone()
    }

    /// Whether a frame has arrived within `stream::IDLE_GAP` of `now`.
    /// Deliberately takes `now` as a parameter rather than reading the
    /// clock itself: freshness must be `Stale` at the same instant
    /// regardless of when the CALLER happens to ask, and a pure function
    /// of an injected clock is what makes that provable without sleeping
    /// in a test.
    pub fn freshness(&self, now: Instant) -> Freshness {
        let last_frame_at = self.state.lock().unwrap().last_frame_at;
        if now.saturating_duration_since(last_frame_at) >= stream::IDLE_GAP {
            Freshness::Stale
        } else {
            Freshness::Fresh
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
    pub fn send(&self, bytes: &[u8]) -> bool {
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

    fn record_frame(&self, snapshot: WireSnapshot) {
        let mut state = self.state.lock().unwrap();
        state.latest = Some(Arc::new(snapshot));
        state.last_frame_at = Instant::now();
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
            last_frame_at: Instant::now(),
        }),
        attempts: AtomicU32::new(0),
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
/// iteration upgrades just long enough to read the fixed endpoint/session
/// or to publish a result, then drops it before blocking again. This is
/// what makes the `Attachment` drop promptly rather than only after
/// whatever blocking call happens to be in flight (up to `IDLE_GAP`) --
/// the lifecycle pattern named in the brief (`blender.rs:30`), sized here
/// to blocking calls that can run far longer than blender's short ones.
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
                reconnects = 0;
                let Some(attachment) = weak.upgrade() else {
                    return;
                };
                attachment.set_status(Status::Live);
                drop(attachment);

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
    fn dropping_the_attachment_ends_its_thread_rather_than_leaking_it() {
        let session = TermSession::spawn(80, 24, 8, 16, None).expect("session spawns");
        let hub = Arc::new(Hub::new());
        hub.register("t1", "attach-drop", session.input_sender());
        let peer_id = PeerId("peerDrop".into());
        hub.set_visible_to("t1", &peer_id, true);
        hub.publish_snapshot("t1", Arc::new(seeded_snapshot("hello")));

        const SECRET: &str = "dropdropdropdropdropdropdropdrop";
        let handle = start(
            Arc::clone(&hub),
            crate::themes::default_theme(),
            ServerConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                token: "phonephonephonephonephonephoneph".into(),
                page: "<title>attach-drop-test</title>",
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
                || attachment.status() == Status::Live,
                Duration::from_secs(5)
            ),
            "never reached Live"
        );
        let weak = Arc::downgrade(&attachment);
        drop(attachment);

        let deadline = Instant::now() + Duration::from_secs(3);
        while weak.upgrade().is_some() {
            assert!(
                Instant::now() < deadline,
                "attachment thread must release its handle and exit after the drop"
            );
            std::thread::sleep(Duration::from_millis(50));
        }

        handle.stop();
        session
            .shutdown()
            .join_with_deadline(Duration::from_secs(5));
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
}
