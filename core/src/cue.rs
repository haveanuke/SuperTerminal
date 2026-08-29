//! Audio-cue decision logic, pure and clock-injected.
//!
//! One `CueGate` per terminal. The caller samples each tick and reports:
//! - `busy`: foreground process group != shell (a job owns the terminal),
//! - `bell`: a cue-worthy bell arrived since the last tick. "Cue-worthy" is
//!   decided at bell ARRIVAL by the caller (bells rung while the user is
//!   looking at that pane — pane focused AND window active — and bells rung
//!   while audio cues are disabled never reach the gate).
//!
//! The gate answers with at most one cue: `Ping` (a program asked for
//! attention) or `Glass` (a job that worked 5s+ returned to the prompt).
//! There is NO output-timing inference here — that heuristic produced false
//! pings from idle TUIs and was removed deliberately.

use crate::activity::Activity;
use std::time::{Duration, Instant};

/// A job must have run this long for its finish to earn a Glass.
const MIN_BUSY: Duration = Duration::from_secs(5);
/// Minimum spacing between cues from one terminal. Bells inside the gap are
/// DISCARDED, never queued — a delayed contextless ding is worse than none.
const MIN_GAP: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CueKind {
    Ping,
    Glass,
}

/// One tick's answers. `long_job_finished` reports the busy(5s+)->prompt
/// transition ITSELF, independent of cue gating — the buddy's reaction
/// trigger listens to it and must see transitions even when a cue was
/// gap-suppressed or audio is off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickOutcome {
    pub cue: Option<CueKind>,
    pub long_job_finished: bool,
}

#[derive(Debug, Default)]
pub struct CueGate {
    busy: bool,
    busy_since: Option<Instant>,
    last_cue: Option<Instant>,
}

