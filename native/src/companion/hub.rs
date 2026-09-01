//! The companion hub: shared observer state between panes (publishers), the
//! workspace (metadata owner), and the server (reader).
//!
//! Lock discipline (binding): the `inner` mutex only guards map access and
//! `Arc` swaps. Serialization happens OUTSIDE it (snapshot Arc + revision are
//! cloned out first) and is memoized per revision under the separate `cache`
//! mutex. Input senders are cloned out under the lock and used after release.
//! Nothing here performs socket I/O or process probes.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::term_session::RenderableSnapshot;
use crate::themes::Theme;
use superterminal_core::activity::Activity;

use super::auth::{PeerId, Principal};
use super::wire::serialize_snapshot;

/// Where a published session's terminal actually lives. Encoded, never
/// inferred: an ATTACHED pane also has an input sender (it forwards
/// keystrokes to its peer), so "has a sender" cannot distinguish them, and
/// re-publishing an attached pane would create remote views of remote views.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    LocalPty,
    /// Not constructed anywhere yet — a pane that attaches to a peer's
    /// session (rendering its stream, forwarding input to it) is Phase C
    /// work, still unbuilt. This arm exists so publication's rules are
    /// written once against the real enum rather than retrofitted when
    /// attaching lands.
    #[cfg_attr(not(test), allow(dead_code))]
    Attached,
}

/// Serialized snapshots above this are replaced by an error event (the page
/// shows "grid too large" instead of the stream stalling).
pub const MAX_SERIALIZED: usize = 1024 * 1024;

/// Phone- or peer-requested terminal spawns waiting for the main-thread
/// tick; the cap keeps a misbehaving page — or peer — from carpeting the
/// Mac in tabs.
pub const MAX_PENDING_SPAWNS: usize = 4;

/// Phone-requested renames waiting for the main-thread tick.
pub const MAX_PENDING_RENAMES: usize = 8;

/// Queued closes. Small: one per visible room is already generous, and a
/// flood would just be the same rooms named twice.
pub const MAX_PENDING_CLOSES: usize = 8;

/// A queued terminal spawn, remembering who asked. `Principal::Peer`
/// requests are made visible only to that one peer once the workspace tick
/// materializes them — never broadcast to every paired peer.
#[derive(Debug, Clone, PartialEq)]
pub struct SpawnRequest {
    pub principal: Principal,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionInfo {
    pub id: String,
    pub label: String,
    pub alive: bool,
    pub busy: bool,
    /// Tri-state successor to `busy`. Kept in lockstep with it: `busy` is
    /// always `activity == Busy`, so a phone page that predates this field
    /// can never read `Unknown` as working.
    pub activity: Activity,
    /// Monotonic long-job completion counter (cue-gate semantics: a
    /// foreground job busy 5s+ returned to the prompt). The phone diffs it
    /// per poll — a counter cannot be missed the way a sampled busy flag
    /// transition can.
    pub finished: u64,
}

struct Published<S> {
    snapshot: Option<Arc<RenderableSnapshot>>,
    revision: u64,
    info: SessionInfo,
    sender: S,
    /// Where this session's terminal actually lives. See [`Origin`]: this is
    /// the ONLY thing publication may key off — never "has a sender".
    origin: Origin,
    /// Peers this session has been broadcast to. Always empty for
    /// `Origin::Attached` — [`CompanionHub::set_visible_to`] is the only
    /// writer and refuses to populate it for that origin, so an attached
    /// pane can never be re-published no matter who calls it.
    visible_to: HashSet<PeerId>,
}

pub struct CompanionHub<S: Clone> {
    inner: Mutex<HashMap<String, Published<S>>>,
    /// Bumped when the server (re)starts: panes compare against their own
    /// counter and publish even without fresh output, populating idle grids.
    pub generation: AtomicU64,
    /// Terminals asked for (phone or peer); drained by the workspace tick
    /// (PTY spawn is main-thread-only). The cap below is on this queue's
    /// length regardless of who is asking — attribution must never turn one
    /// shared cap into one cap per principal.
    pending_spawns: Mutex<Vec<SpawnRequest>>,
    /// (terminal id, new label) renames from the phone; drained by the
    /// workspace tick (tab state is main-thread-only).
    pending_renames: Mutex<Vec<(String, String)>>,
    pending_closes: Mutex<Vec<String>>,
    cache: Mutex<HashMap<String, (u64, Arc<String>)>>,
}

impl<S: Clone> Default for CompanionHub<S> {
    fn default() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            generation: AtomicU64::new(1),
            pending_spawns: Mutex::new(Vec::new()),
            pending_renames: Mutex::new(Vec::new()),
            pending_closes: Mutex::new(Vec::new()),
            cache: Mutex::new(HashMap::new()),
        }
    }
}

