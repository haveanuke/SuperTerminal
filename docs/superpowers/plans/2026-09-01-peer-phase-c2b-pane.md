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

## What the code gives us, and three things a review corrected

`TerminalPane` holds `snapshot: RenderableSnapshot` as a field and the renderer
draws from it — but the decoupling is only PARTIAL, and an earlier draft of this
plan overstated it. The render path still assumes native semantic cells: it
resolves `CellColor` through the VIEWER's `Theme`, applies local selection and
search state, computes wide spacers, and coalesces runs. So neither "convert
`WireSnapshot` to `RenderableSnapshot`" nor "draw wire runs in a separate
renderer" is right — the first feeds semantically-wrong data into theme
resolution, the second duplicates glyph measurement and pinned-run behaviour.

**Three corrections, each of which changes a task:**

1. **The background is not on the wire, and that is worse than a theme
   mismatch.** `WireRun.fg` and an explicit `bg` are resolved hex, but
   `bg: None` means "page background shows through" (`wire.rs:52`), and
   `WireSnapshot` carries no background (`wire.rs:23`). The phone gets away with
   this because `page.html` supplies its own static `--bg`. A native attached
   pane would render BROADCASTER FOREGROUND over VIEWER BACKGROUND — light-grey
   text on a light background is not a cosmetic mismatch, it is unreadable.
   An earlier draft accepted the theme difference; that acceptance was based on
   a wrong model of what the wire carries. Task 1 fixes it.

2. **Scrollback would silently render as nothing.** The renderer draws only
   `snapshot.rows`. `RenderableSnapshot.history_rows` exists for PUBLISHING to
   the companion, not for native painting. A conversion that puts `wire.history`
   into `history_rows` moves the data and paints none of it, so D5's 150 rows of
   scrollback would appear to work and show nothing. Task 3 builds a view model.

3. **`/sessions` polling blocks too**, not just `send()`. `peer_client::get` is
   a blocking round trip; an earlier draft only warned about `send`. BOTH must
   be off the gpui event and render path, or a stalled peer freezes the app.

---

### Task 1: Carry the broadcaster's background on the wire

**Files:**
- Modify: `native/src/companion/wire.rs`
- Test: inline

`WireRun.bg: None` means "the page background shows through", but the snapshot
never says what that background IS. The phone substitutes its own. A native pane
cannot, and rendering the broadcaster's foreground over the viewer's background
can be unreadable rather than merely inconsistent.

Add the resolved background to `WireSnapshot` — the same `"#rrggbb"` treatment
its foregrounds already get. It is ALWAYS serialized: an absent background and a
default one must not be ambiguous, exactly as `bracketedPaste` is always sent.

**This touches the wire the phone reads, so it is additive only.** The phone is
safe by inspection: `page.html` does `JSON.parse` and reads only known keys
(`snap.cols`, `snap.rows`, `snap.history`, `snap.cursor`), so an unknown
top-level field cannot break it. `page.html` needs no change and must not get
one — verify no existing wire assertion moves. If a pre-existing test would have
to change, STOP and report.

**But a required field breaks NATIVE peers across versions, and that must be
gated.** `WireSnapshot` derives `Deserialize` for the peer client. If
`background` is required and always serialized, a NEW client attached to an
OLDER broadcaster fails to parse EVERY frame — failing loudly, but in the worst
possible place: mid-stream, repeatedly, with no explanation the user can act on.

So this task also adds the protocol gate the design already calls for. Before
attaching, the client checks the peer's `/version` (which already advertises
`protocol` and `capabilities`) and REFUSES an incompatible peer up front with a
reason the UI can show, rather than connecting and failing per frame.

The exact values, so this is auditable rather than interpreted: `/version`
(`server.rs:515`) currently advertises `"protocol": 1` and
`"capabilities": ["principals", "origin", "peer-input"]`. This is a breaking
change for native peers, so **`protocol` becomes `2`** and
**`"snapshot-background"` joins the capability list**. Introduce both as named
constants rather than repeating literals at the check site and the serve site —
a hardcoded `2` in two files is how the next bump goes wrong.

Note the failure this prevents is specifically NOT the generic parse error:
without the gate, a wire-shape mismatch collapses into
`BadResponse("frame was not a valid snapshot")` (`stream.rs:120`), repeated per
frame, which tells the user nothing they can act on.

