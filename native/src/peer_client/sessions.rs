//! One `/sessions` poller per PAIRED PEER -- never one per attached pane.
//!
//! `peer_client::attach` reports [`Freshness`](super::attach::Freshness):
//! are frames still arriving. It deliberately does NOT report activity,
//! because a [`WireSnapshot`](crate::companion::wire::WireSnapshot) carries
//! geometry, rows, cursor and two mode flags and never activity. Activity is
//! reported by a DIFFERENT endpoint, `/sessions`, as a string per session
//! (`companion::server`) -- and that endpoint's answer is identical for
//! every pane attached to the same peer, so polling it per attachment would
//! multiply requests by the number of open panes for data that never
//! differs between them. Hence: one poller per peer, handed to every pane
//! that views that machine.
//!
//! **Neither signal alone is a pane's activity.** The combining rule lives
//! in `pane.rs` (`attached_activity`), and it is: a stale attachment wins.
//! What this module owes that rule is an honest [`SessionReport`] --
//! including the honest refusal, [`SessionReport::Unpolled`], for a poll
//! that has not succeeded.
//!
//! **This is also where protocol 2's missing exit signal comes from.** The
//! wire has no "the shell exited" frame, so an attached pane whose
//! broadcaster's terminal goes away would otherwise paint its last frame
//! forever -- a dead terminal that looks alive. A session leaving a peer's
//! `/sessions` list (or being listed as no longer `alive`) is that signal,
//! and it costs nothing extra because this poll already runs. See
//! [`SessionReport::Ended`] for exactly what it does and does not prove.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde::Deserialize;
use superterminal_core::activity::Activity;

use crate::companion::auth::PeerId;

use super::Endpoint;

/// Wait between polls. Two seconds, matching the magnitude of
/// `attach::RECONNECT_DELAY` and the server's `SSE_HEARTBEAT`: this is ONE
/// small round trip per PEER (not per pane) and it bounds how long an
/// attached pane keeps painting a session the peer no longer has.
///
/// Deliberately tighter than the phone page's 5s `/sessions` refresh
/// (`page.html`), which is pacing a battery-powered device over the
/// network; a Mac polling one paired peer is not in that situation.
#[cfg_attr(not(test), allow(dead_code))]
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// TOTAL round-trip budget for one `/sessions` poll -- same magnitude as
/// `attach::SEND_DEADLINE` and `stream::CONNECT_DEADLINE`, and for the same
/// reason: this is a single one-shot request/response, not a held-open
/// stream. A peer that accepts and then stalls costs one poll, never the
/// poller.
#[cfg_attr(not(test), allow(dead_code))]
pub const POLL_DEADLINE: Duration = Duration::from_secs(5);

/// One session a peer is offering us, as `/sessions` describes it.
///
/// `busy` and `finished` are deliberately dropped from the wire shape:
/// `busy` is the pre-tri-state duplicate of `activity` (the server keeps
/// them in lockstep, see `companion::hub::SessionInfo`), and `finished` is
/// a cue-gate counter meaningful only to the machine that owns the
/// terminal. Neither has a viewer-side consumer, and carrying a field
/// nobody reads invites someone to read it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct PeerSession {
    pub id: String,
    pub label: String,
    /// False once the broadcaster has retired the session (its pane is
    /// being torn down) and before the entry itself disappears.
    pub alive: bool,
    pub activity: Activity,
}

/// The outcome of the most recent poll. A failed poll REPLACES the previous
/// list rather than leaving it standing: a list fetched thirty seconds ago
/// cannot say what a peer has now, and the whole point of the tri-state is
/// that a cached answer is not evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub enum LastPoll {
    /// Spawned; no poll has completed yet.
    Pending,
    /// The most recent poll failed, or answered something that is not a
    /// session list.
    Failed,
    /// The most recent poll succeeded and the peer offered exactly this.
    /// An EMPTY list is a real answer -- "this peer is sharing nothing with
    /// us" -- and is why a parse failure must never be folded into one.
    Listed(Vec<PeerSession>),
}

/// What a current poll says about ONE session -- the input the pane's
/// combining rule takes alongside the attachment's freshness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub enum SessionReport {
    /// No successful poll to answer from: none has completed yet, or the
    /// most recent one failed. NOT `Ended` -- a peer we cannot reach has
    /// not told us anything, and treating silence as an ending would close
    /// a live pane on one dropped request.
    Unpolled,
    /// A current poll listed this session, reporting this activity.
    Listed(Activity),
    /// A current poll SUCCEEDED and did not offer this session, or offered
    /// it as no longer `alive`.
    ///
    /// What it proves: the peer will not serve us this session any more.
    /// What it does NOT prove: which of the three reasons applies -- the
    /// shell exited, the broadcaster closed the pane, or sharing was
    /// revoked (an un-shared session simply stops appearing in the list
    /// this peer is shown, exactly like a closed one). Anything built on
    /// this must say "ended", never "exited".
    Ended,
}

