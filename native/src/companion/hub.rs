//! The companion hub: shared observer state between panes (publishers), the
//! workspace (metadata owner), and the server (reader).
//!
//! Lock discipline (binding): the `inner` mutex only guards map access and
//! `Arc` swaps. Serialization happens OUTSIDE it (snapshot Arc + revision are
//! cloned out first) and is memoized per revision under the separate `cache`
//! mutex. Input senders are cloned out under the lock and used after release.
//! Nothing here performs socket I/O or process probes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::term_session::RenderableSnapshot;
use crate::themes::Theme;
use superterminal_core::activity::Activity;

use super::wire::serialize_snapshot;

/// Serialized snapshots above this are replaced by an error event (the page
/// shows "grid too large" instead of the stream stalling).
pub const MAX_SERIALIZED: usize = 1024 * 1024;

/// Phone-requested terminal spawns waiting for the main-thread tick; the cap
/// keeps a misbehaving page from carpeting the Mac in tabs.
pub const MAX_PENDING_SPAWNS: usize = 4;

/// Phone-requested renames waiting for the main-thread tick.
pub const MAX_PENDING_RENAMES: usize = 8;

/// Queued closes. Small: one per visible room is already generous, and a
/// flood would just be the same rooms named twice.
pub const MAX_PENDING_CLOSES: usize = 8;

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
}

pub struct CompanionHub<S: Clone> {
    inner: Mutex<HashMap<String, Published<S>>>,
    /// Bumped when the server (re)starts: panes compare against their own
    /// counter and publish even without fresh output, populating idle grids.
    pub generation: AtomicU64,
    /// Terminals the phone asked for; drained by the workspace tick (PTY
    /// spawn is main-thread-only).
    pending_spawns: Mutex<usize>,
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
            pending_spawns: Mutex::new(0),
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

    pub fn register(&self, id: &str, label: &str, sender: S) {
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

    /// Queue one phone-requested terminal spawn; false when the pending cap
    /// is reached (the server answers 429).
    pub fn request_spawn(&self) -> bool {
        let mut pending = self.pending_spawns.lock().unwrap();
        if *pending >= MAX_PENDING_SPAWNS {
            return false;
        }
        *pending += 1;
        true
    }

    /// Drain and return the number of queued spawns (main-thread tick).
    pub fn take_spawns(&self) -> usize {
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
mod tests {
    use super::*;
    use crate::term_session::{CursorStyle, SnapshotCursor};

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
            assert!(hub.request_spawn());
        }
        assert!(!hub.request_spawn(), "cap must refuse the fifth");
        assert_eq!(hub.take_spawns(), MAX_PENDING_SPAWNS);
        assert_eq!(hub.take_spawns(), 0, "drain resets");
        assert!(hub.request_spawn(), "capacity returns after drain");
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
}
