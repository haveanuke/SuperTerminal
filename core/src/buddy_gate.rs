//! Buddy dispatch gating, pure and clock-injected.
//!
//! The workspace observes the focused pane's repo every tick (single-flight
//! background probes) and feeds results here; this gate decides WHEN the
//! buddy speaks and with which trigger. Observation never stops — only
//! dispatch is serialized (one utterance in flight at a time).
//!
//! Triggers, in precedence order:
//! 1. `Commit` — HEAD moved in a repo we were already watching. A commit is
//!    finished by definition: no stability wait.
//! 2. `WorkingDiff` — the same non-empty working snapshot (diff + status)
//!    held still for 7s. Mid-edit trees never qualify.
//! 3. `Reaction` — a foreground job that ran 5s+ just ended and no review is
//!    brewing: the buddy may react in character (60s cooldown).
//!
//! Hashes are opaque u64s supplied by the caller: `snapshot_hash` covers the
//! FULL working snapshot (not the prompt excerpt). Patch-content dedupe
//! across triggers (a commit that equals the just-reviewed diff) is checked
//! by the caller via `already_reviewed` once it has the normalized patch.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long a working snapshot must hold still before review.
const STABLE_FOR: Duration = Duration::from_secs(7);
/// Minimum spacing between pure reactions.
const REACTION_COOLDOWN: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// The approved snapshot hash rides along so the dispatcher can verify,
    /// AFTER materializing the patch, that the tree still IS that snapshot
    /// (editing may resume between approval and the git read).
    WorkingDiff(u64),
    /// Commit oid to review (`git show` it).
    Commit(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Utterance {
    /// A review of `root`'s changes (the dispatcher runs the patch commands
    /// there).
    Review(String, Trigger),
    Reaction,
}

/// One probe's findings for the focused pane.
#[derive(Debug, Clone)]
pub struct ProbeResult {
    /// Canonical repo root; None = not in a git repository.
    pub repo_root: Option<String>,
    /// Resolved HEAD object id; None = unborn HEAD (fresh repo).
    pub head: Option<String>,
    /// Whether the movement that produced `head` was a COMMIT CREATION
    /// (commit/amend/cherry-pick per the reflog), as opposed to a checkout,
    /// reset, or rebase — only creations deserve a "just committed" review.
    pub head_committed: bool,
    /// Hash of the full working snapshot; None = tree is clean.
    pub snapshot_hash: Option<u64>,
}

#[derive(Debug, Default)]
pub struct BuddyGate {
    generation: u64,
    probe_inflight: bool,
    /// (repo_root, snapshot_hash, held since).
    pending: Option<(String, u64, Instant)>,
    candidate: Option<Utterance>,
    /// The utterance currently being executed, if any.
    inflight: Option<Utterance>,
    /// Focus moved while `inflight` ran: its FAILURE must not requeue (a
    /// retry would pair the old repo with the new pane's context).
    inflight_stale: bool,
    /// Last observed HEAD per repo root (None = unborn).
    heads: HashMap<String, Option<String>>,
    /// Snapshot hash last dispatched as a WorkingDiff review, per root —
    /// stops a still-stable diff from re-dispatching every probe.
    dispatched: HashMap<String, u64>,
    /// Normalized patch hash last successfully reviewed, per root.
    reviewed: HashMap<String, u64>,
    last_reaction: Option<Instant>,
}

impl BuddyGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask to start a probe; None while one is already in flight.
    pub fn want_probe(&mut self) -> Option<u64> {
        if self.probe_inflight {
            return None;
        }
        self.generation += 1;
        self.probe_inflight = true;
        Some(self.generation)
    }

    /// Report a finished probe. Only the latest in-flight generation is
    /// applied; anything else is dropped.
    pub fn probe_done(&mut self, generation: u64, result: ProbeResult, now: Instant) {
        if generation != self.generation || !self.probe_inflight {
            return;
        }
        self.probe_inflight = false;
        // A repo change under an in-flight review (same-pane `cd`, or a
        // pane leaving its repo) makes that review's failure unretryable —
        // a requeue would pair the old repo with the new context.
        if let Some(Utterance::Review(inflight_root, _)) = &self.inflight {
            if result.repo_root.as_deref() != Some(inflight_root.as_str()) {
                self.inflight_stale = true;
            }
        }
        let Some(root) = result.repo_root else {
            self.pending = None;
            self.drop_candidate_unless_at(None);
            return;
        };
        self.drop_candidate_unless_at(Some(&root));

        // Commit detection: HEAD moved in a repo we were already watching.
        // First sighting only records (never review pre-existing history);
        // unborn (None) → Some(first commit) counts as a move.
        match self.heads.get(&root) {
            None => {
                self.heads.insert(root.clone(), result.head.clone());
            }
            Some(prev) if *prev != result.head => {
                self.heads.insert(root.clone(), result.head.clone());
                // Checkout/reset/rebase move HEAD without creating work —
                // only a real commit creation is reviewed.
                if result.head_committed {
                    if let Some(oid) = result.head.clone() {
                        self.candidate =
                            Some(Utterance::Review(root.clone(), Trigger::Commit(oid)));
                    }
                }
            }
            Some(_) => {}
        }

        match result.snapshot_hash {
            None => {
                self.pending = None;
                // A clean tree takes a WorkingDiff candidate's content with
                // it; a Commit candidate survives (the commit likely IS what
                // emptied the tree).
                if matches!(
                    self.candidate,
                    Some(Utterance::Review(_, Trigger::WorkingDiff(_)))
                ) {
                    self.candidate = None;
                }
            }
            Some(hash) => match &self.pending {
                Some((proot, phash, since)) if *proot == root && *phash == hash => {
                    let commit_waiting = matches!(
                        self.candidate,
                        Some(Utterance::Review(_, Trigger::Commit(_)))
                    );
                    if now.duration_since(*since) >= STABLE_FOR
                        && self.dispatched.get(&root) != Some(&hash)
                        && !commit_waiting
                    {
                        self.candidate =
                            Some(Utterance::Review(root.clone(), Trigger::WorkingDiff(hash)));
                    }
                }
                _ => self.pending = Some((root, hash, now)),
            },
        }
    }

    /// A foreground job that ran 5s+ just returned to the prompt.
    pub fn job_finished(&mut self, now: Instant) {
        if self.inflight.is_some() || self.candidate.is_some() || self.is_composing() {
            return;
        }
        if self
            .last_reaction
            .is_some_and(|at| now.duration_since(at) < REACTION_COOLDOWN)
        {
            return;
        }
        self.candidate = Some(Utterance::Reaction);
    }

    /// Pop the next utterance to run, if any and nothing is in flight.
    pub fn take_dispatch(&mut self, now: Instant) -> Option<Utterance> {
        if self.inflight.is_some() {
            return None;
        }
        let utterance = self.candidate.take()?;
        match &utterance {
            Utterance::Review(root, Trigger::WorkingDiff(hash)) => {
                self.dispatched.insert(root.clone(), *hash);
            }
            Utterance::Reaction => {
                self.last_reaction = Some(now);
            }
            Utterance::Review(_, Trigger::Commit(_)) => {}
        }
        self.inflight = Some(utterance.clone());
        Some(utterance)
    }

    /// The in-flight utterance completed. A review passes the (repo root,
    /// normalized patch hash) it covered, for dedupe; reactions pass None.
    pub fn utterance_succeeded(&mut self, reviewed: Option<(String, u64)>) {
        self.inflight = None;
        self.inflight_stale = false;
        if let Some((root, hash)) = reviewed {
            self.reviewed.insert(root, hash);
        }
    }

    /// The in-flight utterance failed (agent timeout/launch error). Reviews
    /// are requeued so the caller's backoff actually retries them — only a
    /// SUCCESS may consume content. A newer candidate wins over the requeue;
    /// reactions are ephemeral and never retried.
    pub fn utterance_failed(&mut self) {
        let Some(failed) = self.inflight.take() else {
            return;
        };
        let stale = std::mem::take(&mut self.inflight_stale);
        match &failed {
            Utterance::Review(root, Trigger::WorkingDiff(_)) => {
                // Un-consume the snapshot; the retry then flows through the
                // NORMAL stability path — unchanged content re-candidates on
                // the next probe, changed content earns a fresh 7s first.
                // Never requeue directly: the tree may have moved during the
                // failure/backoff and must be revalidated.
                self.dispatched.remove(root);
            }
            Utterance::Review(_, Trigger::Commit(_)) => {
                // A commit won't re-trigger (HEAD already advanced), so it
                // IS requeued — unless focus moved mid-flight or something
                // newer superseded it.
                if !stale && self.candidate.is_none() {
                    self.candidate = Some(failed);
                }
            }
            Utterance::Reaction => {}
        }
    }

    /// The in-flight utterance was cancelled: nothing to show (clean tree,
    /// merge commit) or already reviewed. Content stays consumed — retrying
    /// would loop on the same unreviewable state.
    pub fn utterance_cancelled(&mut self) {
        self.inflight = None;
        self.inflight_stale = false;
    }

    /// The observed pane changed. Location-bound state must die with it —
    /// a candidate or pending snapshot from the old repo would otherwise
    /// pair with the NEW pane's terminal context — and any in-flight probe
    /// is invalidated (its result describes the old pane's repo). Long-lived
    /// per-repo memory (heads, reviewed, dispatched) survives: it is keyed
    /// by root and correct wherever focus lands.
    pub fn focus_changed(&mut self) {
        self.pending = None;
        self.candidate = None;
        if self.inflight.is_some() {
            self.inflight_stale = true;
        }
        if self.probe_inflight {
            // The next want_probe issues a fresh generation; the stale
            // result fails the generation check and is dropped.
            self.probe_inflight = false;
            self.generation += 1;
        }
    }

    /// Caller-side dedupe: has this exact patch content already been
    /// reviewed for this repo?
    pub fn already_reviewed(&self, repo_root: &str, patch_hash: u64) -> bool {
        self.reviewed.get(repo_root) == Some(&patch_hash)
    }

    /// Whether the "…" composing bubble should show: something is brewing —
    /// an un-dispatched pending snapshot, a queued candidate, or an
    /// utterance in flight.
    pub fn is_composing(&self) -> bool {
        if self.inflight.is_some() || self.candidate.is_some() {
            return true;
        }
        match &self.pending {
            Some((root, hash, _)) => self.dispatched.get(root) != Some(hash),
            None => false,
        }
    }

    /// Drop a location-bound candidate when focus is no longer in its repo —
    /// it would pair that repo's changes with another pane's context.
    /// Reactions are location-free and survive.
    fn drop_candidate_unless_at(&mut self, root: Option<&str>) {
        if let Some(Utterance::Review(candidate_root, _)) = &self.candidate {
            if root != Some(candidate_root.as_str()) {
                self.candidate = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    fn probe(root: &str, head: &str, snapshot: Option<u64>) -> ProbeResult {
        ProbeResult {
            repo_root: Some(root.into()),
            head: Some(head.into()),
            head_committed: true,
            snapshot_hash: snapshot,
        }
    }

    /// Run one full probe cycle: want → done.
    fn observe(gate: &mut BuddyGate, result: ProbeResult, now: Instant) {
        let gen = gate.want_probe().expect("probe should start");
        gate.probe_done(gen, result, now);
    }

    #[test]
    fn stable_diff_dispatches_working_review_after_7s() {
        let base = Instant::now();
        let mut gate = BuddyGate::new();
        observe(&mut gate, probe("/r", "h1", Some(42)), t(base, 0));
        assert_eq!(gate.take_dispatch(t(base, 0)), None);
        observe(&mut gate, probe("/r", "h1", Some(42)), t(base, 4));
        assert_eq!(gate.take_dispatch(t(base, 4)), None);
        observe(&mut gate, probe("/r", "h1", Some(42)), t(base, 8));
        assert_eq!(
            gate.take_dispatch(t(base, 8)),
            Some(Utterance::Review("/r".into(), Trigger::WorkingDiff(42)))
        );
    }

    #[test]
    fn changing_diff_resets_stability_clock() {
        let base = Instant::now();
        let mut gate = BuddyGate::new();
        observe(&mut gate, probe("/r", "h1", Some(1)), t(base, 0));
        observe(&mut gate, probe("/r", "h1", Some(2)), t(base, 4));
        observe(&mut gate, probe("/r", "h1", Some(2)), t(base, 8));
        // Only 4s since hash 2 appeared.
        assert_eq!(gate.take_dispatch(t(base, 8)), None);
        observe(&mut gate, probe("/r", "h1", Some(2)), t(base, 12));
        assert_eq!(
            gate.take_dispatch(t(base, 12)),
            Some(Utterance::Review("/r".into(), Trigger::WorkingDiff(2)))
        );
    }

    #[test]
    fn stable_diff_dispatches_once_not_every_probe() {
        let base = Instant::now();
        let mut gate = BuddyGate::new();
        observe(&mut gate, probe("/r", "h1", Some(42)), t(base, 0));
        observe(&mut gate, probe("/r", "h1", Some(42)), t(base, 8));
        assert!(gate.take_dispatch(t(base, 8)).is_some());
        gate.utterance_succeeded(Some(("/r".into(), 7000)));
        // Same still-stable snapshot keeps arriving: no re-dispatch.
        observe(&mut gate, probe("/r", "h1", Some(42)), t(base, 16));
        assert_eq!(gate.take_dispatch(t(base, 16)), None);
        // But NEW content starts a fresh cycle.
        observe(&mut gate, probe("/r", "h1", Some(43)), t(base, 20));
        observe(&mut gate, probe("/r", "h1", Some(43)), t(base, 28));
        assert_eq!(
            gate.take_dispatch(t(base, 28)),
            Some(Utterance::Review("/r".into(), Trigger::WorkingDiff(43)))
        );
    }

    #[test]
    fn clean_tree_clears_pending_and_composing() {
        let base = Instant::now();
        let mut gate = BuddyGate::new();
        observe(&mut gate, probe("/r", "h1", Some(42)), t(base, 0));
        assert!(gate.is_composing());
        observe(&mut gate, probe("/r", "h1", None), t(base, 4));
        assert!(!gate.is_composing());
        observe(&mut gate, probe("/r", "h1", None), t(base, 8));
        assert_eq!(gate.take_dispatch(t(base, 8)), None);
    }

    #[test]
    fn probes_are_single_flight() {
        let mut gate = BuddyGate::new();
        let gen = gate.want_probe().expect("first probe starts");
        assert_eq!(gate.want_probe(), None);
        gate.probe_done(gen, probe("/r", "h1", None), Instant::now());
        assert!(gate.want_probe().is_some());
    }

    #[test]
    fn stale_probe_generation_is_ignored() {
        let base = Instant::now();
        let mut gate = BuddyGate::new();
        let gen = gate.want_probe().unwrap();
        gate.probe_done(gen, probe("/r", "h1", Some(1)), t(base, 0));
        let gen2 = gate.want_probe().unwrap();
        // A result for a long-dead generation arrives out of order.
        gate.probe_done(gen, probe("/r", "h1", Some(9)), t(base, 4));
        gate.probe_done(gen2, probe("/r", "h1", Some(1)), t(base, 4));
        observe(&mut gate, probe("/r", "h1", Some(1)), t(base, 8));
        // Stability held (the stale 9 never landed): review at 8s.
        assert_eq!(
            gate.take_dispatch(t(base, 8)),
            Some(Utterance::Review("/r".into(), Trigger::WorkingDiff(1)))
        );
    }

    #[test]
    fn head_change_on_known_repo_dispatches_commit_immediately() {
        let base = Instant::now();
        let mut gate = BuddyGate::new();
        observe(&mut gate, probe("/r", "h1", None), t(base, 0));
        observe(&mut gate, probe("/r", "h2", None), t(base, 4));
        assert_eq!(
            gate.take_dispatch(t(base, 4)),
            Some(Utterance::Review("/r".into(), Trigger::Commit("h2".into())))
        );
    }

    #[test]
    fn first_sighting_of_repo_records_head_without_review() {
        let base = Instant::now();
        let mut gate = BuddyGate::new();
        observe(&mut gate, probe("/r", "h1", None), t(base, 0));
        assert_eq!(gate.take_dispatch(t(base, 0)), None);
    }

    #[test]
    fn switching_repos_never_fakes_a_commit() {
        let base = Instant::now();
        let mut gate = BuddyGate::new();
        observe(&mut gate, probe("/a", "ha", None), t(base, 0));
        observe(&mut gate, probe("/b", "hb", None), t(base, 4));
        assert_eq!(gate.take_dispatch(t(base, 4)), None);
        // Returning to /a with its head unchanged: still nothing.
        observe(&mut gate, probe("/a", "ha", None), t(base, 8));
        assert_eq!(gate.take_dispatch(t(base, 8)), None);
        // But a real commit in /a while we were away IS caught on return.
        observe(&mut gate, probe("/a", "ha2", None), t(base, 12));
        assert_eq!(
            gate.take_dispatch(t(base, 12)),
            Some(Utterance::Review(
                "/a".into(),
                Trigger::Commit("ha2".into())
            ))
        );
    }

    #[test]
    fn unborn_head_to_first_commit_triggers() {
        let base = Instant::now();
        let mut gate = BuddyGate::new();
        let unborn = ProbeResult {
            repo_root: Some("/r".into()),
            head: None,
            head_committed: false,
            snapshot_hash: Some(1),
        };
        observe(&mut gate, unborn, t(base, 0));
        assert_eq!(gate.take_dispatch(t(base, 0)), None);
        observe(&mut gate, probe("/r", "first", None), t(base, 4));
        assert_eq!(
            gate.take_dispatch(t(base, 4)),
            Some(Utterance::Review(
                "/r".into(),
                Trigger::Commit("first".into())
            ))
        );
    }

    #[test]
    fn commit_during_inflight_review_is_held_then_dispatched() {
        let base = Instant::now();
        let mut gate = BuddyGate::new();
        observe(&mut gate, probe("/r", "h1", Some(42)), t(base, 0));
        observe(&mut gate, probe("/r", "h1", Some(42)), t(base, 8));
        assert!(gate.take_dispatch(t(base, 8)).is_some());
        // Review is in flight; a commit lands and is observed.
        observe(&mut gate, probe("/r", "h2", None), t(base, 12));
        assert_eq!(gate.take_dispatch(t(base, 12)), None);
        gate.utterance_succeeded(Some(("/r".into(), 500)));
        assert_eq!(
            gate.take_dispatch(t(base, 16)),
            Some(Utterance::Review("/r".into(), Trigger::Commit("h2".into())))
        );
    }

    #[test]
    fn commit_replaces_pending_working_diff_candidate() {
        let base = Instant::now();
        let mut gate = BuddyGate::new();
        observe(&mut gate, probe("/r", "h1", Some(42)), t(base, 0));
        observe(&mut gate, probe("/r", "h1", Some(42)), t(base, 8));
        // Candidate exists but not yet taken; the commit supersedes it.
        observe(&mut gate, probe("/r", "h2", None), t(base, 9));
        assert_eq!(
            gate.take_dispatch(t(base, 9)),
            Some(Utterance::Review("/r".into(), Trigger::Commit("h2".into())))
        );
        assert_eq!(gate.take_dispatch(t(base, 10)), None);
    }

    #[test]
    fn reaction_fires_only_when_nothing_is_brewing() {
        let base = Instant::now();
        let mut gate = BuddyGate::new();
        observe(&mut gate, probe("/r", "h1", None), t(base, 0));
        gate.job_finished(t(base, 4));
        assert_eq!(gate.take_dispatch(t(base, 4)), Some(Utterance::Reaction));
    }

    #[test]
    fn reaction_suppressed_while_diff_pending() {
        let base = Instant::now();
        let mut gate = BuddyGate::new();
        observe(&mut gate, probe("/r", "h1", Some(42)), t(base, 0));
        gate.job_finished(t(base, 2));
        assert_eq!(gate.take_dispatch(t(base, 2)), None);
    }

    #[test]
    fn review_candidate_replaces_reaction() {
        let base = Instant::now();
        let mut gate = BuddyGate::new();
        observe(&mut gate, probe("/r", "h1", None), t(base, 0));
        gate.job_finished(t(base, 4));
        // Before the reaction dispatches, a commit lands.
        observe(&mut gate, probe("/r", "h2", None), t(base, 5));
        assert_eq!(
            gate.take_dispatch(t(base, 5)),
            Some(Utterance::Review("/r".into(), Trigger::Commit("h2".into())))
        );
        gate.utterance_succeeded(Some(("/r".into(), 1)));
        // The displaced reaction does not resurface.
        assert_eq!(gate.take_dispatch(t(base, 70)), None);
    }

    #[test]
    fn reactions_respect_cooldown() {
        let base = Instant::now();
        let mut gate = BuddyGate::new();
        observe(&mut gate, probe("/r", "h1", None), t(base, 0));
        gate.job_finished(t(base, 4));
        assert_eq!(gate.take_dispatch(t(base, 4)), Some(Utterance::Reaction));
        gate.utterance_succeeded(None);
        gate.job_finished(t(base, 30));
        assert_eq!(gate.take_dispatch(t(base, 30)), None);
        gate.job_finished(t(base, 70));
        assert_eq!(gate.take_dispatch(t(base, 70)), Some(Utterance::Reaction));
    }

    #[test]
    fn dispatch_is_serialized() {
        let base = Instant::now();
        let mut gate = BuddyGate::new();
        observe(&mut gate, probe("/r", "h1", None), t(base, 0));
        observe(&mut gate, probe("/r", "h2", None), t(base, 4));
        assert!(gate.take_dispatch(t(base, 4)).is_some());
        observe(&mut gate, probe("/r", "h3", None), t(base, 8));
        // Second commit observed while first review runs: held.
        assert_eq!(gate.take_dispatch(t(base, 8)), None);
        gate.utterance_succeeded(None);
        assert_eq!(
            gate.take_dispatch(t(base, 12)),
            Some(Utterance::Review("/r".into(), Trigger::Commit("h3".into())))
        );
    }

    #[test]
    fn already_reviewed_tracks_per_repo_patch_hashes() {
        let mut gate = BuddyGate::new();
        assert!(!gate.already_reviewed("/r", 99));
        // Simulate a dispatched+completed review recording its patch hash.
        let base = Instant::now();
        observe(&mut gate, probe("/r", "h1", None), t(base, 0));
        observe(&mut gate, probe("/r", "h2", None), t(base, 4));
        assert!(gate.take_dispatch(t(base, 4)).is_some());
        gate.utterance_succeeded(Some(("/r".into(), 99)));
        assert!(gate.already_reviewed("/r", 99));
        assert!(!gate.already_reviewed("/r", 100));
        assert!(!gate.already_reviewed("/other", 99));
    }

    #[test]
    fn switching_repo_drops_the_other_repos_candidate() {
        let base = Instant::now();
        let mut gate = BuddyGate::new();
        observe(&mut gate, probe("/a", "h1", Some(42)), t(base, 0));
        observe(&mut gate, probe("/a", "h1", Some(42)), t(base, 8));
        // Focus moved to a different repo before dispatch: the /a candidate
        // would pair /a's diff with /b's terminal context — drop it.
        observe(&mut gate, probe("/b", "hb", None), t(base, 9));
        assert_eq!(gate.take_dispatch(t(base, 9)), None);
    }

    #[test]
    fn composing_lifecycle() {
        let base = Instant::now();
        let mut gate = BuddyGate::new();
        assert!(!gate.is_composing());
        observe(&mut gate, probe("/r", "h1", Some(42)), t(base, 0));
        assert!(gate.is_composing());
        observe(&mut gate, probe("/r", "h1", Some(42)), t(base, 8));
        assert!(gate.take_dispatch(t(base, 8)).is_some());
        assert!(gate.is_composing());
        gate.utterance_succeeded(Some(("/r".into(), 1)));
        // Snapshot unchanged and already dispatched: nothing brewing.
        observe(&mut gate, probe("/r", "h1", Some(42)), t(base, 16));
        assert!(!gate.is_composing());
    }

    #[test]
    fn failed_working_review_retries_once_still_stable() {
        let base = Instant::now();
        let mut gate = BuddyGate::new();
        observe(&mut gate, probe("/r", "h1", Some(42)), t(base, 0));
        observe(&mut gate, probe("/r", "h1", Some(42)), t(base, 8));
        assert!(gate.take_dispatch(t(base, 8)).is_some());
        gate.utterance_failed();
        // Failure un-consumes the snapshot; the next probe of the SAME
        // stable content re-candidates it (the caller's backoff spaces the
        // retry). No probe, no retry — content is always revalidated.
        observe(&mut gate, probe("/r", "h1", Some(42)), t(base, 16));
        assert_eq!(
            gate.take_dispatch(t(base, 16)),
            Some(Utterance::Review("/r".into(), Trigger::WorkingDiff(42)))
        );
    }

    #[test]
    fn content_changed_during_backoff_needs_fresh_stability() {
        let base = Instant::now();
        let mut gate = BuddyGate::new();
        observe(&mut gate, probe("/r", "h1", Some(42)), t(base, 0));
        observe(&mut gate, probe("/r", "h1", Some(42)), t(base, 8));
        assert!(gate.take_dispatch(t(base, 8)).is_some());
        gate.utterance_failed();
        // The tree moved while the review was failing/backing off: the new
        // content must earn its own 7s of stillness before any retry.
        observe(&mut gate, probe("/r", "h1", Some(43)), t(base, 16));
        assert_eq!(gate.take_dispatch(t(base, 16)), None);
        observe(&mut gate, probe("/r", "h1", Some(43)), t(base, 20));
        assert_eq!(gate.take_dispatch(t(base, 20)), None);
        observe(&mut gate, probe("/r", "h1", Some(43)), t(base, 24));
        assert_eq!(
            gate.take_dispatch(t(base, 24)),
            Some(Utterance::Review("/r".into(), Trigger::WorkingDiff(43)))
        );
    }

    #[test]
    fn failed_review_after_focus_change_is_dropped() {
        let base = Instant::now();
        let mut gate = BuddyGate::new();
        observe(&mut gate, probe("/r", "h1", None), t(base, 0));
        observe(&mut gate, probe("/r", "h2", None), t(base, 4));
        assert!(gate.take_dispatch(t(base, 4)).is_some());
        // Focus moved while the review was in flight; its failure must NOT
        // requeue a review that would pair with the new pane's context.
        gate.focus_changed();
        gate.utterance_failed();
        assert_eq!(gate.take_dispatch(t(base, 8)), None);
    }

    #[test]
    fn failed_commit_review_is_requeued() {
        let base = Instant::now();
        let mut gate = BuddyGate::new();
        observe(&mut gate, probe("/r", "h1", None), t(base, 0));
        observe(&mut gate, probe("/r", "h2", None), t(base, 4));
        assert!(gate.take_dispatch(t(base, 4)).is_some());
        gate.utterance_failed();
        assert_eq!(
            gate.take_dispatch(t(base, 8)),
            Some(Utterance::Review("/r".into(), Trigger::Commit("h2".into())))
        );
    }

    #[test]
    fn failure_never_clobbers_a_newer_candidate() {
        let base = Instant::now();
        let mut gate = BuddyGate::new();
        observe(&mut gate, probe("/r", "h1", None), t(base, 0));
        observe(&mut gate, probe("/r", "h2", None), t(base, 4));
        assert!(gate.take_dispatch(t(base, 4)).is_some());
        // A newer commit is observed while the h2 review runs and fails.
        observe(&mut gate, probe("/r", "h3", None), t(base, 8));
        gate.utterance_failed();
        assert_eq!(
            gate.take_dispatch(t(base, 12)),
            Some(Utterance::Review("/r".into(), Trigger::Commit("h3".into())))
        );
        gate.utterance_succeeded(Some(("/r".into(), 1)));
        // The failed h2 does not resurface behind it.
        assert_eq!(gate.take_dispatch(t(base, 16)), None);
    }

    #[test]
    fn failed_reaction_is_not_retried() {
        let base = Instant::now();
        let mut gate = BuddyGate::new();
        observe(&mut gate, probe("/r", "h1", None), t(base, 0));
        gate.job_finished(t(base, 4));
        assert_eq!(gate.take_dispatch(t(base, 4)), Some(Utterance::Reaction));
        gate.utterance_failed();
        assert_eq!(gate.take_dispatch(t(base, 8)), None);
    }

    #[test]
    fn cancelled_working_review_does_not_redispatch_same_snapshot() {
        let base = Instant::now();
        let mut gate = BuddyGate::new();
        observe(&mut gate, probe("/r", "h1", Some(42)), t(base, 0));
        observe(&mut gate, probe("/r", "h1", Some(42)), t(base, 8));
        assert!(gate.take_dispatch(t(base, 8)).is_some());
        gate.utterance_cancelled();
        observe(&mut gate, probe("/r", "h1", Some(42)), t(base, 16));
        assert_eq!(gate.take_dispatch(t(base, 16)), None);
    }

    #[test]
    fn focus_change_drops_candidate_and_pending() {
        let base = Instant::now();
        let mut gate = BuddyGate::new();
        observe(&mut gate, probe("/r", "h1", Some(42)), t(base, 0));
        observe(&mut gate, probe("/r", "h1", Some(42)), t(base, 8));
        gate.focus_changed();
        assert_eq!(gate.take_dispatch(t(base, 8)), None);
        // Pending was cleared too: stability restarts from the next probe.
        observe(&mut gate, probe("/r", "h1", Some(42)), t(base, 12));
        assert_eq!(gate.take_dispatch(t(base, 12)), None);
        observe(&mut gate, probe("/r", "h1", Some(42)), t(base, 20));
        assert_eq!(
            gate.take_dispatch(t(base, 20)),
            Some(Utterance::Review("/r".into(), Trigger::WorkingDiff(42)))
        );
    }

    #[test]
    fn focus_change_invalidates_inflight_probe() {
        let base = Instant::now();
        let mut gate = BuddyGate::new();
        observe(&mut gate, probe("/r", "h1", None), t(base, 0));
        let generation = gate.want_probe().unwrap();
        gate.focus_changed();
        // The stale result would have looked like a commit — it must be
        // dropped, and probing must be available again immediately.
        gate.probe_done(generation, probe("/r", "h2", None), t(base, 4));
        assert_eq!(gate.take_dispatch(t(base, 4)), None);
        assert!(gate.want_probe().is_some());
    }

    #[test]
    fn repo_change_in_same_pane_stales_inflight_review() {
        let base = Instant::now();
        let mut gate = BuddyGate::new();
        observe(&mut gate, probe("/r", "h1", None), t(base, 0));
        observe(&mut gate, probe("/r", "h2", None), t(base, 4));
        assert!(gate.take_dispatch(t(base, 4)).is_some());
        // The user cd'd into another repo in the SAME pane while the review
        // ran: a failure must not requeue the old repo's commit against the
        // new repo's terminal context.
        observe(&mut gate, probe("/other", "ho", None), t(base, 8));
        gate.utterance_failed();
        assert_eq!(gate.take_dispatch(t(base, 12)), None);
    }

    #[test]
    fn head_movement_without_commit_is_not_reviewed() {
        let base = Instant::now();
        let mut gate = BuddyGate::new();
        observe(&mut gate, probe("/r", "h1", None), t(base, 0));
        // Branch checkout / reset: HEAD moved but nothing was committed.
        let moved = ProbeResult {
            repo_root: Some("/r".into()),
            head: Some("old-tip".into()),
            head_committed: false,
            snapshot_hash: None,
        };
        observe(&mut gate, moved, t(base, 4));
        assert_eq!(gate.take_dispatch(t(base, 4)), None);
        // And the recorded head DID advance: coming back with a real commit
        // from here triggers exactly once.
        observe(&mut gate, probe("/r", "new-commit", None), t(base, 8));
        assert_eq!(
            gate.take_dispatch(t(base, 8)),
            Some(Utterance::Review(
                "/r".into(),
                Trigger::Commit("new-commit".into())
            ))
        );
    }
}
