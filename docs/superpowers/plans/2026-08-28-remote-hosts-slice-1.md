# Remote Hosts Slice 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the activity and target foundations that must exist before any remote pane can be created, fixing the local-authority defects that would otherwise let a remote tab act on local state.

**Architecture:** A three-state `Activity` replaces the busy boolean end to end, where `Unknown` is never `Idle`. Panes gain a `Target` (Local or Remote) that persists in the layout. Panels gain an explicit target plus an operation token so stale async work cannot write to a new target. Every focus mutation routes through one helper that retargets panels synchronously. No remote pane can be created in this slice — no UI path constructs one.

**Tech Stack:** Rust 2021, gpui `=0.2.2`, alacritty_terminal `=0.26.0`, serde/serde_json. No new crates.

**Spec:** `docs/superpowers/specs/2026-08-28-remote-hosts-design.md`

## Global Constraints

- No new crate dependencies. `gpui` and `alacritty_terminal` are pinned with `=` and must never be forked or patched.
- Hand-rolled `extern "C"` FFI, never the `libc` crate. See `native/src/companion/net.rs` for the house style.
- No emoji in rendered UI. Use the SVG set in `native/src/icons.rs`.
- No attribution trailers in commit messages.
- Run `cargo fmt` before every commit; run `cargo test` from `native/` (workspace root for `core` tests).
- Local panes must keep today's behaviour for: activity semantics (cue, `finished`, caffeinate, buddy, sidebar), legacy wire fields, and serialized layouts. Atomic panel retargeting is an intentional exception — it changes local focus timing deliberately.

---

### Task 1: `Activity` tri-state and `CueGate` migration

**Files:**
- Create: `core/src/activity.rs`
- Modify: `core/src/lib.rs` (add `pub mod activity;`)
- Modify: `core/src/cue.rs`
- Test: `core/src/activity.rs` (inline `#[cfg(test)]`), `core/src/cue.rs` (inline)

**Interfaces:**
- Consumes: nothing.
- Produces: `superterminal_core::activity::Activity` with variants `Unknown`, `Idle`, `Busy`; `Activity::aggregate(impl Iterator<Item = Activity>) -> Activity`; `CueGate::tick(&mut self, now: Instant, activity: Activity, bell: bool) -> TickOutcome`.

- [ ] **Step 1: Write the failing test for `Activity::aggregate`**

Add to `core/src/activity.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_busy_wins() {
        let a = Activity::aggregate([Activity::Idle, Activity::Unknown, Activity::Busy].into_iter());
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p superterminal-core activity`
Expected: FAIL — `core/src/activity.rs` does not exist / unresolved module.

- [ ] **Step 3: Write minimal implementation**

Create `core/src/activity.rs`:

```rust
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
```

Add to `core/src/lib.rs`, alongside the existing module declarations:

```rust
pub mod activity;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p superterminal-core activity`
Expected: PASS (5 tests).

- [ ] **Step 5: Write the failing `CueGate` tri-state tests**

Add these to the existing `mod tests` in `core/src/cue.rs`:

```rust
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
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let busy = (state >> 33) & 1 == 1;
            let bell = (state >> 34) & 3 == 0;
            clock += ((state >> 36) & 7) as f32 * 0.7;
            let now = t(base, clock);
            let a = if busy { Activity::Busy } else { Activity::Idle };
            assert_eq!(new.tick(now, a, bell), old.tick(now, busy, bell));
        }
    }
```

Add `use superterminal_core::activity::Activity;` — since this IS `superterminal-core`, use `use crate::activity::Activity;` at the top of `core/src/cue.rs`.

- [ ] **Step 6: Run tests to verify they fail**

Run: `cargo test -p superterminal-core cue`
Expected: FAIL — `tick` expects `bool`, not `Activity`.

- [ ] **Step 7: Migrate `CueGate` to `Activity`**

In `core/src/cue.rs`, add the import and replace `tick`:

```rust
use crate::activity::Activity;
```

```rust
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
```

- [ ] **Step 8: Update the existing cue tests to `Activity`**

In `core/src/cue.rs` `mod tests`, replace every `gate.tick(t(base, X), true, B)` with `gate.tick(t(base, X), Activity::Busy, B)` and every `gate.tick(t(base, X), false, B)` with `gate.tick(t(base, X), Activity::Idle, B)`. No assertion values change — that is the point.

- [ ] **Step 9: Run the full core suite**

Run: `cargo test -p superterminal-core`
Expected: PASS. Every pre-existing cue test passes with unchanged assertions, plus 5 new tests.

- [ ] **Step 10: Commit**

```bash
cargo fmt
git add core/src/activity.rs core/src/lib.rs core/src/cue.rs
git commit -m "feat(core): three-state Activity, and CueGate that cannot finish on Unknown"
```

---

### Task 2: `AwakeHold` tri-state

**Files:**
- Modify: `native/src/awake.rs`
- Test: `native/src/awake.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Consumes: `superterminal_core::activity::Activity` (Task 1).
- Produces: `AwakeHold::tick(&mut self, auto_enabled: bool, activity: Activity, now: Instant) -> bool`.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` in `native/src/awake.rs`:

```rust
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
    fn the_idle_grace_does_not_age_through_unknown() {
        // Busy, then a long Unknown gap, then ONE Idle sample. The grace
        // must start at that Idle sample, not have elapsed during Unknown —
        // otherwise the hold releases the instant telemetry returns.
        let base = Instant::now();
        let mut hold = AwakeHold::default();
        assert!(hold.tick(true, Activity::Busy, base));
        assert!(hold.tick(true, Activity::Unknown, base + Duration::from_secs(600)));
        assert!(hold.tick(true, Activity::Idle, base + Duration::from_secs(601)));
    }

    #[test]
    fn idle_still_releases_after_the_grace() {
        let base = Instant::now();
        let mut hold = AwakeHold::default();
        assert!(hold.tick(true, Activity::Busy, base));
        assert!(hold.tick(true, Activity::Idle, base + Duration::from_secs(1)));
        assert!(!hold.tick(true, Activity::Idle, base + Duration::from_secs(120)));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p superterminal-native awake`
Expected: FAIL — `tick` expects `bool`.

- [ ] **Step 3: Implement**

In `native/src/awake.rs`, add `use superterminal_core::activity::Activity;` and replace `tick`:

```rust
    /// One sample. `Unknown` is a hold-steady signal: it neither acquires
    /// nor releases, and it does NOT advance the idle grace — otherwise a
    /// long untrusted gap would release the moment one Idle sample landed.
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
        }
        // Activity::Unknown falls through untouched: auto and idle_since
        // both keep their current values.
        self.held()
    }
```

- [ ] **Step 4: Update existing awake tests**

Replace `hold.tick(enabled, true, now)` with `hold.tick(enabled, Activity::Busy, now)` and `hold.tick(enabled, false, now)` with `hold.tick(enabled, Activity::Idle, now)` throughout the existing tests. Assertions do not change.

- [ ] **Step 5: Run tests**

Run: `cargo test -p superterminal-native awake`
Expected: PASS, existing assertions unchanged plus 3 new tests.

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add native/src/awake.rs
git commit -m "feat(native): AwakeHold holds steady through Unknown activity"
```

---

### Task 3: Session and pane activity APIs

**Files:**
- Modify: `native/src/term_session.rs`
- Modify: `native/src/pane.rs`
- Test: `native/src/pane.rs` (inline)

**Interfaces:**
- Consumes: `Activity` (Task 1).
- Produces: `TermSession::foreground_activity(&self) -> Activity`; `TermSession::status_activity(&self) -> (Option<String>, Activity)`; `TerminalPane::foreground_activity(&self) -> Activity`; `TerminalPane::companion_activity(&self) -> Activity`; `TerminalPane::status_activity(&self) -> (Option<String>, Activity)`.

A local session NEVER returns `Unknown` — `tcgetpgrp` always yields a definite answer. `Unknown` enters only via a remote target in Task 6.

- [ ] **Step 1: Write the failing test**

Add to `#[cfg(test)] mod tests` in `native/src/pane.rs`:

```rust
    #[test]
    fn a_pane_with_no_session_is_idle_not_unknown() {
        // A pane whose shell never spawned is definitively not working.
        // Unknown is reserved for "we have a session but cannot see it".
        assert_eq!(Activity::from_local_busy(false), Activity::Idle);
        assert_eq!(Activity::from_local_busy(true), Activity::Busy);
    }
```

Add to `core/src/activity.rs` a matching failing test:

```rust
    #[test]
    fn local_busy_maps_to_two_states_only() {
        assert_eq!(Activity::from_local_busy(true), Activity::Busy);
        assert_eq!(Activity::from_local_busy(false), Activity::Idle);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p superterminal-core activity && cargo test -p superterminal-native pane`
Expected: FAIL — `from_local_busy` not found.

- [ ] **Step 3: Add the constructor**

In `core/src/activity.rs`, inside `impl Activity`:

```rust
    /// A local PTY probe is always definite: tcgetpgrp answers, or the
    /// session is gone. Local panes therefore never produce `Unknown`.
    pub fn from_local_busy(busy: bool) -> Activity {
        if busy {
            Activity::Busy
        } else {
            Activity::Idle
        }
    }
```

- [ ] **Step 4: Add the session APIs**

In `native/src/term_session.rs`, add `use superterminal_core::activity::Activity;` and, next to `foreground_busy`:

```rust
    /// Tri-state form of [`Self::foreground_busy`]. A local session is
    /// always definite; `Unknown` is produced only by a remote target,
    /// which has no local process group to probe.
    pub fn foreground_activity(&self) -> Activity {
        Activity::from_local_busy(self.foreground_busy())
    }

    /// Tri-state form of [`Self::status`].
    pub fn status_activity(&self) -> (Option<String>, Activity) {
        let (cwd, busy) = self.status();
        (cwd, Activity::from_local_busy(busy))
    }
```

- [ ] **Step 5: Add the pane APIs**

In `native/src/pane.rs`, add `use superterminal_core::activity::Activity;` and, next to the existing probes:

```rust
    /// Tri-state foreground probe: cues, `finished`, caffeinate, sidebar
    /// dots, and the folder-write guard all read THIS, not the boolean.
    pub fn foreground_activity(&self) -> Activity {
        match self.session.as_ref() {
            Some(session) => session.foreground_activity(),
            None => Activity::Idle,
        }
    }

    /// Tri-state form of the phone's dot. Local behaviour is unchanged:
    /// the agent/output heuristic in [`busy_dot`] still decides, and is
    /// deliberately NOT merged with [`Self::foreground_activity`].
    pub fn companion_activity(&self) -> Activity {
        Activity::from_local_busy(self.companion_busy())
    }

    /// Tri-state form of [`Self::status`].
    pub fn status_activity(&self) -> (Option<String>, Activity) {
        match self.session.as_ref() {
            Some(session) => session.status_activity(),
            None => (None, Activity::Idle),
        }
    }
```

- [ ] **Step 6: Run tests**

Run: `cargo test -p superterminal-core activity && cargo test -p superterminal-native pane`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
cargo fmt
git add core/src/activity.rs native/src/term_session.rs native/src/pane.rs
git commit -m "feat(native): tri-state activity probes on session and pane"
```

---

### Task 4: Wire the workspace consumers to `Activity`

**Files:**
- Modify: `native/src/workspace/mod.rs` (`cue_tick` around 470, caffeinate around 641, sidebar cache around 748)

**Interfaces:**
- Consumes: Tasks 1-3.
- Produces: no new public API; `cue_tick` and the caffeinate probe now pass `Activity` through.

- [ ] **Step 1: Convert `cue_tick`**

In `native/src/workspace/mod.rs`, in `cue_tick`, replace the sample line:

```rust
            let (activity, bell) =
                pane.update(cx, |pane, _| (pane.foreground_activity(), pane.take_bell()));
            let gate = self.cue_gates.entry(id.clone()).or_default();
            let outcome = gate.tick(now, activity, bell && audio_on);
```

Nothing else in `cue_tick` changes: `long_job_finished` still gates `hub.bump_finished(id)` and the focused buddy trigger, so the `MIN_BUSY` qualification is preserved exactly.

- [ ] **Step 2: Convert the caffeinate probe**

Replace the `any_busy` block around line 641:

```rust
        if self.pet_tick_count.is_multiple_of(3) {
            let activity = if self.settings.auto_caffeinate {
                superterminal_core::activity::Activity::aggregate(
                    self.panes.values().map(|pane| pane.read(cx).foreground_activity()),
                )
            } else {
                superterminal_core::activity::Activity::Idle
            };
            let was_held = self.caffeinate_child.is_some();
            self.awake.tick(
                self.settings.auto_caffeinate,
                activity,
                std::time::Instant::now(),
            );
            self.sync_caffeinate();
            if was_held != self.caffeinate_child.is_some() {
                cx.notify();
            }
        }