/// The shape `/sessions` actually serves, per item. Every field except `id`
/// is defaulted rather than required, because a missing field must degrade
/// to the SAFE answer instead of failing a whole poll:
///
/// * `alive` defaults to TRUE -- defaulting it false would report every
///   session of a peer that omitted the field as ended, closing live panes.
/// * `activity` defaults to the empty string, which [`activity_of`] reads
///   as `Unknown` -- never `Idle`.
///
/// `id` is not defaulted: an item with no id names no session, and a list
/// we cannot key is a shape this build does not understand.
#[derive(Deserialize)]
struct WireSession {
    id: String,
    #[serde(default)]
    label: String,
    #[serde(default = "alive_by_default")]
    alive: bool,
    #[serde(default)]
    activity: String,
}

fn alive_by_default() -> bool {
    true
}

/// `/sessions`' activity string as a tri-state. Anything this build does
/// not recognise -- including the literal `"unknown"`, a future spelling,
/// and an absent field -- is `Unknown`. There is no input for which `Idle`
/// is a guess: `Idle` is only ever the peer positively saying `"idle"`.
pub fn activity_of(raw: &str) -> Activity {
    match raw {
        "busy" => Activity::Busy,
        "idle" => Activity::Idle,
        _ => Activity::Unknown,
    }
}

/// Parse a `/sessions` body. `None` -- never an empty list -- for anything
/// that is not a well-formed session list.
///
/// That distinction is the whole hazard this function exists to keep
/// straight. An empty list is a peer saying "I am sharing nothing with
/// you", which correctly ends every attached pane; a body we cannot parse
/// says nothing at all. Folding the second into the first would close
/// every pane attached to a peer that answered with, say, an HTML error
/// page.
pub fn parse_sessions(body: &[u8]) -> Option<Vec<PeerSession>> {
    let raw: Vec<WireSession> = serde_json::from_slice(body).ok()?;
    if raw.iter().any(|s| s.id.is_empty()) {
        return None;
    }
    Some(
        raw.into_iter()
            .map(|s| PeerSession {
                id: s.id,
                label: s.label,
                alive: s.alive,
                activity: activity_of(&s.activity),
            })
            .collect(),
    )
}

/// What the last poll says about one session id. Pure, so the rule is
/// testable without a peer to talk to.
pub fn report_of(poll: &LastPoll, session_id: &str) -> SessionReport {
    match poll {
        LastPoll::Pending | LastPoll::Failed => SessionReport::Unpolled,
        LastPoll::Listed(sessions) => match sessions.iter().find(|s| s.id == session_id) {
            Some(session) if session.alive => SessionReport::Listed(session.activity),
            _ => SessionReport::Ended,
        },
    }
}

/// A peer's live session list, refreshed by one background thread.
///
/// Owned by the `Workspace` (which keeps exactly one per peer) and shared
/// by `Arc` with every attached pane that views that machine. Every method
/// here is a mutex read: `super::get` is a BLOCKING round trip and runs
/// only on this type's own thread, never on a caller's.
#[cfg_attr(not(test), allow(dead_code))]
pub struct SessionPoller {
    peer: PeerId,
    endpoint: Endpoint,
    state: Mutex<LastPoll>,
    /// Completed polls, successful or not. Not part of the interface;
    /// exists so a test can wait for the loop to have actually run rather
    /// than inferring it from a result that might never change.
    polls: AtomicU32,
}

#[cfg_attr(not(test), allow(dead_code))]
impl SessionPoller {
    /// Which peer this polls. The pane holds only the poller, so this is
    /// how the workspace asks a pane which peer it still needs polled.
    pub fn peer(&self) -> &PeerId {
        &self.peer
    }

    /// The whole list, for a UI that offers a peer's sessions.
    pub fn last_poll(&self) -> LastPoll {
        self.state.lock().unwrap().clone()
    }

    /// What the last poll says about one session. See [`report_of`].
    pub fn report_for(&self, session_id: &str) -> SessionReport {
        report_of(&self.state.lock().unwrap(), session_id)
    }

