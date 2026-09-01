# Peer Instances Phase C1: Broadcast and the Deferred Obligations — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user share a specific local terminal with a specific paired peer, and discharge the two obligations Phase B's reviews deferred to Phase C — per-frame authorization on live streams, and broadcast state surviving a companion restart.

**Architecture:** Server-side only. Phase C2 builds the client that attaches to a shared terminal; this phase makes sharing real, correct, and revocable first. Nothing here renders a remote terminal.

**Tech Stack:** Rust 2021, gpui `=0.2.2`, alacritty_terminal `=0.26.0`, serde/serde_json.

**Spec:** `docs/superpowers/specs/2026-08-31-peer-instances-design.md` — in particular D3c, D3d, D4, and the Phase C note under D4.

## Global Constraints

- `gpui` and `alacritty_terminal` are pinned with `=` and must never be forked or patched.
- Hand-rolled `extern "C"` FFI, never the `libc` crate.
- No emoji in rendered UI — use the SVG set in `native/src/icons.rs`.
- No attribution trailers in commit messages.
- `cargo fmt` before every commit; `cargo test` from the repo root. Baseline: **546 passing, 0 failing.**
- No gpui test harness exists and none may be introduced. Extract decisions into pure functions and test those.
- **The phone must stay byte-identical.** Every pre-existing companion test passes with expected values unchanged.
- No new crate.

## A design decision this plan makes, and why

D3d says a companion restart "must not silently reset every peer's `visible_to` state". The obvious mechanism — persist a `terminal_id -> [PeerId]` map — **does not work**, because terminal ids are not stable across app restarts: `load_session` (`workspace/mod.rs:2322`) deliberately assigns fresh ids per leaf "so pane entities and session files never collide".

But that is not the failure D3d names. The failure is *within one app run*: editing a single peer's grants forces a companion restart (Phase B's revocation mechanism), the restart builds a fresh `Hub`, and every session's `visible_to` is silently emptied — so narrowing WORK's grants would un-share everything with the personal Mac too.

Terminal ids ARE stable within an app run. So broadcast state lives in the `Workspace`, which outlives the companion, and is re-applied to the hub whenever the companion starts.

Across a full app restart broadcast resets. That is acceptable and arguably correct — re-sharing after a relaunch is a deliberate act — **provided it is not silent**. Task 4 must make the current broadcast state visible in the UI, so an empty state after relaunch reads as "nothing shared" rather than as a bug.

---

### Task 1: Broadcast state outlives the companion

**Files:**
- Modify: `native/src/workspace/mod.rs`
- Modify: `native/src/workspace/companion_ui.rs`
- Create: pure helper in `native/src/peers.rs`
- Test: `native/src/peers.rs` (inline)