```

- [ ] **Step 3: Convert the sidebar status cache**

Replace the `status()` call around line 748 so the cache carries `Activity`:

```rust
                        let (cwd, activity) = pane.read(cx).status_activity();
                        let cwd = cwd.map(|cwd| cwd.replace(&home, "~")).unwrap_or_default();
                        (id.clone(), (cwd, activity))
```

Change the `sidebar_status_cache` field type to `HashMap<String, (String, Activity)>` and update the render site at `workspace/mod.rs:1789` so the existing green / yellow / cyan states are preserved for `Busy` and `Idle`, and `Unknown` renders a fourth, hollow state drawn from `icons.rs`. Do not collapse the three existing states.

- [ ] **Step 4: Build and run the full suite**

Run: `cargo test`
Expected: PASS. Every existing workspace test passes unmodified.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add native/src/workspace/mod.rs
git commit -m "feat(native): route cue, caffeinate, and sidebar through Activity"
```

---

### Task 5: Additive `activity` field on the phone wire

**Files:**
- Modify: `native/src/companion/hub.rs` (`SessionInfo`, `set_meta`)
- Modify: `native/src/companion/server.rs` (`/sessions` handler)
- Modify: `native/src/companion/page.html`
- Modify: `native/src/workspace/mod.rs` (the `set_meta` call around 711)
- Test: `native/src/companion/e2e_tests.rs`

**Interfaces:**
- Consumes: Task 3.
- Produces: `SessionInfo.activity: Activity`; wire field `"activity": "idle" | "busy" | "unknown"`, lowercase; the legacy `"busy"` boolean is retained and unchanged.

- [ ] **Step 1: Write the failing tests**

The companion suite asserts on the page via `include_str!("page.html")` (see
`e2e_tests.rs:263`) and drives the real server over raw TCP. Neither needs a
gpui context. Add to `native/src/companion/e2e_tests.rs`:

```rust
#[test]
fn unknown_activity_never_reads_as_busy_on_the_legacy_flag() {
    // The legacy boolean is what an already-loaded phone page reads.
    // Unknown must present as not-busy there, or an untrusted pane paints
    // orange on every old client.
    let hub: Arc<Hub> = Arc::new(Hub::default());
    let (tx, _rx) = mpsc::channel::<Vec<u8>>();
    hub.register("t1", tx);
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
    let hub: Arc<Hub> = Arc::new(Hub::default());
    let (tx, _rx) = mpsc::channel::<Vec<u8>>();
    hub.register("t1", tx);
    hub.set_meta("t1", "one", true, true);
    assert_eq!(hub.sessions()[0].activity, Activity::Busy);
    hub.set_meta("t1", "one", true, false);
    assert_eq!(hub.sessions()[0].activity, Activity::Idle);
}

#[test]
fn the_page_reads_activity_by_value_not_truthiness() {
    // `s.busy ? ...` treats the string "idle" as busy. The new page must
    // compare against a value, so a page that DOES get the new field can
    // never mispaint.
    let page = include_str!("page.html");
    assert!(page.contains("s.activity"), "page ignores the new field");
    assert!(
        !page.contains(r#"(s.activity ? "#),
        "page must not test s.activity for truthiness"
    );
    assert!(
        page.contains(r#"act === "busy""#),
        "page must compare activity by value"
    );
    assert!(
        page.contains(r#"act === "unknown""#),
        "page must render the unknown state"
    );
}
```

Add `use superterminal_core::activity::Activity;` to the file's imports.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p superterminal-native companion`
Expected: FAIL — `set_meta_activity` not found.

- [ ] **Step 3: Extend `SessionInfo` and `set_meta`**

In `native/src/companion/hub.rs`, add `activity: Activity` to `SessionInfo` (default `Activity::Idle`), and add:

```rust
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
```

Keep the existing `set_meta` delegating to it via `Activity::from_local_busy(busy)` so no other caller breaks.

- [ ] **Step 4: Emit the field**

In the `/sessions` handler in `native/src/companion/server.rs`, alongside the existing `"busy"` field, emit:

```rust
        let activity = match info.activity {
            Activity::Busy => "busy",
            Activity::Idle => "idle",
            Activity::Unknown => "unknown",
        };
```

and include `"activity":"{activity}"` in the JSON object. The `"busy"` field keeps its current position and spelling.

- [ ] **Step 5: Update the caller**

In `native/src/workspace/mod.rs` around line 711, replace:

```rust
                    let activity = pane.read(cx).companion_activity();
                    hub.set_meta_activity(id, &label, true, activity);
```

- [ ] **Step 6: Update the page**

In `native/src/companion/page.html`, replace the dot-class line (currently `dot.className = "dot" + (s.busy ? " busy" : s.alive ? " on" : "")`):

```js
          var act = s.activity || (s.busy ? "busy" : "idle");
          dot.className = "dot" + (act === "busy" ? " busy" : act === "unknown" ? " unk" : s.alive ? " on" : "")
            + (attention[s.id] ? " attn" : "");
```

Add a `.dot.unk` rule to the stylesheet: a hollow ring using the existing dot dimensions and the page's muted border colour. No emoji.

- [ ] **Step 7: Run tests**