Tests, both halves:
- Background: the field is present and always serialized; it round-trips; it
  carries the theme's ACTUAL background rather than a hardcoded value.
- Protocol gate: a peer advertising protocol 2 with `snapshot-background` is
  accepted; one advertising protocol 1 is refused BEFORE any stream is opened,
  with a reason distinguishable from a parse failure; the refusal surfaces as a
  status the UI can show.

- [ ] **Step 1: failing tests** — [ ] **2: observe failure** — [ ] **3: implement** — [ ] **4: `cargo test`, phone untouched** — [ ] **5: commit** `feat(companion): the wire snapshot carries its background`

---

### Task 2: One resolved-paint-rows adapter, fed by both sources

**Files:**
- Modify: `native/src/pane.rs`
- Test: pure, inline

Extract the point where the renderer stops needing semantics: a pure adapter
producing RESOLVED paint runs — colours already `#rrggbb`, runs already merged.
A LOCAL snapshot resolves through the viewer's theme and runs the existing
coalescer to get there. A WIRE snapshot is ALREADY in that shape and becomes
paint runs directly, using the background from Task 1 wherever `bg: None`.

**One thing cannot be shared, and the plan previously claimed it could.** The
local renderer's PINNED-RUN behaviour does per-glyph advance checks against the
viewer's font (`pane.rs:982`). The wire has already coalesced cells into
`WireRun { col, width, text }` (`wire.rs:116`), so once `a世b` is one string plus
a total width, the receiver cannot tell which glyph consumed the extra cell.
That information is gone and no adapter can recover it.

Therefore: attached panes draw wire runs AS GIVEN, and per-glyph pinning is a
LOCAL-ONLY refinement. Do not attempt to reconstruct it by guessing glyph
widths — a wrong guess misplaces every character after it on the row.

**This is a new entry in D5's degraded contract and must be recorded there:**
wide-character alignment on an attached pane may differ subtly from the same
terminal viewed locally. Add it to the spec's degraded list alongside the
150-row scrollback and the absence of selection and search, so it is a stated
limit rather than a rendering bug someone chases later.

**The local path must be byte-identical.** Prove it: every pre-existing pane test
passes untouched, and the adapter for a local snapshot produces what the renderer
consumed before.

Test the adapter exhaustively over both sources: empty grid; plain cells; bold,
italic, underline; `bg: None` picking up the background; cursor present and
absent; a wide character and its spacer.

- [ ] **Step 1: failing tests** — [ ] **2: observe failure** — [ ] **3: implement** — [ ] **4: `cargo test`** — [ ] **5: commit** `feat(native): one paint-row adapter for local and received snapshots`

---

### Task 3: An attached view model with real scrollback

**Files:**
- Modify: `native/src/pane.rs`
- Test: pure, inline

The renderer paints `rows` only. D5 promises 150 rows of scrollback on an
attached pane, and simply storing `wire.history` somewhere would deliver none of
it.

Build a pure view model for attached panes: given `wire.history + wire.rows` and
a scroll offset, produce the visible rows to paint. Scrolling is LOCAL to the
viewer — it must not resize or scroll the remote PTY, per D2's broadcaster-owned
geometry.

**The window MOVES, and the contract must say so.** The broadcaster builds its
history tail relative to its live screen, capped at `HISTORY_TAIL = 150`
(`term_session.rs:938`). The wire carries no row identity, so a viewer CANNOT
hold a stable anchor on a specific historical row once it ages out of that tail.
The honest contract is "stays scrolled back BY OFFSET", not "keeps showing the
same row forever" — test the former and do not write a test asserting the latter.

Test: offset zero shows the live rows; scrolling back reaches into history;
scrolling past the oldest row clamps rather than panicking; a snapshot with no
history behaves like a plain grid; a new frame arriving while scrolled back does
not silently yank the view to the bottom (decide the behaviour, state it, test
it); and rows ageing out from under a scrolled-back viewer shifts what is shown
without panicking or clamping to the wrong end.

- [ ] **Step 1: failing tests** — [ ] **2: observe failure** — [ ] **3: implement** — [ ] **4: `cargo test`** — [ ] **5: commit** `feat(native): scrollback for an attached pane`

---

### Task 4: Every session-dependent accessor answers correctly without one

**Files:**
- Modify: `native/src/pane.rs`
- Test: pure predicates, inline