impl CueGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// One sample. At most one cue; when a bell and a job-finish land on
    /// the same tick the explicit signal (Ping) wins.
    ///
    /// `Unknown` is not `Idle`: it can never complete a job, and it RESETS
    /// the busy clock so an untrusted interval is never counted toward
    /// MIN_BUSY when work resumes.
    pub fn tick(&mut self, now: Instant, activity: Activity, bell: bool) -> TickOutcome {
        let gap_ok = self
            .last_cue
            .is_none_or(|last| now.duration_since(last) >= MIN_GAP);
        let busy = activity.is_busy();
        let finished = self.busy
            && activity.is_idle()
            && self
                .busy_since
                .is_some_and(|since| now.duration_since(since) >= MIN_BUSY);
        if busy && !self.busy {
            self.busy_since = Some(now);
        } else if !busy {
            // Idle AND Unknown both clear the clock.
            self.busy_since = None;
        }
        self.busy = busy;
        let cue = if bell && gap_ok {
            Some(CueKind::Ping)
        } else if finished && gap_ok {
            Some(CueKind::Glass)
        } else {
            None
        };
        if cue.is_some() {
            self.last_cue = Some(now);
        }
        TickOutcome {
            cue,
            long_job_finished: finished,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(base: Instant, secs: f32) -> Instant {
        base + Duration::from_millis((secs * 1000.0) as u64)
    }

    #[test]
    fn bell_pings() {
        let base = Instant::now();
        let mut gate = CueGate::new();
        assert_eq!(
            gate.tick(t(base, 0.0), Activity::Busy, true).cue,
            Some(CueKind::Ping)
        );
    }

    #[test]
    fn bell_inside_gap_is_discarded_not_queued() {
        let base = Instant::now();
        let mut gate = CueGate::new();
        assert_eq!(
            gate.tick(t(base, 0.0), Activity::Busy, true).cue,
            Some(CueKind::Ping)
        );
        // 3s later: inside MIN_GAP — discarded.
        assert_eq!(gate.tick(t(base, 3.0), Activity::Busy, true).cue, None);
        // Later tick WITHOUT a bell: the discarded bell must not resurface.
        assert_eq!(gate.tick(t(base, 6.0), Activity::Busy, false).cue, None);
    }

    #[test]
    fn bells_spaced_past_gap_both_ping() {
        let base = Instant::now();
        let mut gate = CueGate::new();
        assert_eq!(
            gate.tick(t(base, 0.0), Activity::Busy, true).cue,
            Some(CueKind::Ping)
        );
        assert_eq!(
            gate.tick(t(base, 6.0), Activity::Busy, true).cue,
            Some(CueKind::Ping)
        );
    }

    #[test]
    fn long_job_finishing_glasses() {
        let base = Instant::now();
        let mut gate = CueGate::new();
        assert_eq!(gate.tick(t(base, 0.0), Activity::Busy, false).cue, None);
        assert_eq!(
            gate.tick(t(base, 6.0), Activity::Idle, false).cue,
            Some(CueKind::Glass)
        );
    }

    #[test]
    fn quick_job_finishing_is_silent() {
        let base = Instant::now();
        let mut gate = CueGate::new();
        assert_eq!(gate.tick(t(base, 0.0), Activity::Busy, false).cue, None);
        assert_eq!(gate.tick(t(base, 2.0), Activity::Idle, false).cue, None);
    }

    #[test]
    fn busy_from_first_sight_earns_glass() {
        // Gate created while a job is already running (e.g. pane opened into
        // a session): first observed busy tick starts the clock.
        let base = Instant::now();
        let mut gate = CueGate::new();
        assert_eq!(gate.tick(t(base, 0.0), Activity::Busy, false).cue, None);
        assert_eq!(gate.tick(t(base, 3.0), Activity::Busy, false).cue, None);
        assert_eq!(
            gate.tick(t(base, 6.0), Activity::Idle, false).cue,
            Some(CueKind::Glass)
        );
    }

    #[test]
    fn bell_and_finish_same_tick_plays_one_cue_ping() {
        let base = Instant::now();
        let mut gate = CueGate::new();
        assert_eq!(gate.tick(t(base, 0.0), Activity::Busy, false).cue, None);
        assert_eq!(
            gate.tick(t(base, 6.0), Activity::Idle, true).cue,
            Some(CueKind::Ping)
        );
    }

    #[test]
    fn glass_shares_the_gap_with_ping() {
        let base = Instant::now();
        let mut gate = CueGate::new();
        assert_eq!(
            gate.tick(t(base, 0.0), Activity::Busy, true).cue,
            Some(CueKind::Ping)
        );
        // Job finishes 3s after the ping: inside the shared gap — silent.
        assert_eq!(gate.tick(t(base, 3.0), Activity::Busy, false).cue, None);
        assert_eq!(gate.tick(t(base, 4.0), Activity::Idle, false).cue, None);
    }

    #[test]
    fn glass_after_gap_from_ping_plays() {
        let base = Instant::now();
        let mut gate = CueGate::new();
        assert_eq!(
            gate.tick(t(base, 0.0), Activity::Busy, true).cue,
            Some(CueKind::Ping)
        );
        assert_eq!(gate.tick(t(base, 3.0), Activity::Busy, false).cue, None);
        assert_eq!(
            gate.tick(t(base, 7.0), Activity::Idle, false).cue,
            Some(CueKind::Glass)
        );
    }

    #[test]
    fn idle_tui_with_no_bell_never_pings() {
        // Regression for the removed heuristic: an idle TUI (busy forever,
        // never ringing) stays silent no matter how ticks land.
        let base = Instant::now();
        let mut gate = CueGate::new();
        for i in 0..300 {
            assert_eq!(
                gate.tick(t(base, i as f32 * 0.9), Activity::Busy, false)
                    .cue,
                None
            );
        }
    }

    #[test]
    fn long_job_finish_reported_even_when_cue_is_gap_suppressed() {
        let base = Instant::now();
        let mut gate = CueGate::new();
        assert_eq!(gate.tick(t(base, 0.0), Activity::Busy, false).cue, None);
        assert_eq!(
            gate.tick(t(base, 3.0), Activity::Busy, true).cue,
            Some(CueKind::Ping)
        );
        // Job ran 6s (long) but finished 3s after the ping: Glass is inside
        // the gap, yet the transition itself must still reach the buddy.
        let outcome = gate.tick(t(base, 6.0), Activity::Idle, false);
        assert_eq!(outcome.cue, None);
        assert!(outcome.long_job_finished);
    }

    #[test]
    fn quick_job_finish_is_not_a_long_job() {
        let base = Instant::now();
        let mut gate = CueGate::new();
        assert!(
            !gate
                .tick(t(base, 0.0), Activity::Busy, false)
                .long_job_finished
        );
        assert!(
            !gate
                .tick(t(base, 2.0), Activity::Idle, false)
                .long_job_finished
        );
    }

    #[test]
    fn long_job_finish_accompanies_glass() {
        let base = Instant::now();
        let mut gate = CueGate::new();
        gate.tick(t(base, 0.0), Activity::Busy, false);
        let outcome = gate.tick(t(base, 6.0), Activity::Idle, false);
        assert_eq!(outcome.cue, Some(CueKind::Glass));
        assert!(outcome.long_job_finished);
    }

    #[test]
    fn glass_only_fires_on_transition_not_while_idle() {
        let base = Instant::now();
        let mut gate = CueGate::new();
        assert_eq!(gate.tick(t(base, 0.0), Activity::Busy, false).cue, None);
        assert_eq!(
            gate.tick(t(base, 6.0), Activity::Idle, false).cue,
            Some(CueKind::Glass)
        );
        // Staying idle must not re-fire.
        assert_eq!(gate.tick(t(base, 12.0), Activity::Idle, false).cue, None);
        assert_eq!(gate.tick(t(base, 18.0), Activity::Idle, false).cue, None);
    }

    #[test]
    fn busy_to_unknown_is_not_a_finish() {
        let base = Instant::now();
        let mut gate = CueGate::new();
        gate.tick(t(base, 0.0), Activity::Busy, false);
        let outcome = gate.tick(t(base, 6.0), Activity::Unknown, false);
        assert_eq!(outcome.cue, None);
        assert!(!outcome.long_job_finished);
    }

    #[test]
    fn unknown_resets_the_busy_clock() {
        // Busy 6s, then Unknown, then Busy again for only 2s before Idle.
        // The second run is SHORT, so no Glass: the Unknown interval must
        // not have been carried into it.
        let base = Instant::now();
        let mut gate = CueGate::new();
        gate.tick(t(base, 0.0), Activity::Busy, false);
        gate.tick(t(base, 6.0), Activity::Unknown, false);
        gate.tick(t(base, 7.0), Activity::Busy, false);
        let outcome = gate.tick(t(base, 9.0), Activity::Idle, false);
        assert_eq!(outcome.cue, None);
        assert!(!outcome.long_job_finished);
    }

    #[test]
    fn unknown_to_idle_is_not_a_finish() {
        let base = Instant::now();
        let mut gate = CueGate::new();
        gate.tick(t(base, 0.0), Activity::Unknown, false);
        let outcome = gate.tick(t(base, 9.0), Activity::Idle, false);
        assert_eq!(outcome.cue, None);
        assert!(!outcome.long_job_finished);
    }

    #[test]
    fn a_bell_still_pings_while_unknown() {
        // Telemetry absence must not silence an explicit attention request.
        let base = Instant::now();
        let mut gate = CueGate::new();
        assert_eq!(
            gate.tick(t(base, 0.0), Activity::Unknown, true).cue,
            Some(CueKind::Ping)
        );
    }

    /// Legacy reference model: the pre-Activity implementation, verbatim.
    /// Any sequence of LOCAL-only samples must agree with the new gate.
    #[derive(Default)]
    struct LegacyGate {
        busy: bool,
        busy_since: Option<Instant>,
        last_cue: Option<Instant>,
    }

    impl LegacyGate {
        fn tick(&mut self, now: Instant, busy: bool, bell: bool) -> TickOutcome {
            let gap_ok = self
                .last_cue
                .is_none_or(|last| now.duration_since(last) >= MIN_GAP);
            let finished = self.busy
                && !busy
                && self
                    .busy_since
                    .is_some_and(|since| now.duration_since(since) >= MIN_BUSY);
            if busy && !self.busy {
                self.busy_since = Some(now);
            } else if !busy {
                self.busy_since = None;
            }
            self.busy = busy;
            let cue = if bell && gap_ok {
                Some(CueKind::Ping)
            } else if finished && gap_ok {
                Some(CueKind::Glass)
            } else {
                None
            };
            if cue.is_some() {
                self.last_cue = Some(now);
            }
            TickOutcome {
                cue,
                long_job_finished: finished,
            }
        }
    }

    #[test]
    fn local_only_sequences_match_the_legacy_gate() {
        // Deterministic pseudo-random walk: no rand dependency.
        let base = Instant::now();
        let mut new = CueGate::new();
        let mut old = LegacyGate::default();
        let mut state: u64 = 0x2545_F491_4F6C_DD1D;
        let mut clock = 0.0f32;
        for _ in 0..4000 {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let busy = (state >> 33) & 1 == 1;
            let bell = (state >> 34) & 3 == 0;
            clock += ((state >> 36) & 7) as f32 * 0.7;
            let now = t(base, clock);
            let a = if busy { Activity::Busy } else { Activity::Idle };
            assert_eq!(new.tick(now, a, bell), old.tick(now, busy, bell));
        }
    }
}