Run: `cargo test -p superterminal-native companion`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
cargo fmt
git add native/src/companion/hub.rs native/src/companion/server.rs native/src/companion/page.html native/src/workspace/mod.rs native/src/companion/e2e_tests.rs
git commit -m "feat(companion): additive activity field, legacy busy flag preserved"
```

---

### Task 6: `RemoteProfile`, `ProfileId`, and permissive settings

**Files:**
- Create: `native/src/hosts.rs`
- Modify: `native/src/main.rs` (add `mod hosts;`)
- Modify: `native/src/settings.rs`
- Test: `native/src/hosts.rs` (inline)

**Interfaces:**
- Consumes: nothing.
- Produces: `hosts::ProfileId(String)`; `hosts::HostOs { MacOs, Linux, Windows }`; `hosts::ShellKind { Zsh, Bash, Fish, PowerShell, Cmd }`; `hosts::RemoteProfile { id, label, destination, user, port, os, shell }`; `hosts::new_profile_id() -> ProfileId`; `hosts::validate_destination(&str) -> Result<String, DestinationError>`; `hosts::load_profiles(raw: &serde_json::Value) -> (Vec<RemoteProfile>, Vec<ProfileProblem>)`.

- [ ] **Step 1: Write the failing tests**

Create `native/src/hosts.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_hostname_is_accepted() {
        assert_eq!(validate_destination("pc1").unwrap(), "pc1");
    }

    #[test]
    fn a_magicdns_fqdn_is_accepted() {
        let d = "pc1.tail1a2b3c.ts.net";
        assert_eq!(validate_destination(d).unwrap(), d);
    }

    #[test]
    fn shell_metacharacters_are_rejected() {
        for bad in ["a;rm -rf /", "a$(id)", "a`id`", "a b", "a\nb", "a|b", "a&b"] {
            assert!(validate_destination(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn a_leading_dash_is_rejected() {
        // ssh would read it as an option.
        assert!(validate_destination("-oProxyCommand=id").is_err());
        assert!(validate_destination("-pc1").is_err());
    }

    #[test]
    fn empty_and_overlong_are_rejected() {
        assert!(validate_destination("").is_err());
        assert!(validate_destination(&"a".repeat(64)).is_err());
        let long_fqdn = std::iter::repeat("abcdefghij")
            .take(30)
            .collect::<Vec<_>>()
            .join(".");
        assert!(validate_destination(&long_fqdn).is_err());
    }

    #[test]
    fn ipv6_is_stored_bare_and_brackets_are_stripped() {
        // `ssh -- '[::1]'` tries to resolve the literal "[::1]" and fails.
        assert_eq!(validate_destination("::1").unwrap(), "::1");
        assert_eq!(validate_destination("[::1]").unwrap(), "::1");
        assert_eq!(
            validate_destination("[fe80::1ff:fe23:4567:890a]").unwrap(),
            "fe80::1ff:fe23:4567:890a"
        );
    }

    #[test]
    fn generated_ids_are_unique_and_opaque() {
        let a = new_profile_id();
        let b = new_profile_id();
        assert_ne!(a, b);
        assert_eq!(a.0.len(), 32);
        assert!(a.0.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    fn raw(json: &str) -> serde_json::Value {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn one_invalid_profile_does_not_discard_the_valid_ones() {
        let (ok, problems) = load_profiles(&raw(
            r#"[{"id":"aa","label":"good","destination":"pc1","os":"linux","shell":"bash"},
                {"id":"bb","label":"bad","destination":"a;id","os":"linux","shell":"bash"}]"#,
        ));
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].label, "good");
        assert_eq!(problems.len(), 1);
    }

    #[test]
    fn a_malformed_container_yields_no_profiles_rather_than_an_error() {
        // settings.rs falls back to Settings::default() on any serde error,
        // so a bad container must never reach serde as a hard failure.
        let (ok, problems) = load_profiles(&raw("5"));
        assert!(ok.is_empty());
        assert_eq!(problems.len(), 1);
    }

    #[test]
    fn duplicate_ids_quarantine_every_colliding_profile() {
        // Never first-wins or last-wins: an ambiguous id must not be able
        // to resolve to the wrong host.
        let (ok, problems) = load_profiles(&raw(
            r#"[{"id":"dup","label":"one","destination":"pc1","os":"linux","shell":"bash"},
                {"id":"dup","label":"two","destination":"pc2","os":"linux","shell":"bash"},
                {"id":"solo","label":"three","destination":"pc3","os":"linux","shell":"bash"}]"#,
        ));
        assert_eq!(ok.len(), 1, "only the non-colliding profile survives");
        assert_eq!(ok[0].label, "three");
        assert_eq!(problems.len(), 2, "both colliding profiles are reported");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p superterminal-native hosts`
Expected: FAIL — module does not compile; nothing is defined.

- [ ] **Step 3: Implement `hosts.rs`**

Write the module above the test block:

```rust
//! Remote host profiles. Identity is opaque and app-generated so a label
//! or destination can be edited without silently repointing a saved pane.
//!
//! Nothing here spawns anything: this slice can describe a remote host but
//! cannot open one. See `docs/superpowers/specs/2026-08-28-remote-hosts-design.md`.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProfileId(pub String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostOs {
    MacOs,
    Linux,
    Windows,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShellKind {
    Zsh,
    Bash,
    Fish,
    PowerShell,
    Cmd,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProfile {
    pub id: ProfileId,
    pub label: String,
    /// Already normalised: bare IPv6, no brackets, validated charset.
    pub destination: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    pub os: HostOs,
    pub shell: ShellKind,
}

#[derive(Debug, PartialEq)]
pub struct ProfileProblem {
    pub label: String,
    pub reason: String,
}

#[derive(Debug, PartialEq)]
pub enum DestinationError {
    Empty,
    TooLong,
    LeadingDash,
    BadCharacter,
}

const MAX_DESTINATION: usize = 253;
const MAX_LABEL: usize = 63;

/// 128 bits from /dev/urandom, matching the existing dependency-free
/// pattern in `companion/auth.rs`.
pub fn new_profile_id() -> ProfileId {
    ProfileId(crate::companion::auth::generate_token())
}

/// Validate and normalise an ssh destination. Accepts a dot-separated
/// hostname/FQDN, an IPv4 literal, or a BARE IPv6 literal; brackets around
/// an IPv6 literal are accepted from settings and stripped, because
/// `ssh -- '[::1]'` resolves the literal string "[::1]" and fails.
pub fn validate_destination(raw: &str) -> Result<String, DestinationError> {
    let trimmed = raw.trim();
    let unbracketed = trimmed
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(trimmed);
    if unbracketed.is_empty() {
        return Err(DestinationError::Empty);
    }
    if unbracketed.len() > MAX_DESTINATION {
        return Err(DestinationError::TooLong);
    }
    if unbracketed.starts_with('-') {
        return Err(DestinationError::LeadingDash);
    }
    // IPv6 literal: hex groups and colons only.
    if unbracketed.contains(':') {
        let ok = unbracketed
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == ':');
        return if ok {
            Ok(unbracketed.to_string())
        } else {
            Err(DestinationError::BadCharacter)
        };
    }
    for label in unbracketed.split('.') {
        if label.is_empty() || label.len() > MAX_LABEL {
            return Err(DestinationError::TooLong);
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(DestinationError::BadCharacter);
        }
        if !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(DestinationError::BadCharacter);
        }
    }
    Ok(unbracketed.to_string())
}

/// Permissive profile loading. NEVER returns an error: `settings.rs` falls
/// back to `Settings::default()` on any serde failure, so one hand-edited
/// profile must not be able to reset every unrelated setting. A malformed
/// container degrades to "no profiles", and duplicate ids quarantine EVERY
/// colliding profile rather than picking one.
pub fn load_profiles(raw: &serde_json::Value) -> (Vec<RemoteProfile>, Vec<ProfileProblem>) {
    let mut problems = Vec::new();
    let Some(items) = raw.as_array() else {
        if !raw.is_null() {
            problems.push(ProfileProblem {
                label: String::new(),
                reason: "remoteProfiles is not a list".to_string(),
            });
        }
        return (Vec::new(), problems);
    };
    let mut candidates: Vec<RemoteProfile> = Vec::new();
    for item in items {
        let label = item
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or("(unnamed)")
            .to_string();
        let parsed: RemoteProfile = match serde_json::from_value(item.clone()) {
            Ok(profile) => profile,
            Err(error) => {
                problems.push(ProfileProblem {
                    label,
                    reason: error.to_string(),
                });
                continue;
            }
        };
        if parsed.id.0.is_empty() {
            problems.push(ProfileProblem {
                label,
                reason: "empty id".to_string(),
            });
            continue;
        }
        match validate_destination(&parsed.destination) {
            Ok(destination) => candidates.push(RemoteProfile {
                destination,
                ..parsed
            }),
            Err(error) => problems.push(ProfileProblem {
                label,
                reason: format!("bad destination: {error:?}"),
            }),
        }
    }
    // Quarantine every member of an id collision.
    let mut kept = Vec::new();
    for profile in &candidates {
        let count = candidates.iter().filter(|other| other.id == profile.id).count();
        if count > 1 {
            problems.push(ProfileProblem {
                label: profile.label.clone(),
                reason: format!("duplicate id {}", profile.id.0),
            });
        } else {
            kept.push(profile.clone());
        }
    }
    (kept, problems)
}
```

Add `mod hosts;` to `native/src/main.rs` beside the other module declarations. `companion::auth::generate_token` must be `pub` — it already is.

- [ ] **Step 4: Wire into settings**

In `native/src/settings.rs`, add to `Settings`:

```rust
    /// Raw, so a malformed entry can never fail whole-settings serde and
    /// trip the `unwrap_or_default()` fallback at load.
    #[serde(default)]
    pub remote_profiles: serde_json::Value,
```

and a resolver:

```rust
    /// Validated profiles plus anything that was rejected, for surfacing.
    pub fn profiles(&self) -> (Vec<crate::hosts::RemoteProfile>, Vec<crate::hosts::ProfileProblem>) {
        crate::hosts::load_profiles(&self.remote_profiles)
    }
```

- [ ] **Step 5: Write the settings-preservation test**

Add to `native/src/settings.rs` tests:

```rust
    #[test]
    fn a_malformed_profile_does_not_reset_unrelated_settings() {
        let dir = std::env::temp_dir().join(format!("st-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            r#"{"audioCues":false,"remoteProfiles":[{"id":"x","label":"bad","destination":"a;id","os":"linux","shell":"bash"}]}"#,
        )
        .unwrap();
        let settings = Settings::load_from(&path);
        // The unrelated setting survives.
        assert!(!settings.audio_cues);
        let (ok, problems) = settings.profiles();
        assert!(ok.is_empty());
        assert_eq!(problems.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }
```

- [ ] **Step 6: Run tests**

Run: `cargo test -p superterminal-native hosts && cargo test -p superterminal-native settings`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
cargo fmt
git add native/src/hosts.rs native/src/main.rs native/src/settings.rs
git commit -m "feat(native): remote host profiles with quarantining identity rules"
```

---

### Task 7: `Target` on panes, persisted in the layout

**Files:**
- Modify: `native/src/hosts.rs` (add `Target`)
- Modify: `native/src/layout.rs` (`PaneNode::Terminal`)
- Modify: `native/src/pane.rs` (store the target)
- Test: `native/src/layout.rs` (inline)

**Interfaces:**
- Consumes: `ProfileId` (Task 6).
- Produces: `hosts::Target { Local, Remote(ProfileId) }` with `Target::is_local()`; `PaneNode::Terminal { terminal_id, target }`; `TerminalPane::target(&self) -> &Target`.

- [ ] **Step 1: Write the failing layout tests**

Add to `#[cfg(test)] mod tests` in `native/src/layout.rs`:

```rust
    #[test]
    fn a_local_terminal_serialises_exactly_as_before() {
        // Existing saved layouts must round-trip byte-identically.
        let node = PaneNode::terminal("t1");
        let json = serde_json::to_string(&node).unwrap();
        assert_eq!(json, r#"{"type":"terminal","terminalId":"t1"}"#);
    }

    #[test]
    fn a_layout_without_a_target_loads_as_local() {
        let node: PaneNode =
            serde_json::from_str(r#"{"type":"terminal","terminalId":"t1"}"#).unwrap();
        match node {
            PaneNode::Terminal { target, .. } => assert!(target.is_local()),
            _ => panic!("expected a terminal"),
        }
    }

    #[test]
    fn a_remote_target_round_trips() {
        let node = PaneNode::Terminal {
            terminal_id: "t1".to_string(),
            target: Target::Remote(ProfileId("abc".to_string())),
        };
        let json = serde_json::to_string(&node).unwrap();
        assert!(json.contains(r#""target""#), "{json}");
        let back: PaneNode = serde_json::from_str(&json).unwrap();
        assert_eq!(back, node);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p superterminal-native layout`
Expected: FAIL — `Target` not found.

- [ ] **Step 3: Add `Target`**

In `native/src/hosts.rs`:

```rust
/// Where a pane's shell runs. `Local` is the default and is omitted from
/// serialized layouts so existing session files are unchanged.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Target {
    #[default]
    Local,
    Remote(ProfileId),
}

impl Target {
    pub fn is_local(&self) -> bool {
        matches!(self, Target::Local)
    }

    pub fn profile_id(&self) -> Option<&ProfileId> {
        match self {
            Target::Local => None,
            Target::Remote(id) => Some(id),
        }
    }
}
```

- [ ] **Step 4: Extend `PaneNode`**

In `native/src/layout.rs`:

```rust
    Terminal {
        terminal_id: String,
        /// Omitted when Local, so existing saved layouts are byte-identical
        /// and older files load as Local.
        #[serde(default, skip_serializing_if = "Target::is_local")]
        target: Target,
    },
```

and update the constructor:

```rust
    pub fn terminal(terminal_id: impl Into<String>) -> Self {
        PaneNode::Terminal {
            terminal_id: terminal_id.into(),
            target: Target::Local,
        }
    }
```

Add `use crate::hosts::{ProfileId, Target};`. The compiler will now flag every construction and destructuring site, including `remap_ids` at `workspace/mod.rs:3751` — carry `target` through at each, never defaulting it.

- [ ] **Step 5: Store the target on the pane**

In `native/src/pane.rs`, add a `target: Target` field to `TerminalPane` (defaulting to `Target::Local` in every existing constructor) and:

```rust
    /// Where this pane's shell runs. Local panes behave exactly as before.
    pub fn target(&self) -> &Target {
        &self.target
    }
```

- [ ] **Step 6: Cover a saved target whose profile is gone**

Spec D7: a saved `ProfileId` that no longer exists must NEVER restore as a
local shell — that is exactly how a remote pane would inherit local authority.
No UI creates remote panes in this slice, but a hand-edited layout can.

Add to `#[cfg(test)] mod tests` in `native/src/hosts.rs`:

```rust
    #[test]
    fn a_target_whose_profile_is_missing_does_not_become_local() {
        let profiles: Vec<RemoteProfile> = Vec::new();
        let target = Target::Remote(ProfileId("gone".into()));
        assert_eq!(
            resolve_target(&target, &profiles),
            ResolvedTarget::MissingProfile(ProfileId("gone".into()))
        );
        // The critical assertion: it is not Local.
        assert_ne!(resolve_target(&target, &profiles), ResolvedTarget::Local);
    }

    #[test]
    fn a_target_with_a_live_profile_resolves_to_it() {
        let profiles = vec![RemoteProfile {
            id: ProfileId("p1".into()),
            label: "pc1".into(),
            destination: "pc1".into(),
            user: None,
            port: None,
            os: HostOs::Linux,
            shell: ShellKind::Bash,
        }];
        match resolve_target(&Target::Remote(ProfileId("p1".into())), &profiles) {
            ResolvedTarget::Remote(profile) => assert_eq!(profile.label, "pc1"),
            other => panic!("expected a resolved profile, got {other:?}"),
        }
    }
```

Implement in `native/src/hosts.rs`:

```rust
#[derive(Debug, PartialEq)]
pub enum ResolvedTarget<'a> {
    Local,
    Remote(&'a RemoteProfile),
    /// The pane restores dead, labelled with the missing id, reconnect
    /// disabled. It must never silently fall back to a local shell.
    MissingProfile(ProfileId),
}

pub fn resolve_target<'a>(target: &Target, profiles: &'a [RemoteProfile]) -> ResolvedTarget<'a> {
    match target {
        Target::Local => ResolvedTarget::Local,
        Target::Remote(id) => match profiles.iter().find(|p| &p.id == id) {
            Some(profile) => ResolvedTarget::Remote(profile),
            None => ResolvedTarget::MissingProfile(id.clone()),
        },
    }
}
```

At the restore site (`workspace/mod.rs:2153`, which currently respawns every
leaf locally), branch on `resolve_target`. `ResolvedTarget::Remote` and
`MissingProfile` both construct a dead pane through a path that does NOT call
`TerminalPane::new`, since that unconditionally attempts a local shell. In this
slice both render as a dead pane; slice 2 gives `Remote` a real connection.

- [ ] **Step 6: Run tests**

Run: `cargo test`
Expected: PASS, including the byte-identical serialization assertion.

- [ ] **Step 7: Commit**

```bash
cargo fmt
git add native/src/hosts.rs native/src/layout.rs native/src/pane.rs native/src/workspace/mod.rs
git commit -m "feat(native): pane targets, persisted without changing saved layouts"
```

---

### Task 8: `PanelTarget` and operation tokens

**Files:**
- Modify: `native/src/hosts.rs` (add `PanelTarget`, `from_pane`, `accepts_completion`)
- Modify: `native/src/files_panel.rs`
- Modify: `native/src/git_panel.rs`
- Test: `native/src/hosts.rs` (inline)

**Interfaces:**
- Consumes: `Target` (Task 7).
- Produces: `hosts::PanelTarget { Local(PathBuf), Remote(String), Detached }`; `PanelTarget::from_pane(target: &Target, cwd: Option<String>, label: &str) -> PanelTarget`; `PanelTarget::local_path(&self) -> Option<&Path>`; `hosts::accepts_completion(current: u64, token: u64) -> bool`; `FilesPanel::set_target(&mut self, target: PanelTarget, cx)`; `GitPanel::set_target(&mut self, target: PanelTarget, cx)`.

**Note on test placement:** `git_panel.rs` and `files_panel.rs` have no test
modules, and this codebase never constructs gpui entities in tests — it tests
pure functions (`core/src/cue.rs`, `native/src/awake.rs`, `native/src/net.rs`,
`workspace/mod.rs:4041`). So the decision logic is extracted into `hosts.rs` and
tested there; the panels become thin consumers of it.

- [ ] **Step 1: Write the failing tests**

Add to `#[cfg(test)] mod tests` in `native/src/hosts.rs`:

```rust
    #[test]
    fn a_local_pane_with_a_cwd_targets_that_path() {
        let t = PanelTarget::from_pane(&Target::Local, Some("/tmp/repo".into()), "");
        assert_eq!(t.local_path(), Some(std::path::Path::new("/tmp/repo")));
    }

    #[test]
    fn a_local_pane_without_a_cwd_detaches() {
        let t = PanelTarget::from_pane(&Target::Local, None, "");
        assert_eq!(t, PanelTarget::Detached);
        assert_eq!(t.local_path(), None);
    }

    #[test]
    fn a_remote_pane_never_yields_a_local_path() {
        // Even if a cwd string is somehow present, a remote pane must not
        // hand the panels a local path to act on.
        let remote = Target::Remote(ProfileId("p1".into()));
        let t = PanelTarget::from_pane(&remote, Some("/tmp/repo".into()), "pc1");
        assert_eq!(t, PanelTarget::Remote("pc1".to_string()));
        assert_eq!(t.local_path(), None, "remote pane leaked a local path");
    }

    #[test]
    fn a_stale_token_is_refused_and_the_current_one_accepted() {
        // git_panel.rs:354 clears `busy` BEFORE checking generation, so a
        // completion from a previous target could unbusy the new one.
        assert!(!accepts_completion(7, 6), "stale token accepted");
        assert!(!accepts_completion(7, 8), "future token accepted");
        assert!(accepts_completion(7, 7));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p superterminal-native hosts`
Expected: FAIL — `PanelTarget` and `accepts_completion` do not exist.

- [ ] **Step 3: Implement the pure logic**

Add to `native/src/hosts.rs`:

```rust
/// What a side panel is pointed at. `Detached` is "nothing focused, or no
/// cwd"; `Remote` is "the focused pane is on another host", which this
/// slice renders as disabled rather than remoting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelTarget {
    Local(std::path::PathBuf),
    Remote(String),
    Detached,
}

impl PanelTarget {
    /// The ONLY way a panel target is derived. A remote pane can never
    /// produce a local path, regardless of what cwd it reports.
    pub fn from_pane(target: &Target, cwd: Option<String>, label: &str) -> PanelTarget {
        match target {
            Target::Remote(_) => PanelTarget::Remote(label.to_string()),
            Target::Local => match cwd {
                Some(cwd) => PanelTarget::Local(cwd.into()),
                None => PanelTarget::Detached,
            },
        }
    }

    pub fn local_path(&self) -> Option<&std::path::Path> {
        match self {
            PanelTarget::Local(path) => Some(path.as_path()),
            _ => None,
        }
    }

    pub fn is_local(&self) -> bool {
        matches!(self, PanelTarget::Local(_))
    }
}

/// Whether an async completion still owns the panel. Checked BEFORE any
/// field is written, `busy` included.
pub fn accepts_completion(current: u64, token: u64) -> bool {
    current == token
}
```

- [ ] **Step 4: Convert the panels**

In each panel, replace `set_root` / `set_target_cwd` with:

```rust
    /// Point the panel at a new target. Unlike the old `Option<cwd>`
    /// setters, which returned early on `None` and left the previous repo
    /// live and actionable, this ALWAYS applies.
    pub fn set_target(&mut self, target: PanelTarget, cx: &mut Context<Self>) {
        if self.target == target {
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        self.busy = false;
        self.clear_for_retarget();   // listings/status/diff/armed actions
        self.target = target;
        cx.notify();
    }
```

Then, at every async completion — including `git_panel.rs:354`, where the
current code clears `busy` first — check the token before touching anything:

```rust
        if !crate::hosts::accepts_completion(self.generation, token) {
            return;
        }
        self.busy = false;
```

Every click / refresh / submit handler re-checks `self.target.is_local()` at
invocation time, not only at render time. `GitPanel`'s armed destructive
actions are cleared by `clear_for_retarget`; an operation already running is
merely disowned, which is all that is possible.

- [ ] **Step 5: Run tests**

Run: `cargo test -p superterminal-native hosts`
Expected: PASS (4 new tests).

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add native/src/hosts.rs native/src/git_panel.rs native/src/files_panel.rs
git commit -m "fix(native): panels detach on target change and ignore stale completions"
```

---

### Task 9: Atomic focus helper across all fourteen sites

**Files:**
- Modify: `native/src/workspace/mod.rs`
- Test: `native/src/workspace/mod.rs` (inline)

**Interfaces:**
- Consumes: Tasks 7 and 8.
- Produces: `Workspace::set_focused_terminal(&mut self, id: Option<String>, cx: &mut Context<Self>)`.

The fourteen assignment sites are lines **1097, 1131, 1147, 1172, 1221, 1236, 1259, 1304, 1361, 1946, 2178, 2270, 2278, 2301**. Six already call `push_git_cwd` nearby (742, 1098, 1132, 1364, 1947, 1987); eight do not, and the three close paths (1236, 1259, 1304) are the ones that matter most — closing a focused local pane can land focus on a remote pane while the panels still hold local authority. They assign across a line break, so a grep for `focused_terminal = ` with a trailing space silently misses them.

- [ ] **Step 1: Write the failing test**

The behavioural half is already covered purely by `PanelTarget::from_pane`
(Task 8). What remains is the STRUCTURAL invariant: that no code path can
assign focus without retargeting. Add to `#[cfg(test)] mod tests` in
`native/src/workspace/mod.rs`:

```rust
    #[test]
    fn only_the_helper_assigns_focus() {
        // Focus and panel identity must change together. The periodic
        // refresh is gated on `sidebar_open && pet_tick_count % 3`, so any
        // raw assignment reintroduces a window where a remote pane is
        // focused while the panels still hold local authority — including
        // the git panel's destructive actions.
        //
        // Note the pattern has no trailing space: three of the fourteen
        // original sites assign across a line break and a trailing-space
        // grep silently misses them (1236, 1259, 1304 — all close paths).
        let source = include_str!("mod.rs");
        let assignments = source.matches("focused_terminal =").count();
        assert_eq!(
            assignments, 1,
            "found {assignments} raw `focused_terminal =` assignments; \
             exactly one may exist, inside set_focused_terminal"
        );
    }

    #[test]
    fn the_helper_retargets_the_panels() {
        // Guards against the helper being reduced to a bare assignment.
        let source = include_str!("mod.rs");
        let helper = source
            .split("fn set_focused_terminal")
            .nth(1)
            .expect("set_focused_terminal must exist");
        let body_end = helper.find("\n    fn ").unwrap_or(helper.len());
        assert!(
            helper[..body_end].contains("retarget_panels"),
            "set_focused_terminal must retarget the panels in the same update"
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p superterminal-native workspace`
Expected: FAIL — `set_focused_terminal` does not exist; the invariant test reports 14.

- [ ] **Step 3: Add the helper**

```rust
    /// The ONLY writer of `focused_terminal`. Focus and panel identity
    /// change together, in one update, before any other UI action can
    /// dispatch — the periodic refresh at `pet_tick_count % 3` is gated on
    /// the sidebar being open and must never be responsible for identity.
    fn set_focused_terminal(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        self.focused_terminal = id;
        self.retarget_panels(cx);
    }

    /// Point both panels (and the viewer) at the focused pane's target.
    fn retarget_panels(&mut self, cx: &mut Context<Self>) {
        let target = self
            .focused_terminal
            .as_ref()
            .and_then(|id| self.panes.get(id))
            .map(|pane| {
                let pane = pane.read(cx);
                match pane.target() {
                    Target::Local => pane
                        .cwd()
                        .map(|cwd| PanelTarget::Local(cwd.into()))
                        .unwrap_or(PanelTarget::Detached),
                    Target::Remote(id) => PanelTarget::Remote(self.profile_label(id)),
                }
            })
            .unwrap_or(PanelTarget::Detached);
        if let Some(panel) = self.git_panel.clone() {
            panel.update(cx, |panel, panel_cx| panel.set_target(target.clone(), panel_cx));
        }
        if let Some(panel) = self.files_panel.clone() {
            panel.update(cx, |panel, panel_cx| panel.set_target(target.clone(), panel_cx));
        }
        if !matches!(target, PanelTarget::Local(_)) {
            self.close_file_viewer(cx);
        }
    }
```

- [ ] **Step 4: Convert all fourteen sites**

Replace each `self.focused_terminal = <expr>;` with `self.set_focused_terminal(<expr>, cx);`, and each `ws.focused_terminal = Some(id);` (2270, 2278, 2301) with `ws.set_focused_terminal(Some(id), cx);`. Sites 1221, 1236, 1259, 1304, 1361 and 2178 assign the result of a `collect_terminal_ids(..).into_iter()` chain — bind it to a local first, then pass it:

```rust
                    let next = collect_terminal_ids(self.tabs[tab_index].active_pane())
                        .into_iter()
                        .next();
                    self.set_focused_terminal(next, cx);
```

Rewrite `push_git_cwd` to delegate to `retarget_panels` so its six existing call sites keep refreshing panel CONTENT on the tick without owning identity.

- [ ] **Step 5: Run tests**

Run: `cargo test`
Expected: PASS. The invariant test now counts exactly one raw assignment (inside the helper).

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add native/src/workspace/mod.rs
git commit -m "fix(native): focus and panel target change atomically at all 14 sites"
```

---

### Task 10: The folder-write guard

**Files:**
- Modify: `native/src/workspace/mod.rs:2056`
- Test: `native/src/workspace/mod.rs` (inline)

**Interfaces:**
- Consumes: Tasks 3, 7, 9.
- Produces: no new API.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn the_folder_picker_refuses_a_remote_pane_even_when_idle() {
        // Two independent guards. This one does not trust the activity
        // signal at all, so a forged remote report cannot defeat it.
        assert!(!may_write_cd(&Target::Remote(ProfileId("p1".into())), Activity::Idle));
        assert!(!may_write_cd(&Target::Remote(ProfileId("p1".into())), Activity::Busy));
        assert!(!may_write_cd(&Target::Remote(ProfileId("p1".into())), Activity::Unknown));
    }

    #[test]
    fn the_folder_picker_requires_a_positively_idle_local_pane() {
        assert!(may_write_cd(&Target::Local, Activity::Idle));
        assert!(!may_write_cd(&Target::Local, Activity::Busy));
        assert!(
            !may_write_cd(&Target::Local, Activity::Unknown),
            "Unknown must never authorise a write"
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p superterminal-native workspace`
Expected: FAIL — `may_write_cd` not found.

- [ ] **Step 3: Implement**

```rust
/// Whether a picked LOCAL path may be typed into this pane.
///
/// Two independent guards. The activity check is the old best-effort race
/// gate, tightened so `Unknown` never authorises. The target check does not
/// depend on the activity signal being honest at all, which is what makes
/// this safe once a remote host can report its own state.
fn may_write_cd(target: &Target, activity: Activity) -> bool {
    target.is_local() && activity.is_idle()
}
```

At `workspace/mod.rs:2056`, replace the match guard:

```rust
                    Some(pane)
                        if pane.read(cx).has_live_shell()
                            && may_write_cd(
                                pane.read(cx).target(),
                                pane.read(cx).foreground_activity(),
                            ) =>
```

In the fall-through arm, a remote focused pane must NOT silently open a new local window at the picked path: leave the directory control disabled and relabelled for remote panes (Task 8 already disables the panels; this is the focused-bar control at `workspace/mod.rs:2551`).

Re-check the guard when the async picker returns, since focus can change while the native dialog is open.

- [ ] **Step 4: Handle the remaining cwd consumers**

Spec D11 lists five `cwd()`-derived consumers. Tasks 8 and 9 cover the panels
and sidebar; two remain, and both must treat a remote pane's `None` cwd as
"no local context" rather than falling back to a stale or home path:

- **Buddy repository probing** (`workspace/mod.rs:797`) — skip the probe
  entirely for a remote pane. Probing the Mac's repo while the user is on
  another host produces confidently wrong buddy notes.
- **Focused-bar cwd display and directory control** (`workspace/mod.rs:2551`) —
  show the host label instead of a path, and disable the directory control.
  This is the fall-through arm referenced in Step 3: a remote focused pane must
  not silently open a new local window at the picked path.

Add to `#[cfg(test)] mod tests` in `native/src/workspace/mod.rs`:

```rust
    #[test]
    fn a_remote_pane_offers_no_local_directory_context() {
        // Buddy probing and the focused-bar control both key off this.
        assert!(!local_context_available(&Target::Remote(ProfileId("p1".into())), None));
        assert!(!local_context_available(
            &Target::Remote(ProfileId("p1".into())),
            Some("/tmp/repo".to_string())
        ));
        assert!(local_context_available(&Target::Local, Some("/tmp/repo".to_string())));
        assert!(!local_context_available(&Target::Local, None));
    }
```

Implement beside `may_write_cd`:

```rust
/// Whether this pane gives the app a usable LOCAL directory: the gate for
/// buddy repo probing and the focused-bar directory control.
fn local_context_available(target: &Target, cwd: Option<String>) -> bool {
    target.is_local() && cwd.is_some()
}
```

- [ ] **Step 5: Run the full suite**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add native/src/workspace/mod.rs
git commit -m "fix(native): never type a local path into a pane that is not local and idle"
```

---

## Done criteria

- `cargo test` green from the repo root (core + native).
- `cargo fmt --check` clean.
- The `every_focus_assignment_goes_through_the_helper` invariant test passes.
- Existing cue, awake, layout, and companion assertions are unchanged in value — only their argument types moved to `Activity`.
- No UI path constructs a `Target::Remote` pane. Remote panes exist only in tests in this slice.
- The Codex gate passes on the full commit range before pushing.