impl<S: Clone> CompanionHub<S> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a session with its origin stated explicitly — see
    /// [`Origin`]. Every production caller passes `Origin::LocalPty` today.
    /// The old two-argument `register` that assumed `Origin::LocalPty` had
    /// no caller left outside tests, so it now lives only in this module's
    /// `#[cfg(test)]` block as `tests::RegisterLocalPty`.
    pub fn register_with_origin(&self, id: &str, label: &str, sender: S, origin: Origin) {
        let mut inner = self.inner.lock().unwrap();
        inner.insert(
            id.to_string(),
            Published {
                snapshot: None,
                revision: 0,
                info: SessionInfo {
                    id: id.to_string(),
                    label: label.to_string(),
                    alive: true,
                    busy: false,
                    activity: Activity::Idle,
                    finished: 0,
                },
                sender,
                origin,
                visible_to: HashSet::new(),
            },
        );
    }

    /// Pane closed: flip alive=false so input answers 410 (Gone), keeping
    /// the entry until the workspace's next metadata sweep removes it.
    pub fn retire(&self, id: &str) {
        if let Some(entry) = self.inner.lock().unwrap().get_mut(id) {
            entry.info.alive = false;
        }
    }

    /// All registered ids (the workspace sweep prunes ones whose pane is
    /// gone).
    pub fn ids(&self) -> Vec<String> {
        self.inner.lock().unwrap().keys().cloned().collect()
    }

    /// Ids eligible for publication to a peer: `Origin::LocalPty` only.
    /// Never derived from "has a sender" — an attached pane has one too.
    ///
    /// NOT the enforcement path: `sessions_for` and `may_touch` are —
    /// they re-check origin directly on each entry rather than trusting a
    /// filter like this one to have already been applied, so a route never
    /// calls this. It exists for tests to assert on hub state directly,
    /// e.g. `only_local_pty_sessions_are_publishable`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn publishable_ids(&self) -> Vec<String> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, entry)| entry.origin == Origin::LocalPty)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Ids currently broadcast to this peer.
    ///
    /// NOT the enforcement path: `sessions_for` and `may_touch` are — see
    /// `publishable_ids`. This exists so tests can assert on
    /// `set_visible_to`'s own mutation-time guard in isolation from the
    /// redundant origin check `may_touch`/`sessions_for` also perform,
    /// which would otherwise mask a broken guard; see
    /// `an_attached_session_cannot_be_broadcast_even_if_asked`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn visible_to(&self, peer: &PeerId) -> Vec<String> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, entry)| entry.visible_to.contains(peer))
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Broadcast (or un-broadcast) a session to one peer. No-op for
    /// `Origin::Attached` sessions: the rule holds here, at the mutation, not
    /// only in [`Self::sessions_for`]/[`Self::may_touch`]'s own origin
    /// checks, so a future caller cannot route around it by writing
    /// `visible_to` directly (those two re-check anyway, deliberately
    /// redundant — see their doc comments).
    pub fn set_visible_to(&self, id: &str, peer: &PeerId, visible: bool) {
        if let Some(entry) = self.inner.lock().unwrap().get_mut(id) {
            if entry.origin != Origin::LocalPty {
                return;
            }
            if visible {
                entry.visible_to.insert(peer.clone());
            } else {
                entry.visible_to.remove(peer);
            }
        }
    }

    pub fn unregister(&self, id: &str) {
        self.inner.lock().unwrap().remove(id);
        self.cache.lock().unwrap().remove(id);
    }

    pub fn publish_snapshot(&self, id: &str, snapshot: Arc<RenderableSnapshot>) {
        if let Some(entry) = self.inner.lock().unwrap().get_mut(id) {
            entry.snapshot = Some(snapshot);
            entry.revision += 1;
        }
    }

    /// A long foreground job in this session just returned to the prompt
    /// (cue-gate transition). No-op for unknown ids.
    pub fn bump_finished(&self, id: &str) {
        if let Some(entry) = self.inner.lock().unwrap().get_mut(id) {
            entry.info.finished += 1;
        }
    }

    /// No-op for unknown ids (metadata refresh racing an unregister).
    /// Delegates to [`Self::set_meta_activity`] so both stay in lockstep.
    /// Kept for backward-compatible callers/tests; the workspace now calls
    /// [`Self::set_meta_activity`] directly.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_meta(&self, id: &str, label: &str, alive: bool, busy: bool) {
        self.set_meta_activity(id, label, alive, Activity::from_local_busy(busy));
    }

    /// Sets both the legacy boolean and the tri-state. `busy` stays
    /// `activity == Busy`, so a page that predates the new field can never
    /// read `Unknown` as working.
    pub fn set_meta_activity(&self, id: &str, label: &str, alive: bool, activity: Activity) {
        if let Some(entry) = self.inner.lock().unwrap().get_mut(id) {
            entry.info.label = label.to_string();
            entry.info.alive = alive;
            entry.info.busy = activity.is_busy();
            entry.info.activity = activity;
        }
    }

    pub fn sessions(&self) -> Vec<SessionInfo> {
        let mut list: Vec<SessionInfo> = self
            .inner
            .lock()
            .unwrap()
            .values()
            .map(|entry| entry.info.clone())
            .collect();
        list.sort_by(|a, b| a.label.cmp(&b.label).then(a.id.cmp(&b.id)));
        list
    }

    /// Sessions a principal may see. `Phone` gets everything, unchanged from
    /// [`Self::sessions`] — this must never regress what the phone sees
    /// today. A `Peer` gets only sessions that are BOTH `Origin::LocalPty`
    /// AND broadcast to it: origin is checked here directly, on the entry,
    /// rather than trusted to have already been enforced by
    /// [`Self::set_visible_to`] — the two guards are deliberately redundant.
    pub fn sessions_for(&self, principal: &Principal) -> Vec<SessionInfo> {
        match principal {
            Principal::Phone => self.sessions(),
            Principal::Peer(peer) => {
                let mut list: Vec<SessionInfo> = self
                    .inner
                    .lock()
                    .unwrap()
                    .values()
                    .filter(|entry| {
                        entry.origin == Origin::LocalPty && entry.visible_to.contains(peer)
                    })
                    .map(|entry| entry.info.clone())
                    .collect();
                list.sort_by(|a, b| a.label.cmp(&b.label).then(a.id.cmp(&b.id)));
                list
            }
        }
    }

    /// Whether a principal may touch (stream from, or write input to) one
    /// session. `Phone` may touch any session that exists, matching its
    /// unrestricted access today. A `Peer` needs the same two conditions as
    /// [`Self::sessions_for`], checked directly on the entry so a future
    /// caller that writes `visible_to` without going through
    /// `set_visible_to` is still refused. An unknown id refuses everyone.
    pub fn may_touch(&self, principal: &Principal, id: &str) -> bool {
        let inner = self.inner.lock().unwrap();
        let Some(entry) = inner.get(id) else {
            return false;
        };
        match principal {
            Principal::Phone => true,
            Principal::Peer(peer) => {
                entry.origin == Origin::LocalPty && entry.visible_to.contains(peer)
            }
        }
    }

    pub fn revision(&self, id: &str) -> Option<u64> {
        self.inner.lock().unwrap().get(id).map(|e| e.revision)
    }

    /// Latest serialized snapshot with its revision; memoized so N phones
    /// never re-serialize the same grid. None until first publish.
    pub fn snapshot_json(&self, id: &str, theme: &Theme) -> Option<(u64, Arc<String>)> {
        let (snapshot, revision) = {
            let inner = self.inner.lock().unwrap();
            let entry = inner.get(id)?;
            (entry.snapshot.clone()?, entry.revision)
        };
        if let Some((cached_rev, json)) = self.cache.lock().unwrap().get(id) {
            if *cached_rev == revision {
                return Some((revision, Arc::clone(json)));
            }
        }
        let wire = serialize_snapshot(&snapshot, theme);
        let json = serde_json::to_string(&wire).unwrap_or_default();
        let json = if json.len() > MAX_SERIALIZED {
            Arc::new(String::from(r#"{"error":"grid too large"}"#))
        } else {
            Arc::new(json)
        };
        self.cache
            .lock()
            .unwrap()
            .insert(id.to_string(), (revision, Arc::clone(&json)));
        Some((revision, json))
    }

    /// (alive, cloned sender) — callers send AFTER the lock is released.
    pub fn input_sender(&self, id: &str) -> Option<(bool, S)> {
        let inner = self.inner.lock().unwrap();
        let entry = inner.get(id)?;
        Some((entry.info.alive, entry.sender.clone()))
    }

    /// Latest snapshot's app-cursor mode (arrow translation).
    pub fn app_cursor(&self, id: &str) -> bool {
        self.inner
            .lock()
            .unwrap()
            .get(id)
            .and_then(|e| e.snapshot.as_ref())
            .map(|s| s.app_cursor_mode)
            .unwrap_or(false)
    }

    /// Latest snapshot's bracketed-paste mode (text framing).
    pub fn bracketed_paste(&self, id: &str) -> bool {
        self.inner
            .lock()
            .unwrap()
            .get(id)
            .and_then(|e| e.snapshot.as_ref())
            .map(|s| s.bracketed_paste)
            .unwrap_or(false)
    }

    /// Queue one terminal spawn on behalf of `principal`; false when the
    /// pending cap is reached. The cap is on the queue's length, not on any
    /// one principal's share of it — a peer does not get a fresh quota just
    /// by being a different principal than the phone.
    pub fn request_spawn_by(&self, principal: Principal) -> bool {
        let mut pending = self.pending_spawns.lock().unwrap();
        if pending.len() >= MAX_PENDING_SPAWNS {
            return false;
        }
        pending.push(SpawnRequest { principal });
        true
    }

    /// Drain and return the queued spawn requests, in request order
    /// (main-thread tick).
    pub fn drain_spawns(&self) -> Vec<SpawnRequest> {
        std::mem::take(&mut *self.pending_spawns.lock().unwrap())
    }

    /// Queue one phone-requested rename; a newer label for the same id
    /// replaces the queued one (latest intent wins). False when the cap is
    /// reached (the server answers 429).
    pub fn request_rename(&self, id: &str, label: &str) -> bool {
        let mut pending = self.pending_renames.lock().unwrap();
        if let Some(slot) = pending.iter_mut().find(|(qid, _)| qid == id) {
            slot.1 = label.to_string();
            return true;
        }
        if pending.len() >= MAX_PENDING_RENAMES {
            return false;
        }
        pending.push((id.to_string(), label.to_string()));
        true
    }

    /// Drain queued renames (main-thread tick).
    pub fn take_renames(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.pending_renames.lock().unwrap())
    }

    /// Queue one phone-requested close. Idempotent per id: asking twice
    /// before the tick drains is the same request, not two. False when the
    /// cap is reached (the server answers 429).
    pub fn request_close(&self, id: &str) -> bool {
        let mut pending = self.pending_closes.lock().unwrap();
        if pending.iter().any(|queued| queued == id) {
            return true;
        }
        if pending.len() >= MAX_PENDING_CLOSES {
            return false;
        }
        pending.push(id.to_string());
        true
    }

    /// Drain queued closes (main-thread tick — killing a PTY is main-thread
    /// work, same as spawning one).
    pub fn take_closes(&self) -> Vec<String> {
        std::mem::take(&mut *self.pending_closes.lock().unwrap())
    }

    pub fn bump_generation(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
    }
}

