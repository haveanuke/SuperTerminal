//! Keep-awake hold state: who wants the Mac awake and why. Two independent
//! sources — the manual rail toggle and the auto hold (terminals busy while
//! the setting is on). Pure state machine, no gpui, no process handling; the
//! workspace owns the actual `caffeinate` child and syncs it to `held()`.

use std::time::{Duration, Instant};
use superterminal_core::activity::Activity;

/// How long every terminal must stay idle before the auto hold lets go —
/// stops flapping between quick sequential commands.
pub const AUTO_RELEASE_GRACE: Duration = Duration::from_secs(10);

#[derive(Debug, Default)]
pub struct AwakeHold {
    manual: bool,
    auto: bool,
    idle_since: Option<Instant>,
}

impl AwakeHold {
    pub fn toggle_manual(&mut self) {
        self.manual = !self.manual;
    }

    /// Drop the manual hold (external `caffeinate` kill must not leave the
    /// toggle lying). The auto hold re-acquires on its own if still busy.
    pub fn clear_manual(&mut self) {
        self.manual = false;
    }

    /// One sample. `Unknown` is a hold-steady signal: it neither acquires
    /// nor releases the hold. It also RESTARTS the idle grace rather than
    /// letting it age across an untrusted gap — otherwise a long Unknown
    /// interval would release the moment one Idle sample landed.
    pub fn tick(&mut self, auto_enabled: bool, activity: Activity, now: Instant) -> bool {
        if !auto_enabled {
            self.auto = false;
            self.idle_since = None;
        } else if activity.is_busy() {
            self.auto = true;
            self.idle_since = None;
        } else if activity.is_idle() && self.auto {
            let since = *self.idle_since.get_or_insert(now);
            if now.duration_since(since) >= AUTO_RELEASE_GRACE {
                self.auto = false;
                self.idle_since = None;
            }
        } else {
            // Unknown: `auto` is deliberately untouched, so this neither
            // acquires nor releases. Clearing `idle_since` restarts the
            // grace at the next Idle instead of counting the untrusted gap.
            self.idle_since = None;
        }
        self.held()
    }

    /// Current answer without advancing the clock.
    pub fn held(&self) -> bool {
        self.manual || self.auto
    }