**Interfaces:**
- Produces: `peers::BroadcastMap` — a `HashMap<String, HashSet<PeerId>>` newtype with `share(id, peer)`, `unshare(id, peer)`, `peers_for(id)`, `prune_to(live_ids)`; `Workspace.broadcasts: BroadcastMap`; re-application on companion start.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn sharing_is_per_terminal_and_per_peer() {
        let mut map = BroadcastMap::default();
        let p1 = PeerId("p1".into());
        let p2 = PeerId("p2".into());
        map.share("t1", &p1);
        assert_eq!(map.peers_for("t1"), vec![p1.clone()]);
        assert!(map.peers_for("t2").is_empty(), "sharing leaked to another terminal");
        map.share("t1", &p2);
        assert_eq!(map.peers_for("t1").len(), 2);
        map.unshare("t1", &p1);
        assert_eq!(map.peers_for("t1"), vec![p2]);
    }

    #[test]
    fn nothing_is_shared_by_default() {
        let map = BroadcastMap::default();
        assert!(map.peers_for("t1").is_empty());
    }

    #[test]
    fn pruning_drops_terminals_that_no_longer_exist() {
        // A closed terminal's id must not linger and be re-shared if a future
        // id ever collides with it.
        let mut map = BroadcastMap::default();
        let p1 = PeerId("p1".into());
        map.share("gone", &p1);
        map.share("alive", &p1);
        map.prune_to(&["alive".to_string()]);
        assert!(map.peers_for("gone").is_empty());
        assert_eq!(map.peers_for("alive"), vec![p1]);
    }

    #[test]
    fn unsharing_a_terminal_never_shared_is_a_no_op() {
        let mut map = BroadcastMap::default();
        map.unshare("t1", &PeerId("p1".into()));
        assert!(map.peers_for("t1").is_empty());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p superterminal-native peers`
Expected: FAIL — `BroadcastMap` does not exist.

- [ ] **Step 3: Implement `BroadcastMap`**

A newtype over `HashMap<String, HashSet<PeerId>>`. `peers_for` returns a deterministically ordered `Vec` (sort by the inner string) so tests and UI are stable — a `HashSet` iteration order would make both flaky.

- [ ] **Step 4: Hold it on the Workspace and re-apply on companion start**

Add `broadcasts: BroadcastMap` to `Workspace`. In `companion_ui.rs`, immediately after the hub is created and panes are registered, replay the map: for every `(terminal_id, peers)` still present in `self.panes`, call `hub.set_visible_to(id, &peer, true)`.

Order matters: registration must happen BEFORE replay, or `set_visible_to` finds no entry and silently does nothing. Verify that ordering in the code rather than assuming it.

- [ ] **Step 5: Prune on terminal close**

Where a terminal is removed, prune its entry. A stale id whose slot is later reused would silently re-share a new terminal with an old peer.

- [ ] **Step 6: Run the full suite**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
cargo fmt
git add native/src/peers.rs native/src/workspace/mod.rs native/src/workspace/companion_ui.rs
git commit -m "feat(native): broadcast state outlives the companion restart"
```

---

### Task 2: Live streams re-check authorization per frame

**Files:**
- Modify: `native/src/companion/server.rs`
- Test: `native/src/companion/server.rs` or `e2e_tests.rs`

**Interfaces:**
- Consumes: `hub::may_touch`.

Spec D3c. `serve_stream` (`server.rs:743`) authorizes at CONNECT time and then loops on `cancel` and `revision` only. Today that is safe solely because every revocation path restarts the whole server — but this phase adds a per-session broadcast toggle, and un-sharing a session must cut a live stream that is already open. Otherwise a peer keeps receiving frames from a terminal you just stopped sharing.

- [ ] **Step 1: Write the failing test**

The property: a stream that was authorized at connect stops delivering once `may_touch` becomes false. Drive it at whatever level you can genuinely test — if the full SSE loop is impractical, extract the per-iteration decision into a pure function (`fn may_continue_stream(cancelled: bool, still_permitted: bool) -> bool`) and test that exhaustively, then call it in the loop. State plainly in your report which you did.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p superterminal-native companion`
Expected: FAIL.

- [ ] **Step 3: Implement**

Re-check `may_touch(&principal, &id)` each iteration alongside the existing `cancel` check, and end the stream when it goes false. The principal is already resolved at connect — keep it for the life of the connection rather than re-resolving, since re-resolution would make a revoked peer's stream die for the right reason but by a different mechanism, and that is the mechanism Phase B already covers.

Ending mid-stream should look to the client like a normal end of stream. Do not invent a new error frame.

- [ ] **Step 4: Run the full suite**

Run: `cargo test`
Expected: PASS. **The phone streams through this same loop** — verify every existing SSE test passes untouched, and that `Principal::Phone` is never denied by the new check.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add native/src/companion/server.rs
git commit -m "feat(companion): live streams re-check authorization every frame"
```

---

### Task 3: `BroadcastHub` encodes origin rather than inferring it

**Files:**
- Modify: `native/src/pane.rs`
- Test: `native/src/pane.rs` or a pure helper

Spec D4's Phase C note. `BroadcastHub` is the LOCAL keystroke fan-out — type in one pane, every member receives it. Membership is currently inferred from a pane having a session (`pane.rs:407`). An attached pane (Phase C2) will also have a sender, because it forwards keystrokes to its peer.

If that inference survives, enabling local broadcast would fan your keystrokes into a terminal on another machine. Encode origin now, while no attached pane exists to be swept in — the same reasoning that put `Origin` on the companion hub in Phase A before attached panes existed.

- [ ] **Step 1: Write the failing test**

Extract the decision — "may this pane join the local fan-out?" — into a pure function taking the pane's target, and test that a `Target::Local` pane may and a `Target::Remote` pane may not.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p superterminal-native pane`
Expected: FAIL.

- [ ] **Step 3: Implement**

Gate `broadcast_register` on that predicate. A remote-target pane never becomes a member, regardless of whether it has a sender.

- [ ] **Step 4: Run the full suite**

Run: `cargo test`
Expected: PASS — local broadcast behaviour is unchanged for local panes.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add native/src/pane.rs
git commit -m "feat(native): local keystroke fan-out excludes remote-target panes by construction"
```

---

### Task 4: The share affordance

**Files:**
- Modify: `native/src/workspace/mod.rs` or the tab-strip render site
- Modify: `native/src/peers.rs` (pure decision logic)
- Test: `native/src/peers.rs`

**Interfaces:**
- Produces: `peers::shareable_peers(peers: &[PeerRecord]) -> Vec<&PeerRecord>` — peers that can meaningfully receive a share.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn only_peers_that_may_view_are_offered_a_share() {
        // Sharing with a peer that cannot view is a no-op the user would have
        // to debug. Offer only peers whose grants let them actually see it.
        let can = PeerRecord { grants: Grants { view: true, ..Default::default() }, ..sample("a") };
        let cannot = PeerRecord { grants: Grants::default(), ..sample("b") };
        let all = vec![can.clone(), cannot];
        let offered: Vec<&str> = shareable_peers(&all).iter().map(|p| p.label.as_str()).collect();
        assert_eq!(offered, vec!["a"]);
    }

    #[test]
    fn no_peers_means_nothing_to_offer() {
        assert!(shareable_peers(&[]).is_empty());
    }
```

Write `sample(label)` as a small local helper building a valid `PeerRecord`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p superterminal-native peers`
Expected: FAIL.

- [ ] **Step 3: Implement the predicate and the UI**

A per-terminal control listing shareable peers with a toggle each, reflecting `BroadcastMap` state. Toggling calls `share`/`unshare` AND `hub.set_visible_to` so the change is live immediately — do not rely on the next companion restart.

**The current state must be visible.** After an app relaunch the map is empty by design; the UI must show that as "not shared" so it reads as a fact rather than a bug.

No emoji — `icons.rs` or the existing text-glyph conventions.

- [ ] **Step 4: Run the full suite**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add native/src/peers.rs native/src/workspace/
git commit -m "feat(native): share a terminal with a paired peer"
```

---

## Done criteria

- `cargo test` green from the repo root; `cargo fmt --check` clean; no new warnings.
- Every pre-existing companion test passes with expected values unchanged.
- Un-sharing a terminal cuts an already-open stream — D3c discharged.
- A companion restart caused by a peer edit does not un-share anything else — D3d discharged.
- A remote-target pane can never join the local keystroke fan-out.
- The Codex gate passes on the full commit range before pushing.
