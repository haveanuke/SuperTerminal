# Peer Instances Phase C2b: The Attached Pane — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A tab on this Mac that shows a terminal running on another Mac — rendering its frames, typing into it, and reporting honestly when the connection goes quiet.

**Architecture:** Everything underneath exists. The peer client streams frames and tracks freshness; the server shares sessions and scopes them per peer; `Target::Remote` and `Origin::Attached` already exist on panes and are already excluded from re-broadcast and from local keystroke fan-out. This phase connects the client to a pane.

**Tech Stack:** Rust 2021, gpui `=0.2.2`, alacritty_terminal `=0.26.0`. No new crate.

**Spec:** `docs/superpowers/specs/2026-08-31-peer-instances-design.md` — D1 (raw byte input), D2 (broadcaster-owned geometry), D5 (the degraded contract), D3e (the `spawn_pane` invariant this phase must discharge).

## Global Constraints

- `gpui` and `alacritty_terminal` are pinned with `=` and must never be forked or patched.
- Hand-rolled `extern "C"` FFI, never the `libc` crate.
- No emoji in rendered UI — use `native/src/icons.rs`.
- No attribution trailers in commit messages.
- `cargo fmt` before every commit; `cargo test` from the repo root. Baseline: **593 passing, 0 failing.**
- **No gpui test harness exists and none may be introduced.** Rendering is entity-bound; extract every decision into a pure function and test that. Say plainly which paths are verified by reading.
- **Local panes must be byte-identical.** A local terminal must not change in any way. If a pre-existing test's expected value would have to change, STOP and report.

## What the code already gives us, and one thing it costs

`TerminalPane` holds `snapshot: RenderableSnapshot` as a FIELD, and the renderer draws from that rather than from the session. So the path is already `session → snapshot → render`, and an attached pane simply replaces the first arrow. That is a far smaller change than "add a rendering mode".

**The cost, which is user-visible and must be decided rather than discovered.** `serialize_snapshot(&snapshot, theme)` resolves theme colours SERVER-SIDE into `"#rrggbb"` strings (`wire.rs:159`, `WireRun.fg`/`bg`). An attached pane therefore renders the BROADCASTER's theme, not the viewer's. If the work Mac runs a light theme and the personal Mac runs dark, that pane will look light sitting beside dark local panes.

This plan ACCEPTS that for now:
- It matches what the phone already does, so it is consistent rather than novel.
- "You see what they see" is defensible, and arguably correct for a shared terminal.
- The alternative — sending semantic colour slots instead of resolved hex — is a wire change affecting the phone, and re-mapping hex on receipt is impossible in general because the hex has already lost whether `#ff0000` was "ANSI red" or a literal truecolor.

Task 1 must render this legibly rather than letting it look like a bug. If it looks wrong in practice, the wire change is a later decision with its own design.

---

### Task 1: Render a received snapshot

**Files:**
- Modify: `native/src/pane.rs`
- Test: pure conversion tests, inline

**Interfaces:**
- Produces: a conversion from `WireSnapshot` into whatever the renderer consumes, and a pane that draws from it when its target is remote.

`WireSnapshot` carries merged runs with resolved colours; `RenderableSnapshot` carries per-cell styles. Choose ONE and justify it in your report:
- convert `WireSnapshot` → `RenderableSnapshot` so the existing renderer is untouched, or
- give the renderer a second path that draws `WireSnapshot` runs directly, which are arguably easier to draw since they are already merged and resolved.

Whichever you pick, **the local path must not change**. Prove it: every existing pane test passes untouched.

The conversion (or the run-drawing) is pure and must be tested exhaustively: an empty grid; a row of plain cells; runs carrying bold/italic/underline; a run with `bg: None` meaning the page background shows through; the cursor present and absent; history rows present and absent.

- [ ] **Step 1: Write the failing tests**
- [ ] **Step 2: Run them, observe the failure**
- [ ] **Step 3: Implement**
- [ ] **Step 4: `cargo test` from the repo root; every pre-existing pane test unchanged**
- [ ] **Step 5: Commit** — `feat(native): render a snapshot received from a peer`

---

### Task 2: Every session-dependent accessor answers correctly without one

**Files:**
- Modify: `native/src/pane.rs`
- Test: pure predicates, inline

`pane.rs` references `self.session` at sixteen sites. An attached pane has NO session — no PTY, no local process. Each site must answer correctly rather than incidentally.

Go through all sixteen and classify each in your report:
- **correct as-is** because `Option` already yields the right answer for a remote pane;
- **needs a target-aware answer** because the `None` result would be wrong or misleading;
- **should be unreachable** for a remote pane, and is structurally prevented from being called.

Ones known to matter, from the design:
- `cwd()` must be `None` — slice 1 established this and the panels depend on it.
- `has_live_shell()` currently means "the local PTY child has not exited". For an attached pane there is no child; a name that says "local PTY alive" would stop it being read as "the remote shell is alive". Rename or make target-aware, and say which.
- `foreground_busy` / `foreground_activity` / `companion_activity` / `status_activity` must not report a local process's state for a remote pane.
- `input_sender()` — an attached pane forwards keystrokes to its peer. Whatever it returns must not make the pane eligible for anything that treats a sender as proof of a local PTY. `Origin` and the local-fan-out predicate already guard the two known consumers; verify nothing else infers from it.