`pane.rs` references `self.session` at sixteen sites. An attached pane has NO session — no PTY, no local process. Each site must answer correctly rather than incidentally.

Go through all sixteen and classify each in your report:
- **correct as-is** because `Option` already yields the right answer for a remote pane;
- **needs a target-aware answer** because the `None` result would be wrong or misleading;
- **should be unreachable** for a remote pane, and is structurally prevented from being called.

A review has already classified them; verify each rather than trusting this list.
**The list below has already been corrected once, and the correction is
instructive:** it cited line 408 as a `self.session` site, but 408 contains no
such reference — the real sites are the sixteen below. That phantom entry had
displaced a REAL one, `has_live_shell()` (496), which went unclassified. Verify
by grep, not by trust.

The verified sixteen, with their enclosing functions: `cwd()` 481,
`has_live_shell()` 496, `foreground_busy()` 501, `foreground_activity()` 513,
`companion_busy()` 526, `status_activity()` 549, `input_sender()` 567,
`shutdown()` 578, `process_events()` 582, `write_self()` 627, `set_search()` 633,
`search_next()` 643, `scroll_to_bottom_on_input()` 932, `render()` 1193,
`resize_to()` 1577, `handle_click()` 1648.

- **Correct as-is:** `cwd()` (481), `process_events()` (582), the render-path
  local sync (1193), and
  `resize_to()` (1577) — the last ONLY if the UI states that geometry is
  broadcaster-owned rather than letting a resize silently do nothing.
- **Needs a target/attachment-aware answer:** `foreground_activity()` (513),
  `status_activity()` (549), `shutdown()` (578), `write_self()` (627),
  probably `scroll_to_bottom_on_input()` (932), and **`has_live_shell()` (496)**.
  That last one is the site the phantom hid, and it is not cosmetic: it gates the
  folder picker's `cd` at `workspace/mod.rs:2270`. Left as `None -> false`, a user
  picks a folder for an attached pane and NOTHING HAPPENS, with no explanation —
  the same silent-no-op failure this task exists to prevent. A remote pane's shell
  liveness is the ATTACHMENT's liveness, not the absence of a local session.
- **Plausible-but-wrong if left to return `None -> false/idle`:**
  `foreground_busy()` (501) through `companion_busy()`/`companion_activity()`
  (526/541). A remote pane would look IDLE rather than unknown or peer-reported —
  the precise failure the tri-state exists to prevent.
- **Must stay local-only, NOT generalised:** `input_sender()` (567). Its concrete
  `EventLoopSender` type and its consumers (`companion_ui.rs:79`,
  `workspace/mod.rs:1166`) mean returning a fake or remote sender would be a bad
  seam. Remote input goes through the attachment queue instead (Task 5).
- **Structurally unreachable ONLY IF the UI gates them:** `set_search()` (633),
  `search_next()` (643), and selection start (1648). Today the search overlay and
  click-drag can reach the focused pane without target gating (see
  `workspace/mod.rs:3579`). Ungated, these are plausible-but-wrong no-op UI —
  the user searches an attached pane and nothing happens, with no explanation.
  Gating them is Task 7's job; name the requirement here so it is not lost.

**This is the phase's highest-risk task**, because a site that answers plausibly-but-wrongly will not fail a test — it will quietly mislead a consumer. Enumerate all sixteen; do not stop at the ones listed here.

- [ ] **Step 1: Enumerate and classify all sixteen sites IN THE REPORT before changing any**
- [ ] **Step 2: Write failing tests for the predicates you extract**
- [ ] **Step 3: Implement**
- [ ] **Step 4: `cargo test`; local behaviour unchanged**
- [ ] **Step 5: Commit** — `feat(native): pane accessors answer honestly without a local session`

---

### Task 5: Typing into a remote terminal

**Files:**
- Modify: `native/src/pane.rs`

The pane already calls `keys::key_to_bytes(&input, self.snapshot.app_cursor_mode, true)` at `pane.rs:906` and `:918` and writes the bytes to its PTY. For an attached pane the SAME encoder runs — that is spec D1, and the reason the peer endpoint takes raw bytes rather than a symbolic vocabulary — but the bytes go to `Attachment::send` instead.

`app_cursor_mode` must come from the RECEIVED snapshot, not a local default, or arrow keys will be wrong in exactly the applications where it matters.

