//! Three-state terminal activity. `Unknown` means "no trustworthy signal"
//! and is deliberately NOT `Idle`: a remote pane with no telemetry must
//! never be read as "finished", because a false Idle authorises cues,
//! releases the caffeinate hold, and gates a write into the terminal.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    Unknown,
    Idle,
    Busy,
}

impl Activity {
    pub fn is_busy(self) -> bool {
        matches!(self, Activity::Busy)
    }

    /// True ONLY for a positively-observed prompt. `Unknown` is false.
    pub fn is_idle(self) -> bool {
        matches!(self, Activity::Idle)
    }

    /// Workspace-wide reduction: any Busy wins; otherwise any Unknown wins;
    /// otherwise Idle. An empty set is Idle (nothing is running).
    pub fn aggregate(items: impl Iterator<Item = Activity>) -> Activity {
        let mut seen_unknown = false;
        for item in items {
            match item {
                Activity::Busy => return Activity::Busy,
                Activity::Unknown => seen_unknown = true,
                Activity::Idle => {}
            }
        }
        if seen_unknown {
            Activity::Unknown
        } else {
            Activity::Idle
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_busy_wins() {
        let a =
            Activity::aggregate([Activity::Idle, Activity::Unknown, Activity::Busy].into_iter());
        assert_eq!(a, Activity::Busy);
    }

    #[test]
    fn unknown_beats_idle() {
        let a = Activity::aggregate([Activity::Idle, Activity::Unknown].into_iter());
        assert_eq!(a, Activity::Unknown);
    }

    #[test]
    fn all_idle_is_idle() {
        let a = Activity::aggregate([Activity::Idle, Activity::Idle].into_iter());
        assert_eq!(a, Activity::Idle);
    }

    #[test]
    fn empty_is_idle() {
        assert_eq!(Activity::aggregate(std::iter::empty()), Activity::Idle);
    }

    #[test]
    fn unknown_is_not_idle() {
        assert_ne!(Activity::Unknown, Activity::Idle);
        assert!(!Activity::Unknown.is_idle());
        assert!(!Activity::Unknown.is_busy());
    }
}