    #[cfg(test)]
    fn polls(&self) -> u32 {
        self.polls.load(Ordering::Acquire)
    }
}

/// Spawn the poller for one peer. The thread holds only a [`Weak`], so
/// dropping the last handle ends it -- within one `POLL_DEADLINE` plus one
/// `POLL_INTERVAL`, since both of the calls it blocks in are bounded (this
/// is why `attach`'s socket-shutdown trick is not needed here: nothing in
/// this loop can block indefinitely the way a heartbeat-fed stream read
/// can).
#[cfg_attr(not(test), allow(dead_code))]
pub fn spawn(peer: PeerId, endpoint: Endpoint) -> Arc<SessionPoller> {
    let poller = Arc::new(SessionPoller {
        peer,
        endpoint,
        state: Mutex::new(LastPoll::Pending),
        polls: AtomicU32::new(0),
    });
    let weak = Arc::downgrade(&poller);
    let _ = std::thread::Builder::new()
        .name("peer-sessions".into())
        .spawn(move || run(weak));
    poller
}

/// Poll, publish, sleep, repeat. Unlike `attach::run` there is no terminal
/// state and no attempt cap: a failed `/sessions` poll is never a statement
/// about the peer's sessions (only about our reach), and the poller exists
/// only while a pane is attached to that peer, so "keep trying until nobody
/// is watching" is the correct lifetime. Polls FIRST and sleeps after, so a
/// freshly attached pane does not wait an interval for its first answer.
#[cfg_attr(not(test), allow(dead_code))]
fn run(weak: Weak<SessionPoller>) {
    loop {
        let Some(poller) = weak.upgrade() else {
            return;
        };
        // Cloned so nothing is borrowed from the `Arc` across the blocking
        // call below -- the lifecycle pattern the rest of this module's
        // sibling threads follow.
        let endpoint = poller.endpoint.clone();
        drop(poller);

        let outcome = match super::get(&endpoint, "/sessions", POLL_DEADLINE) {
            Ok(body) => match parse_sessions(&body) {
                Some(sessions) => LastPoll::Listed(sessions),
                None => LastPoll::Failed,
            },
            Err(_) => LastPoll::Failed,
        };

        let Some(poller) = weak.upgrade() else {
            return;
        };
        *poller.state.lock().unwrap() = outcome;
        poller.polls.fetch_add(1, Ordering::AcqRel);
        drop(poller);

        std::thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::companion::hub::tests::RegisterLocalPty;
    use crate::companion::hub::Hub;
    use crate::companion::server::{start, ServerConfig};
    use crate::peers::{Grants, PeerRecord};
    use crate::term_session::TermSession;
    use std::time::Instant;

    fn full_grants() -> Grants {
        Grants {
            view: true,
            type_: true,
            spawn: true,
        }
    }

    fn thumbs() -> Arc<crate::companion::thumbs::Thumbnailer> {
        crate::companion::thumbs::Thumbnailer::new(
            std::env::temp_dir().join(format!("st-thumbcache-sessions-{}", std::process::id())),
        )
    }

    fn previews() -> Arc<crate::companion::previews::PreviewStore> {
        Arc::new(crate::companion::previews::PreviewStore::new(None))
    }

    /// Polls `cond` until true or `timeout` elapses. Same helper (and same
    /// reasoning) as `attach::tests::wait_until`: a passing run is as fast
    /// as the real work allows, a broken one fails instead of hanging.
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

    fn listed(id: &str, alive: bool, activity: Activity) -> PeerSession {
        PeerSession {
            id: id.to_string(),
            label: format!("label for {id}"),
            alive,
            activity,
        }
    }

    // --- parsing ------------------------------------------------------

    #[test]
    fn a_real_sessions_body_parses_into_ids_labels_and_tri_state_activity() {
        // Byte-for-byte the shape `companion::server`'s `/sessions` arm
        // serves, including the two fields this type drops.
        let body = br#"[
            {"id":"t1","label":"work","alive":true,"busy":true,"activity":"busy","finished":3},
            {"id":"t2","label":"notes","alive":true,"busy":false,"activity":"idle","finished":0},
            {"id":"t3","label":"gone","alive":false,"busy":false,"activity":"unknown","finished":1}
        ]"#;
        let parsed = parse_sessions(body).expect("a real body must parse");
        assert_eq!(
            parsed,
            vec![
                PeerSession {
                    id: "t1".into(),
                    label: "work".into(),
                    alive: true,
                    activity: Activity::Busy
                },
                PeerSession {
                    id: "t2".into(),
                    label: "notes".into(),
                    alive: true,
                    activity: Activity::Idle
                },
                PeerSession {
                    id: "t3".into(),
                    label: "gone".into(),
                    alive: false,
                    activity: Activity::Unknown
                },
            ]
        );
    }

    #[test]
    fn a_body_that_is_not_a_session_list_is_refused_rather_than_read_as_no_sessions() {
        // THE hazard. An empty list ends every attached pane on this peer
        // (see `SessionReport::Ended`); a body we cannot parse says
        // nothing at all. An implementation that folded the second into
        // the first would close live panes on an error page.
        for body in [
            &b"<html>404</html>"[..],
            &b"{\"sessions\":[]}"[..],
            &b""[..],
            &b"[{\"label\":\"no id here\"}]"[..],
            &b"[{\"id\":\"\",\"label\":\"empty id\"}]"[..],
            &b"[{\"id\":5}]"[..],
        ] {
            assert_eq!(
                parse_sessions(body),
                None,
                "parsed {:?} as a session list",
                String::from_utf8_lossy(body)
            );
        }
        // ...and the one that really IS "nothing shared" still parses.
        assert_eq!(parse_sessions(b"[]"), Some(Vec::new()));
    }

    #[test]
    fn an_unrecognised_or_absent_activity_is_unknown_and_never_idle() {
        // `Idle` authorises: it releases the caffeinate hold and reads as
        // "at a prompt". It must only ever come from the peer positively
        // saying so.
        for raw in ["unknown", "", "IDLE", "working", "busy ", "null"] {
            assert_eq!(activity_of(raw), Activity::Unknown, "{raw:?}");
        }
        assert_eq!(activity_of("busy"), Activity::Busy);
        assert_eq!(activity_of("idle"), Activity::Idle);

        let parsed = parse_sessions(br#"[{"id":"t1"},{"id":"t2","activity":"weird"}]"#)
            .expect("missing fields must degrade, not fail the poll");
        assert_eq!(parsed[0].activity, Activity::Unknown);
        assert_eq!(parsed[1].activity, Activity::Unknown);
        assert!(
            parsed[0].alive,
            "a missing `alive` must default to true: false would end every live pane"
        );
    }

    // --- the report ---------------------------------------------------

    #[test]
    fn a_listed_session_reports_the_activity_the_peer_gave_it() {
        for activity in [Activity::Busy, Activity::Idle, Activity::Unknown] {
            let poll = LastPoll::Listed(vec![
                listed("other", true, Activity::Busy),
                listed("t1", true, activity),
            ]);
            assert_eq!(report_of(&poll, "t1"), SessionReport::Listed(activity));
        }
    }

    #[test]
    fn a_session_missing_from_a_successful_poll_has_ended() {
        // The exit signal. Protocol 2 carries no "the shell exited" frame,
        // so this is what stops an attached pane painting a dead terminal
        // forever.
        let poll = LastPoll::Listed(vec![listed("t2", true, Activity::Busy)]);
        assert_eq!(report_of(&poll, "t1"), SessionReport::Ended);
        assert_eq!(
            report_of(&LastPoll::Listed(Vec::new()), "t1"),
            SessionReport::Ended,
            "an empty list is a real answer: the peer is sharing nothing"
        );
    }

    #[test]
    fn a_session_listed_as_no_longer_alive_has_also_ended() {
        // `Hub::retire` flips `alive` when the broadcaster's pane starts
        // tearing down; the entry itself only disappears on that
        // workspace's next sweep. Reading only the id would keep painting
        // a terminal that is already going away.
        let poll = LastPoll::Listed(vec![listed("t1", false, Activity::Busy)]);
        assert_eq!(report_of(&poll, "t1"), SessionReport::Ended);
    }

    #[test]
    fn a_failed_poll_reports_nothing_rather_than_an_ending_or_a_stale_answer() {
        // Both halves matter. A peer we cannot reach has not said the
        // session ended (that would close a live pane on one dropped
        // request), and it has not confirmed the last thing it said
        // either.
        for poll in [LastPoll::Pending, LastPoll::Failed] {
            assert_eq!(report_of(&poll, "t1"), SessionReport::Unpolled, "{poll:?}");
            assert_ne!(report_of(&poll, "t1"), SessionReport::Ended);
        }
    }

    // --- the poller itself, against a real companion server -----------

    #[test]
    fn the_poller_reads_a_real_peers_sessions_and_notices_one_ending() {
        let session = TermSession::spawn(80, 24, 8, 16, None).expect("session spawns");
        let hub = Arc::new(Hub::new());
        hub.register("t1", "poller-live", session.input_sender());
        let peer_id = PeerId("peerPoll".into());
        hub.set_visible_to("t1", &peer_id, true);

        const SECRET: &str = "pollpollpollpollpollpollpollpoll";
        let handle = start(
            Arc::clone(&hub),
            crate::themes::default_theme(),
            ServerConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                token: "phonephonephonephonephonephoneph".into(),
                page: "<title>sessions-poll-test</title>",
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

        let poller = spawn(peer_id.clone(), endpoint);
        assert_eq!(poller.peer(), &peer_id);
        assert_eq!(
            poller.report_for("t1"),
            SessionReport::Unpolled,
            "before any poll completes there is nothing to report"
        );

        // The session is really there, with the label and activity the
        // broadcaster set.
        assert!(
            wait_until(
                || matches!(poller.report_for("t1"), SessionReport::Listed(_)),
                Duration::from_secs(5)
            ),
            "the poller never listed a shared session: {:?}",
            poller.last_poll()
        );
        match poller.last_poll() {
            LastPoll::Listed(sessions) => {
                assert_eq!(sessions.len(), 1, "{sessions:?}");
                assert_eq!(sessions[0].id, "t1");
                assert_eq!(sessions[0].label, "poller-live");
            }
            other => panic!("expected a listed poll, got {other:?}"),
        }
        assert_eq!(
            poller.report_for("never-existed"),
            SessionReport::Ended,
            "a session this peer does not offer has ended"
        );

        // Now the broadcaster's pane goes away, exactly as the workspace
        // sweep does it. The NEXT poll must notice.
        hub.unregister("t1");
        assert!(
            wait_until(
                || poller.report_for("t1") == SessionReport::Ended,
                Duration::from_secs(5)
            ),
            "the poller never noticed the session ending: {:?}",
            poller.last_poll()
        );

        handle.stop();
        session
            .shutdown()
            .join_with_deadline(Duration::from_secs(5));
    }

    #[test]
    fn an_unreachable_peer_never_reports_its_sessions_as_ended() {
        // A poller whose peer is simply off must keep answering "I don't
        // know" forever. Reporting `Ended` here would close every attached
        // pane the moment a laptop lid shut.
        let addr = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap()
        };
        let poller = spawn(
            PeerId("peerOff".into()),
            Endpoint {
                addr,
                secret: "offoffoffoffoffoffoffoffoffoffof".into(),
            },
        );
        assert!(
            wait_until(|| poller.polls() >= 2, Duration::from_secs(10)),
            "the poll loop never ran twice"
        );
        assert_eq!(poller.last_poll(), LastPoll::Failed);
        assert_eq!(poller.report_for("t1"), SessionReport::Unpolled);
    }

    #[test]
    fn a_failed_poll_replaces_a_previous_list_rather_than_leaving_it_standing() {
        // A list fetched before the peer went away cannot say what it has
        // now. This is the same rule the pane's stale-attachment check
        // enforces for frames, applied to the other signal.
        let session = TermSession::spawn(80, 24, 8, 16, None).expect("session spawns");
        let hub = Arc::new(Hub::new());
        hub.register("t1", "poller-stale", session.input_sender());
        let peer_id = PeerId("peerStale".into());
        hub.set_visible_to("t1", &peer_id, true);

        const SECRET: &str = "stalestalestalestalestalestalest";
        let handle = start(
            Arc::clone(&hub),
            crate::themes::default_theme(),
            ServerConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                token: "phonephonephonephonephonephoneph".into(),
                page: "<title>sessions-stale-test</title>",
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
        let poller = spawn(
            peer_id,
            Endpoint {
                addr: handle.addr(),
                secret: SECRET.into(),
            },
        );
        assert!(
            wait_until(
                || matches!(poller.report_for("t1"), SessionReport::Listed(_)),
                Duration::from_secs(5)
            ),
            "the poller never listed the session in the first place"
        );

        // The peer goes away entirely -- not "stops sharing", which would
        // be a successful poll with an empty list.
        handle.stop();
        assert!(
            wait_until(
                || poller.last_poll() == LastPoll::Failed,
                Duration::from_secs(10)
            ),
            "a poll against a stopped server must fail: {:?}",
            poller.last_poll()
        );
        assert_eq!(
            poller.report_for("t1"),
            SessionReport::Unpolled,
            "the previous list must not answer for a poll that failed"
        );

        session
            .shutdown()
            .join_with_deadline(Duration::from_secs(5));
    }
}