    /// Whether the auto hold alone is active (busy terminals, or in grace).
    pub fn auto_held(&self) -> bool {
        self.auto
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn starts_released() {
        let mut hold = AwakeHold::default();
        assert!(!hold.tick(false, Activity::Idle, t0()));
        assert!(!hold.held());
    }

    #[test]
    fn manual_toggle_holds_until_toggled_again() {
        let mut hold = AwakeHold::default();
        hold.toggle_manual();
        assert!(hold.tick(false, Activity::Idle, t0()));
        hold.toggle_manual();
        assert!(!hold.tick(false, Activity::Idle, t0()));
    }

    #[test]
    fn auto_acquires_while_busy() {
        let mut hold = AwakeHold::default();
        assert!(hold.tick(true, Activity::Busy, t0()));
    }

    #[test]
    fn auto_does_nothing_when_setting_off() {
        let mut hold = AwakeHold::default();
        assert!(!hold.tick(false, Activity::Busy, t0()));
    }

    #[test]
    fn auto_holds_through_grace_then_releases() {
        let mut hold = AwakeHold::default();
        let t = t0();
        assert!(hold.tick(true, Activity::Busy, t));
        // Idle, but inside the grace window: still held.
        assert!(hold.tick(true, Activity::Idle, t + Duration::from_secs(1)));
        assert!(hold.tick(true, Activity::Idle, t + Duration::from_secs(9)));
        // Grace expired: released.
        assert!(!hold.tick(true, Activity::Idle, t + Duration::from_secs(12)));
    }

    #[test]
    fn busy_during_grace_restarts_the_clock() {
        let mut hold = AwakeHold::default();
        let t = t0();
        assert!(hold.tick(true, Activity::Busy, t));
        assert!(hold.tick(true, Activity::Idle, t + Duration::from_secs(5)));
        // Busy again mid-grace: the idle clock resets.
        assert!(hold.tick(true, Activity::Busy, t + Duration::from_secs(8)));
        assert!(hold.tick(true, Activity::Idle, t + Duration::from_secs(9)));
        // 9s idle since the reset at t+9: still held.
        assert!(hold.tick(true, Activity::Idle, t + Duration::from_secs(18)));
        // 11s idle since the reset: released.
        assert!(!hold.tick(true, Activity::Idle, t + Duration::from_secs(20)));
    }

    #[test]
    fn manual_hold_survives_auto_release() {
        let mut hold = AwakeHold::default();
        let t = t0();
        hold.toggle_manual();
        assert!(hold.tick(true, Activity::Busy, t));
        // Idle observed: grace starts, then expires — auto gone, manual holds.
        assert!(hold.tick(true, Activity::Idle, t + Duration::from_secs(1)));
        assert!(hold.tick(true, Activity::Idle, t + Duration::from_secs(30)));
        // Only dropping manual too releases it.
        hold.toggle_manual();
        assert!(!hold.tick(true, Activity::Idle, t + Duration::from_secs(31)));
    }

    #[test]
    fn disabling_setting_releases_auto_immediately() {
        let mut hold = AwakeHold::default();
        let t = t0();
        assert!(hold.tick(true, Activity::Busy, t));
        // Setting switched off: no grace, released at once even while busy.
        assert!(!hold.tick(false, Activity::Busy, t + Duration::from_secs(1)));
    }

    #[test]
    fn auto_held_reports_only_the_auto_hold() {
        let mut hold = AwakeHold::default();
        hold.toggle_manual();
        assert!(!hold.auto_held());
        hold.tick(true, Activity::Busy, t0());
        assert!(hold.auto_held());
    }

    #[test]
    fn clear_manual_releases_manual_hold() {
        let mut hold = AwakeHold::default();
        hold.toggle_manual();
        assert!(hold.tick(false, Activity::Idle, t0()));
        hold.clear_manual();
        assert!(!hold.tick(false, Activity::Idle, t0()));
        assert!(!hold.held());
    }

    #[test]
    fn unknown_neither_acquires_nor_releases() {
        let base = Instant::now();
        let mut hold = AwakeHold::default();
        // Unknown from a cold start must NOT acquire.
        assert!(!hold.tick(true, Activity::Unknown, base));
        // Busy acquires.
        assert!(hold.tick(true, Activity::Busy, base));
        // Unknown must NOT release, however long it lasts.
        assert!(hold.tick(true, Activity::Unknown, base + Duration::from_secs(600)));
    }

    #[test]
    fn the_idle_grace_restarts_rather_than_ageing_through_unknown() {
        // A real Idle sample first, so `idle_since` is genuinely set. Then a
        // long Unknown gap. An implementation that let the grace age across
        // that gap — or that treated Unknown as Idle — releases here. The
        // hold must survive, and must then need a FRESH full grace.
        let base = Instant::now();
        let mut hold = AwakeHold::default();
        assert!(hold.tick(true, Activity::Busy, base));
        assert!(hold.tick(true, Activity::Idle, base + Duration::from_secs(1)));
        assert!(
            hold.tick(true, Activity::Unknown, base + Duration::from_secs(600)),
            "a long Unknown gap must not release the hold"
        );
        assert!(
            hold.tick(true, Activity::Idle, base + Duration::from_secs(601)),
            "the grace must restart at the Idle sample, not have aged"
        );
        // And a full fresh grace after that Idle does release.
        assert!(!hold.tick(true, Activity::Idle, base + Duration::from_secs(612)));
    }

    #[test]
    fn idle_still_releases_after_the_grace() {
        let base = Instant::now();
        let mut hold = AwakeHold::default();
        assert!(hold.tick(true, Activity::Busy, base));
        assert!(hold.tick(true, Activity::Idle, base + Duration::from_secs(1)));
        assert!(!hold.tick(true, Activity::Idle, base + Duration::from_secs(120)));
    }
}