/// The concrete hub the app uses (panes hand their PTY input senders in).
pub type Hub = CompanionHub<alacritty_terminal::event_loop::EventLoopSender>;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::companion::auth::{PeerId, Principal};
    use crate::term_session::{CursorStyle, SnapshotCursor};

    /// Test-only shorthand for the old two-argument `register`, which had
    /// no production caller left once every real site moved to
    /// `register_with_origin` (see that method's doc comment). An extension
    /// trait, not a free function, so every existing `hub.register(...)`
    /// call site — here and in `server.rs` / `e2e_tests.rs` tests — keeps
    /// its method-call syntax; those files bring it into scope with
    /// `use crate::companion::hub::tests::RegisterLocalPty;`.
    pub(crate) trait RegisterLocalPty<S: Clone> {
        fn register(&self, id: &str, label: &str, sender: S);
    }

    impl<S: Clone> RegisterLocalPty<S> for CompanionHub<S> {
        fn register(&self, id: &str, label: &str, sender: S) {
            self.register_with_origin(id, label, sender, Origin::LocalPty);
        }
    }

    type TestHub = CompanionHub<std::sync::mpsc::Sender<Vec<u8>>>;

    fn hub_with(id: &str, label: &str) -> (TestHub, std::sync::mpsc::Receiver<Vec<u8>>) {
        let hub = TestHub::new();
        let (tx, rx) = std::sync::mpsc::channel();
        hub.register(id, label, tx);
        (hub, rx)
    }

    fn snapshot(text: &str) -> Arc<RenderableSnapshot> {
        use crate::term_session::{CellColor, CellStyle, SnapshotCell};
        let row = text
            .chars()
            .map(|ch| SnapshotCell {
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
            })
            .collect::<Vec<_>>();
        Arc::new(RenderableSnapshot {
            cols: text.chars().count(),
            lines: 1,
            rows: vec![row],
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
        })
    }

    fn theme() -> &'static Theme {
        crate::themes::default_theme()
    }

    #[test]
    fn publish_bumps_revision() {
        let (hub, _rx) = hub_with("t1", "work");
        assert_eq!(hub.revision("t1"), Some(0));
        hub.publish_snapshot("t1", snapshot("a"));
        assert_eq!(hub.revision("t1"), Some(1));
        hub.publish_snapshot("t1", snapshot("b"));
        assert_eq!(hub.revision("t1"), Some(2));
    }

    #[test]
    fn snapshot_json_memoizes_by_revision() {
        let (hub, _rx) = hub_with("t1", "work");
        assert!(hub.snapshot_json("t1", theme()).is_none(), "no publish yet");
        hub.publish_snapshot("t1", snapshot("hello"));
        let (rev_a, json_a) = hub.snapshot_json("t1", theme()).unwrap();
        let (rev_b, json_b) = hub.snapshot_json("t1", theme()).unwrap();
        assert_eq!(rev_a, rev_b);
        assert!(
            Arc::ptr_eq(&json_a, &json_b),
            "same revision reuses the Arc"
        );
        hub.publish_snapshot("t1", snapshot("world"));
        let (rev_c, json_c) = hub.snapshot_json("t1", theme()).unwrap();
        assert_ne!(rev_a, rev_c);
        assert!(json_c.contains("world"));
    }

    #[test]
    fn unregister_removes_session_and_cache() {
        let (hub, _rx) = hub_with("t1", "work");
        hub.publish_snapshot("t1", snapshot("x"));
        let _ = hub.snapshot_json("t1", theme());
        hub.unregister("t1");
        assert!(hub.sessions().is_empty());
        assert!(hub.snapshot_json("t1", theme()).is_none());
        assert!(hub.input_sender("t1").is_none());
    }

    #[test]
    fn set_meta_on_missing_id_is_noop() {
        let hub = TestHub::new();
        hub.set_meta("ghost", "label", true, true);
        assert!(hub.sessions().is_empty());
    }

    #[test]
    fn unknown_activity_never_reads_as_busy_on_the_legacy_flag() {
        // The legacy boolean is what an already-loaded phone page reads.
        // Unknown must present as not-busy there, or an untrusted pane
        // paints orange on every old client.
        let (hub, _rx) = hub_with("t1", "one");
        hub.set_meta_activity("t1", "one", true, Activity::Unknown);
        let info = hub.sessions().into_iter().next().unwrap();
        assert!(!info.busy, "Unknown must not set the legacy busy flag");
        assert_eq!(info.activity, Activity::Unknown);

        hub.set_meta_activity("t1", "one", true, Activity::Busy);
        let info = hub.sessions().into_iter().next().unwrap();
        assert!(info.busy);
        assert_eq!(info.activity, Activity::Busy);
    }

    #[test]
    fn the_legacy_set_meta_still_maps_to_two_states() {
        let (hub, _rx) = hub_with("t1", "one");
        hub.set_meta("t1", "one", true, true);
        assert_eq!(hub.sessions()[0].activity, Activity::Busy);
        hub.set_meta("t1", "one", true, false);
        assert_eq!(hub.sessions()[0].activity, Activity::Idle);
    }

    #[test]
    fn sessions_sorted_by_label_then_id() {
        let hub = TestHub::new();
        let (tx, _rx) = std::sync::mpsc::channel();
        hub.register("t2", "beta", tx.clone());
        hub.register("t3", "alpha", tx.clone());
        hub.register("t1", "beta", tx);
        let order: Vec<(String, String)> = hub
            .sessions()
            .into_iter()
            .map(|s| (s.label, s.id))
            .collect();
        assert_eq!(
            order,
            vec![
                ("alpha".to_string(), "t3".to_string()),
                ("beta".to_string(), "t1".to_string()),
                ("beta".to_string(), "t2".to_string())
            ]
        );
    }

    #[test]
    fn input_sender_reflects_alive_and_delivers_after_release() {
        let (hub, rx) = hub_with("t1", "work");
        let (alive, sender) = hub.input_sender("t1").unwrap();
        assert!(alive);
        sender.send(b"hi".to_vec()).unwrap();
        assert_eq!(rx.recv().unwrap(), b"hi");
        hub.set_meta("t1", "work", false, false);
        let (alive, _) = hub.input_sender("t1").unwrap();
        assert!(!alive);
    }

    #[test]
    fn spawn_queue_caps_and_drains() {
        let hub = TestHub::new();
        for _ in 0..MAX_PENDING_SPAWNS {
            assert!(hub.request_spawn_by(Principal::Phone));
        }
        assert!(
            !hub.request_spawn_by(Principal::Phone),
            "cap must refuse the fifth"
        );
        assert_eq!(hub.drain_spawns().len(), MAX_PENDING_SPAWNS);
        assert_eq!(hub.drain_spawns().len(), 0, "drain resets");
        assert!(
            hub.request_spawn_by(Principal::Phone),
            "capacity returns after drain"
        );
    }

    #[test]
    fn a_drained_spawn_remembers_who_asked() {
        let hub = TestHub::new();
        assert!(hub.request_spawn_by(Principal::Phone));
        assert!(hub.request_spawn_by(Principal::Peer(PeerId("p1".into()))));
        let drained = hub.drain_spawns();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].principal, Principal::Phone);
        assert_eq!(drained[1].principal, Principal::Peer(PeerId("p1".into())));
        assert!(hub.drain_spawns().is_empty(), "drain must be exhaustive");
    }

    #[test]
    fn the_pending_cap_still_holds_across_principals() {
        // The cap exists to stop a misbehaving client carpeting the Mac in
        // tabs; attribution must not turn one cap into one cap per peer.
        let hub = TestHub::new();
        for _ in 0..MAX_PENDING_SPAWNS {
            assert!(hub.request_spawn_by(Principal::Phone));
        }
        assert!(
            !hub.request_spawn_by(Principal::Peer(PeerId("p1".into()))),
            "a peer must not get its own fresh quota"
        );
    }

    #[test]
    fn finish_counter_bumps_and_rides_session_info() {
        let (hub, _rx) = hub_with("t1", "work");
        assert_eq!(hub.sessions()[0].finished, 0);
        hub.bump_finished("t1");
        hub.bump_finished("t1");
        assert_eq!(hub.sessions()[0].finished, 2);
        hub.bump_finished("ghost"); // unknown ids are a quiet no-op
    }

    #[test]
    fn rename_queue_caps_replaces_and_drains() {
        let (hub, _rx) = hub_with("t1", "work");
        assert!(hub.request_rename("t1", "build"));
        // A newer rename for the same id replaces the queued one — the
        // phone's latest intent wins, without eating queue capacity.
        assert!(hub.request_rename("t1", "deploy"));
        for i in 0..(MAX_PENDING_RENAMES - 1) {
            assert!(hub.request_rename(&format!("x{i}"), "n"));
        }
        assert!(
            !hub.request_rename("overflow", "n"),
            "cap must refuse past MAX_PENDING_RENAMES"
        );
        let drained = hub.take_renames();
        assert_eq!(drained.len(), MAX_PENDING_RENAMES);
        assert_eq!(drained[0], ("t1".to_string(), "deploy".to_string()));
        assert!(hub.take_renames().is_empty(), "drain resets");
        assert!(hub.request_rename("t1", "again"), "capacity returns");
    }

    #[test]
    fn app_cursor_follows_latest_snapshot() {
        let (hub, _rx) = hub_with("t1", "work");
        assert!(!hub.app_cursor("t1"));
        let mut snap = (*snapshot("a")).clone();
        snap.app_cursor_mode = true;
        hub.publish_snapshot("t1", Arc::new(snap));
        assert!(hub.app_cursor("t1"));
    }

    #[test]
    fn bracketed_paste_follows_latest_snapshot() {
        let (hub, _rx) = hub_with("t1", "work");
        assert!(!hub.bracketed_paste("t1"));
        let mut snap = (*snapshot("a")).clone();
        snap.bracketed_paste = true;
        hub.publish_snapshot("t1", Arc::new(snap));
        assert!(hub.bracketed_paste("t1"));
    }

    #[test]
    fn close_requests_dedupe_and_cap() {
        let (hub, _rx) = hub_with("t1", "work");
        // Asking twice before the tick drains is one request, not two —
        // a double-tap must not queue two closes.
        assert!(hub.request_close("t1"));
        assert!(hub.request_close("t1"));
        assert_eq!(hub.take_closes(), vec!["t1".to_string()]);
        assert!(hub.take_closes().is_empty(), "draining empties the queue");

        for i in 0..MAX_PENDING_CLOSES {
            assert!(hub.request_close(&format!("id{i}")), "under the cap");
        }
        assert!(!hub.request_close("one-too-many"), "cap answers 429");
        assert_eq!(hub.take_closes().len(), MAX_PENDING_CLOSES);
    }

    #[test]
    fn only_local_pty_sessions_are_publishable() {
        // An attached pane forwards keystrokes, so it HAS an input sender.
        // Publication must key off origin, never off "has a sender".
        let hub = TestHub::new();
        let (tx, _rx) = std::sync::mpsc::channel();
        hub.register_with_origin("local", "one", tx.clone(), Origin::LocalPty);
        hub.register_with_origin("attached", "two", tx, Origin::Attached);
        assert_eq!(hub.publishable_ids(), vec!["local".to_string()]);
    }

    #[test]
    fn a_session_is_visible_to_nobody_until_broadcast() {
        let hub = TestHub::new();
        let (tx, _rx) = std::sync::mpsc::channel();
        hub.register_with_origin("local", "one", tx, Origin::LocalPty);
        assert!(hub.visible_to(&PeerId("p1".into())).is_empty());
    }

    #[test]
    fn broadcasting_to_one_peer_does_not_expose_it_to_another() {
        let hub = TestHub::new();
        let (tx, _rx) = std::sync::mpsc::channel();
        hub.register_with_origin("local", "one", tx, Origin::LocalPty);
        hub.set_visible_to("local", &PeerId("p1".into()), true);
        assert_eq!(
            hub.visible_to(&PeerId("p1".into())),
            vec!["local".to_string()]
        );
        assert!(hub.visible_to(&PeerId("p2".into())).is_empty());
    }

    #[test]
    fn an_attached_session_cannot_be_broadcast_even_if_asked() {
        // Defence in depth: the rule holds at the mutation, not only at the
        // listing, so a future caller cannot route around it.
        let hub = TestHub::new();
        let (tx, _rx) = std::sync::mpsc::channel();
        hub.register_with_origin("attached", "two", tx, Origin::Attached);
        hub.set_visible_to("attached", &PeerId("p1".into()), true);
        assert!(hub.visible_to(&PeerId("p1".into())).is_empty());
    }

    #[test]
    fn the_phone_still_sees_every_session() {
        // Scoping must not change what the phone sees. This is the
        // regression guard for the whole task.
        let hub = TestHub::new();
        let (tx, _rx) = std::sync::mpsc::channel();
        hub.register_with_origin("a", "one", tx.clone(), Origin::LocalPty);
        hub.register_with_origin("b", "two", tx, Origin::LocalPty);
        let seen: Vec<String> = hub
            .sessions_for(&Principal::Phone)
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(seen.len(), 2);
    }

    #[test]
    fn a_peer_sees_only_what_it_was_made_visible() {
        let hub = TestHub::new();
        let (tx, _rx) = std::sync::mpsc::channel();
        hub.register_with_origin("a", "one", tx.clone(), Origin::LocalPty);
        hub.register_with_origin("b", "two", tx, Origin::LocalPty);
        let p1 = PeerId("p1".into());
        hub.set_visible_to("a", &p1, true);
        let seen: Vec<String> = hub
            .sessions_for(&Principal::Peer(p1.clone()))
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(seen, vec!["a".to_string()]);
    }

    #[test]
    fn a_peer_sees_nothing_before_anything_is_broadcast() {
        let hub = TestHub::new();
        let (tx, _rx) = std::sync::mpsc::channel();
        hub.register_with_origin("a", "one", tx, Origin::LocalPty);
        assert!(hub
            .sessions_for(&Principal::Peer(PeerId("p1".into())))
            .is_empty());
    }

    #[test]
    fn a_peer_may_not_touch_a_session_it_cannot_see() {
        let hub = TestHub::new();
        let (tx, _rx) = std::sync::mpsc::channel();
        hub.register_with_origin("a", "one", tx.clone(), Origin::LocalPty);
        hub.register_with_origin("b", "two", tx, Origin::LocalPty);
        let p1 = PeerId("p1".into());
        hub.set_visible_to("a", &p1, true);
        let peer = Principal::Peer(p1);
        assert!(hub.may_touch(&peer, "a"));
        assert!(
            !hub.may_touch(&peer, "b"),
            "peer reached an unshared session"
        );
        assert!(hub.may_touch(&Principal::Phone, "b"), "phone lost access");
    }

    #[test]
    fn set_visible_to_refuses_an_attached_session() {
        // Covers the guard at the MUTATION: set_visible_to must not record
        // visibility for an Attached entry in the first place. This does
        // NOT exercise may_touch's own origin check — see
        // `may_touch_refuses_an_attached_session_even_if_visibility_was_recorded`
        // for that, which bypasses this guard on purpose.
        let hub = TestHub::new();
        let (tx, _rx) = std::sync::mpsc::channel();
        hub.register_with_origin("mirror", "two", tx, Origin::Attached);
        let p1 = PeerId("p1".into());
        hub.set_visible_to("mirror", &p1, true);
        assert!(!hub.may_touch(&Principal::Peer(p1), "mirror"));
    }

    #[test]
    fn may_touch_refuses_an_attached_session_even_if_visibility_was_recorded() {
        // set_visible_to refuses Attached entries, so going through it would
        // leave visible_to empty and this test would pass even with may_touch's
        // origin check deleted. Write the visibility DIRECTLY to simulate a
        // future caller that bypasses that guard — proving may_touch's own
        // check is load-bearing rather than decorative.
        let hub = TestHub::new();
        let (tx, _rx) = std::sync::mpsc::channel();
        hub.register_with_origin("mirror", "two", tx, Origin::Attached);
        let p1 = PeerId("p1".into());
        hub.inner
            .lock()
            .unwrap()
            .get_mut("mirror")
            .expect("registered above")
            .visible_to
            .insert(p1.clone());
        assert!(
            !hub.may_touch(&Principal::Peer(p1), "mirror"),
            "may_touch relied on set_visible_to instead of checking origin itself"
        );
    }

    #[test]
    fn sessions_for_omits_an_attached_session_even_if_visibility_was_recorded() {
        // Same gap, same fix, for the list form: sessions_for's peer arm
        // must check origin itself rather than trust that visible_to was
        // only ever populated by set_visible_to.
        let hub = TestHub::new();
        let (tx, _rx) = std::sync::mpsc::channel();
        hub.register_with_origin("mirror", "two", tx, Origin::Attached);
        let p1 = PeerId("p1".into());
        hub.inner
            .lock()
            .unwrap()
            .get_mut("mirror")
            .expect("registered above")
            .visible_to
            .insert(p1.clone());
        assert!(
            hub.sessions_for(&Principal::Peer(p1)).is_empty(),
            "sessions_for relied on set_visible_to instead of checking origin itself"
        );
    }
}