**This is the phase's highest-risk task**, because a site that answers plausibly-but-wrongly will not fail a test — it will quietly mislead a consumer. Enumerate all sixteen; do not stop at the ones listed here.

- [ ] **Step 1: Enumerate and classify all sixteen sites IN THE REPORT before changing any**
- [ ] **Step 2: Write failing tests for the predicates you extract**
- [ ] **Step 3: Implement**
- [ ] **Step 4: `cargo test`; local behaviour unchanged**
- [ ] **Step 5: Commit** — `feat(native): pane accessors answer honestly without a local session`

---

### Task 3: Typing into a remote terminal

**Files:**
- Modify: `native/src/pane.rs`

The pane already calls `keys::key_to_bytes(&input, self.snapshot.app_cursor_mode, true)` at `pane.rs:906` and `:918` and writes the bytes to its PTY. For an attached pane the SAME encoder runs — that is spec D1, and the reason the peer endpoint takes raw bytes rather than a symbolic vocabulary — but the bytes go to `Attachment::send` instead.

`app_cursor_mode` must come from the RECEIVED snapshot, not a local default, or arrow keys will be wrong in exactly the applications where it matters.

**`send()` blocks up to its deadline.** The gpui render/event path must not call it inline: a peer that stalls would freeze the app. Hand it to the attachment's thread or a queue and say in your report which.

Test the encode-and-route decision as a pure function. The gpui event plumbing is entity-bound; state that.

- [ ] **Step 1: Write the failing tests**
- [ ] **Step 2: Run them, observe the failure**
- [ ] **Step 3: Implement**
- [ ] **Step 4: `cargo test`**
- [ ] **Step 5: Commit** — `feat(native): type into an attached remote terminal`

---

### Task 4: Activity, and the stale-wins rule

**Files:**
- Modify: `native/src/pane.rs`, `native/src/workspace/`
- Create: a per-peer session-list poller

**Interfaces:**
- Produces: one poller per PEER (not per attachment) fetching `/sessions`, and a combining function.

`Attachment` reports freshness; `/sessions` reports activity. Neither alone is the pane's activity.

**The rule, already written into `attach.rs` for this phase to inherit: stale attachment wins.** If frames have stopped, the pane reports `Activity::Unknown` regardless of what the last session list said — a cached "busy" from thirty seconds ago is exactly the stale signal `Unknown` exists to represent.

One poller per peer, not per attachment: polling `/sessions` per attachment multiplies requests by the number of open panes for data identical across all of them.

The combining function is pure — test it exhaustively over freshness × reported activity × poll-failed.

- [ ] **Step 1: Write the failing tests**
- [ ] **Step 2: Run them, observe the failure**
- [ ] **Step 3: Implement**
- [ ] **Step 4: `cargo test`**
- [ ] **Step 5: Commit** — `feat(native): an attached pane reports Unknown when its stream goes quiet`

---

### Task 5: Opening one, and discharging D3e

**Files:**
- Modify: `native/src/workspace/`
- Test: pure predicates

Attaching: pick a peer, see what it has shared, open one as a pane. The pane is constructed with `Target::Remote(PeerId)` and `Origin::Attached`.

**D3e, which this phase owes.** The spec records that `BroadcastMap::share` is ungated and safe ONLY because `spawn_pane` structurally cannot produce a non-local pane — an invariant nothing encodes. This phase is the one most likely to give `spawn_pane` a target. Either encode the invariant where it is relied upon, or give `BroadcastMap` the origin fact, having designed how it stays in step. Do not leave it true by coincidence once a second shape of pane exists.

Geometry is broadcaster-owned (D2): the attached pane fits and scrolls to what it is given and does NOT resize the remote PTY. Say so in the UI rather than letting a resize silently do nothing.

The degraded contract (D5) must be visible, not discovered: 150 rows of scrollback, no selection, no search, no mouse reporting.

- [ ] **Step 1: Write the failing tests**
- [ ] **Step 2: Run them, observe the failure**
- [ ] **Step 3: Implement**
- [ ] **Step 4: `cargo test`**
- [ ] **Step 5: Commit** — `feat(native): open a shared peer terminal as a pane`

---

## Done criteria

- `cargo test` green from the repo root; `cargo fmt --check` clean; no new warnings.
- **Every pre-existing test passes with expected values unchanged** — local panes are untouched.
- An attached pane never reports a local process's cwd, activity, or liveness.
- Typing reaches the remote terminal, encoded by the same `key_to_bytes` a local pane uses, with `app_cursor_mode` taken from the received snapshot.
- A quiet stream produces `Activity::Unknown`, never `Idle`.
- D3e is discharged: the `spawn_pane` invariant is encoded, not coincidental.
- The Codex gate passes on the full commit range before pushing.