**`send()` blocks up to its deadline — and so does the `/sessions` poller.**
`Attachment::send` performs a blocking one-shot round trip with a 5s deadline
(`attach.rs:192`), and `peer_client::get` is equally blocking. NEITHER may run
inline on the gpui path — not from key handling, not from paste, not from IME,
not from click-to-move, and not from render. A stalled peer would freeze the
whole app, including every local pane.

Enumerate every call site this phase adds that could reach a blocking peer call
from a gpui handler, and say in your report how each is moved off the path
(the attachment's own thread, a queue, or a dedicated poller thread).

Test the encode-and-route decision as a pure function. The gpui event plumbing is entity-bound; state that.

- [ ] **Step 1: Write the failing tests**
- [ ] **Step 2: Run them, observe the failure**
- [ ] **Step 3: Implement**
- [ ] **Step 4: `cargo test`**
- [ ] **Step 5: Commit** — `feat(native): type into an attached remote terminal`

---

### Task 6: Activity, and the stale-wins rule

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

### Task 7: Opening one, and discharging D3e

**Files:**
- Modify: `native/src/workspace/`
- Test: pure predicates

Attaching: pick a peer, see what it has shared, open one as a pane. The pane is constructed with `Target::Remote(PeerId)` and `Origin::Attached`.

**D3e, which this phase owes, at a named site.** `BroadcastMap::share` is
ungated and safe ONLY because `spawn_pane` structurally cannot produce a
non-local pane — an invariant nothing encodes. The concrete writer is
`workspace/mod.rs:755`, which calls `hub.set_visible_to()` and
`self.broadcasts.share()` immediately after `spawn_pane`. If this phase gives
`spawn_pane` any target flexibility, that call site must assert or type local
origin BEFORE sharing. Do not leave it true by coincidence once a second shape of
pane exists.

**Gate search and selection on attached panes** (carried from Task 4). The search
overlay and click-drag currently reach the focused pane without target gating
(`workspace/mod.rs:3579`). An attached pane must either support them honestly or
refuse them visibly — a search box that silently does nothing is worse than one
that is disabled with a reason.

Geometry is broadcaster-owned (D2): the attached pane fits and scrolls to what it is given and does NOT resize the remote PTY. Say so in the UI rather than letting a resize silently do nothing.

The degraded contract (D5) must be visible, not discovered: 150 rows of scrollback, no selection, no search, no mouse reporting.

- [ ] **Step 1: Write the failing tests**
- [ ] **Step 2: Run them, observe the failure**
- [ ] **Step 3: Implement**
- [ ] **Step 4: `cargo test`**
- [ ] **Step 5: Commit** — `feat(native): open a shared peer terminal as a pane`

---

## The seams this seven-task split creates

Every serious defect in this project's recent phases lived in the seam BETWEEN
two individually-correct tasks. A review named these; each one gets checked
deliberately at the whole-branch stage rather than assumed away:

- **1 to 2** — the background field exists but the adapter mishandles a missing
  or old-version frame.
- **2 to 3** — paint runs are right but scrollback composition shifts the cursor
  row or the live/history boundary.
- **3 to 5** — local scroll offset and "scroll to bottom on input" fight.
- **4 to 6** — accessor truthfulness needs freshness AND polled activity landing
  in the same pane state.
- **5 to 6** — input-queue health and stream freshness disagree; stale must
  still win.
- **4 to 7** — sessionless no-ops become visible UI bugs unless the workspace
  gates them everywhere, not just where Task 7 looked.
- **7 to D3e** — opening attached panes near `spawn_pane` can share a non-local
  pane if the invariant is not encoded at the writer.
- **Cross-cutting** — local rendering, remote rendering, input, selection,
  resize and activity all mutate the same entity, so "local panes byte-identical"
  is an integration risk, not a slogan.

## Done criteria

- `cargo test` green from the repo root; `cargo fmt --check` clean; no new warnings.
- **Every pre-existing test passes with expected values unchanged** — local panes are untouched.
- An attached pane never reports a local process's cwd, activity, or liveness.
- Typing reaches the remote terminal, encoded by the same `key_to_bytes` a local pane uses, with `app_cursor_mode` taken from the received snapshot.
- A quiet stream produces `Activity::Unknown`, never `Idle`.
- D3e is discharged: the `spawn_pane` invariant is encoded, not coincidental.
- The Codex gate passes on the full commit range before pushing.
