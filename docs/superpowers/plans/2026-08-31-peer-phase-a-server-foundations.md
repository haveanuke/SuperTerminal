# Peer Instances Phase A: Server Foundations — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the companion server able to serve a second SuperTerminal instance safely — principals instead of one token, encoded origin instead of inferred, attributed spawn requests, and the wire fields a native pane needs — without changing anything the phone does.

**Architecture:** Every change is server-side and reachable only by a principal that cannot exist yet (no peer is ever created in this phase), so Phase A is a behavioural no-op for the phone. That is the acceptance criterion. Phase B adds pairing and discovery; Phase C adds the attached pane.

**Tech Stack:** Rust 2021, gpui `=0.2.2`, alacritty_terminal `=0.26.0`, serde/serde_json. Hand-rolled HTTP in `companion/http.rs`.

**Spec:** `docs/superpowers/specs/2026-08-31-peer-instances-design.md`

## Global Constraints

- `gpui` and `alacritty_terminal` are pinned with `=` and must never be forked or patched.
- Hand-rolled `extern "C"` FFI, never the `libc` crate.
- No emoji in rendered UI.
- No attribution trailers in commit messages.
- Run `cargo fmt` before every commit; run `cargo test` from the repo root (`/Users/tomas/Documents/projects/SuperTerminal`). Baseline at branch point: **466 passing, 0 failing.**
- This codebase has **no gpui test harness** and tests pure functions only (`core/src/cue.rs`, `native/src/awake.rs`, `native/src/layout.rs`, `native/src/hosts.rs`, plus the companion's own TCP-level e2e suite). **Do not introduce a gpui harness.** Extract pure predicates and test those.
- **The phone must be byte-identical after every task.** Every existing companion test passes with its expected values unchanged. If one would have to change, STOP and report it.

---

### Task 1: `Principal` and route admission

**Files:**
- Modify: `native/src/companion/auth.rs`
- Modify: `native/src/companion/server.rs`
- Test: `native/src/companion/auth.rs` (inline)

**Interfaces:**
- Consumes: nothing.
- Produces: `auth::PeerId(pub String)`; `auth::Principal { Phone, Peer(PeerId) }`; `auth::admits(path: &str, method: Method, principal: &Principal) -> bool`; `auth::principal_for(phone_token: &str, presented: &str) -> Option<Principal>`.

`Principal::Peer` is unreachable in this phase — no peer secret exists for `principal_for` to match. It is defined now so the admission table is written once, not retrofitted.

- [ ] **Step 1: Write the failing tests**

Add to `#[cfg(test)] mod tests` in `native/src/companion/auth.rs`:

```rust
    use crate::companion::http::Method;

    fn peer() -> Principal {
        Principal::Peer(PeerId("p1".to_string()))
    }

    #[test]
    fn the_phone_keeps_every_route_it_has_today() {
        // Phase A must not remove a single phone capability.
        for (path, method) in [
            ("/sessions", Method::Get),
            ("/stream", Method::Get),
            ("/input", Method::Post),
            ("/spawn", Method::Post),
            ("/close", Method::Post),
            ("/rename", Method::Post),
            ("/previews", Method::Get),
            ("/preview", Method::Get),
            ("/version", Method::Get),
        ] {
            assert!(
                admits(path, method, &Principal::Phone),
                "phone lost {path}"
            );
        }
    }

    #[test]
    fn a_peer_may_view_type_and_spawn_but_not_manage() {
        assert!(admits("/sessions", Method::Get, &peer()));
        assert!(admits("/stream", Method::Get, &peer()));
        assert!(admits("/spawn", Method::Post, &peer()));
        // Management and the preview gallery are phone-only for now.
        assert!(!admits("/close", Method::Post, &peer()), "peer got /close");
        assert!(!admits("/rename", Method::Post, &peer()), "peer got /rename");
        assert!(!admits("/previews", Method::Get, &peer()), "peer got /previews");
        assert!(!admits("/preview", Method::Get, &peer()), "peer got /preview");
    }

    #[test]
    fn an_unknown_route_admits_nobody() {
        assert!(!admits("/nonexistent", Method::Get, &Principal::Phone));
        assert!(!admits("/nonexistent", Method::Get, &peer()));
    }

    #[test]
    fn only_the_phone_token_resolves_to_a_principal_in_this_phase() {
        assert_eq!(principal_for("abc123", "abc123"), Some(Principal::Phone));
        assert_eq!(principal_for("abc123", "wrong"), None);
        assert_eq!(principal_for("abc123", ""), None);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p superterminal-native companion::auth`
Expected: FAIL — `Principal`, `PeerId`, `admits`, `principal_for` do not exist.

- [ ] **Step 3: Implement**

Add to `native/src/companion/auth.rs`:

```rust
use crate::companion::http::Method;

/// Identity of a paired peer instance. Phase A never constructs one; it
/// exists so the admission table is written once rather than retrofitted
/// when Phase B adds pairing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PeerId(pub String);

/// Who is making a request. Every protected route states which principals
/// it admits, because a single shared token would otherwise let the phone
/// reach peer-only surfaces and let a peer reach phone-only management.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    Phone,
    Peer(PeerId),
}

/// The admission table. Deny by default: an unknown path admits nobody, so
/// a new route is unreachable until it is listed here deliberately.
pub fn admits(path: &str, method: Method, principal: &Principal) -> bool {
    let phone_only = matches!(
        (path, method),
        ("/close", Method::Post)
            | ("/rename", Method::Post)
            | ("/previews", Method::Get)
            | ("/preview", Method::Get)
    );
    let shared = matches!(
        (path, method),
        ("/sessions", Method::Get)
            | ("/stream", Method::Get)
            | ("/input", Method::Post)
            | ("/spawn", Method::Post)
            | ("/version", Method::Get)
    );
    match principal {
        Principal::Phone => phone_only || shared,
        Principal::Peer(_) => shared,
    }
}

/// Resolve a presented token to a principal. Constant-time against the
/// phone token; Phase A has no peer secrets, so `Peer` is unreachable here.
pub fn principal_for(phone_token: &str, presented: &str) -> Option<Principal> {
    if token_matches(phone_token, presented) {
        Some(Principal::Phone)
    } else {
        None
    }
}
```

Note `/stream` and `/input` and `/preview` arrive as `/stream/<id>` etc. — `admits` takes the ROUTE PREFIX, and the caller passes the prefix it already matched. Check how `serve_connection` dispatches and pass the same prefix string it uses; do not re-parse the path inside `admits`.

- [ ] **Step 4: Wire it into the server**

In `serve_connection` (`server.rs`), replace the single `token_matches(&shared.token, token_of(&request))` gate with a `principal_for(...)` resolution that keeps the SAME response on failure (`404 Not Found`, so a bad token still learns nothing), then check `admits(...)` for the matched route and answer `404` there too. The static page stays the one tokenless route.

Preserve the existing ordering comment's intent exactly: token first (constant-time), then exact Host.

- [ ] **Step 5: Run the full suite**

Run: `cargo test`
Expected: PASS. Every existing companion test passes unmodified — that is the phone-byte-identical criterion.

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add native/src/companion/auth.rs native/src/companion/server.rs
git commit -m "feat(companion): resolve requests to a principal and admit routes per principal"
```

---

### Task 2: Encode session origin instead of inferring it

**Files:**
- Modify: `native/src/companion/hub.rs`
- Modify: `native/src/workspace/companion_ui.rs`
- Test: `native/src/companion/hub.rs` (inline)

**Interfaces:**
- Consumes: `auth::PeerId` (Task 1).
- Produces: `hub::Origin { LocalPty, Attached }`; a per-session `origin: Origin` plus `visible_to: HashSet<PeerId>`; `CompanionHub::register_with_origin(id, label, sender, origin)`; `CompanionHub::publishable_ids() -> Vec<String>`; `CompanionHub::visible_to(&PeerId) -> Vec<String>`; `CompanionHub::set_visible_to(id, &PeerId, bool)`.

Today `companion_ui.rs:85` registers any pane that yields `Some(sender)` from `input_sender()`. An attached pane (Phase C) will also have a sender — it forwards keystrokes to a peer — so origin would be inferred wrongly and an attached pane would be re-published, producing remote views of remote views.

- [ ] **Step 1: Write the failing tests**

Add to `hub.rs`'s existing `mod tests`, using its established `TestHub` / `hub_with` helpers:

```rust
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
        assert_eq!(hub.visible_to(&PeerId("p1".into())), vec!["local".to_string()]);
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p superterminal-native companion::hub`
Expected: FAIL — `Origin`, `register_with_origin`, `publishable_ids`, `visible_to`, `set_visible_to` do not exist.

- [ ] **Step 3: Implement**

In `hub.rs`:

```rust
/// Where a published session's terminal actually lives. Encoded, never
/// inferred: an ATTACHED pane also has an input sender (it forwards
/// keystrokes to its peer), so "has a sender" cannot distinguish them, and
/// re-publishing an attached pane would create remote views of remote views.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    LocalPty,
    Attached,
}
```

Store `origin: Origin` and `visible_to: std::collections::HashSet<PeerId>` on the hub's per-session record. `set_visible_to` is a no-op for `Origin::Attached`. `publishable_ids` filters to `Origin::LocalPty`. Keep the existing `register` delegating to `register_with_origin(.., Origin::LocalPty)` so every current caller and test is unchanged.

- [ ] **Step 4: Update the registration site**

In `native/src/workspace/companion_ui.rs` (~line 85), pass the origin explicitly rather than relying on `input_sender()` being `Some`. In this phase every pane is `Origin::LocalPty`; Phase C supplies `Attached`. Add a comment saying why the distinction exists, so the next reader does not "simplify" it back to a sender check.

- [ ] **Step 5: Run the full suite**

Run: `cargo test`
Expected: PASS, existing assertions unchanged.

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add native/src/companion/hub.rs native/src/workspace/companion_ui.rs
git commit -m "feat(companion): encode session origin so attached panes can never be re-published"
```

---

### Task 3: Attributed spawn requests

**Files:**
- Modify: `native/src/companion/hub.rs`
- Modify: `native/src/companion/server.rs`
- Modify: `native/src/workspace/mod.rs` (the spawn drain, ~line 683)
- Test: `native/src/companion/hub.rs` (inline)

**Interfaces:**
- Consumes: `auth::Principal` (Task 1), `hub::Origin` (Task 2).
- Produces: `hub::SpawnRequest { principal: Principal }`; `CompanionHub::request_spawn_by(principal) -> bool`; `CompanionHub::drain_spawns() -> Vec<SpawnRequest>`.

`request_spawn()` is a bare counter drained as anonymous tab spawns, so it cannot express who asked. A peer-spawned terminal must become visible to exactly the peer that asked — which requires knowing which peer that was.

- [ ] **Step 1: Write the failing tests**

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p superterminal-native companion::hub`
Expected: FAIL — `request_spawn_by` / `drain_spawns` do not exist.

- [ ] **Step 3: Implement**

Replace the `Mutex<usize>` with `Mutex<Vec<SpawnRequest>>`, capped at the existing `MAX_PENDING_SPAWNS`. Keep `request_spawn()` delegating to `request_spawn_by(Principal::Phone)` so existing callers and tests are unchanged.

- [ ] **Step 4: Carry the principal through the drain**

`workspace/mod.rs` (~683) currently drains a count and spawns that many anonymous tabs. It now drains `SpawnRequest`s. For `Principal::Phone` the behaviour is exactly today's. For `Principal::Peer(id)`, the spawned session is additionally made visible to that ONE peer via Task 2's `set_visible_to` — not globally broadcast. No UI path produces a peer request in this phase, so that arm is unreachable but must be written correctly.

- [ ] **Step 5: Run the full suite**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add native/src/companion/hub.rs native/src/companion/server.rs native/src/workspace/mod.rs
git commit -m "feat(companion): spawn requests carry the principal that asked"
```

---

### Task 4: Paste and mouse mode on the wire

**Files:**
- Modify: `native/src/companion/wire.rs`
- Test: `native/src/companion/wire.rs` (inline)

**Interfaces:**
- Consumes: nothing.
- Produces: `WireSnapshot.bracketed_paste: bool` and `WireSnapshot.mouse_tracking: bool`, serialized as `bracketedPaste` and `mouseTracking`.

Local paste fidelity depends on `bracketed_paste` (`pane.rs:894`) and the mode already exists in `RenderableSnapshot` (`term_session.rs:314`) — it simply is not serialized. Without it an attached pane's paste loses the wrapping that stops editors treating a paste as typed input. Mouse tracking crosses so an attached pane can SAY mouse reporting is unavailable rather than silently dropping events.

- [ ] **Step 1: Write the failing test**

This module already has `snap(rows)` and `cell(ch, style)` helpers; use them.
Note `RenderableSnapshot`'s fields are named `bracketed_paste` and
`mouse_tracking` — match those names on the wire as `bracketedPaste` and
`mouseTracking` rather than inventing new ones.

```rust
    #[test]
    fn the_snapshot_carries_paste_and_mouse_modes() {
        let mut snapshot = snap(vec![vec![cell('x', style())]]);
        snapshot.bracketed_paste = true;
        snapshot.mouse_tracking = true;
        let wire = serialize_snapshot(&snapshot, theme());
        assert!(wire.contains("\"bracketedPaste\":true"), "{wire}");
        assert!(wire.contains("\"mouseTracking\":true"), "{wire}");
    }

    #[test]
    fn both_modes_are_always_present_even_when_false() {
        // A missing field and a false field must not be ambiguous to a
        // client deciding whether to wrap a paste.
        let snapshot = snap(vec![vec![cell('x', style())]]);
        let wire = serialize_snapshot(&snapshot, theme());
        assert!(wire.contains("\"bracketedPaste\":false"), "{wire}");
        assert!(wire.contains("\"mouseTracking\":false"), "{wire}");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p superterminal-native companion::wire`
Expected: FAIL — fields absent from the JSON.

- [ ] **Step 3: Implement**

Add both fields to `WireSnapshot` and populate them from `RenderableSnapshot`. They are ALWAYS serialized (no `skip_serializing_if`): a missing field and a false field must not be ambiguous to a client.

- [ ] **Step 4: Confirm the phone is unaffected**

The phone ignores unknown fields, so `page.html` needs no change. Verify no existing wire test asserts an exact whole-JSON string that would now differ; if one does, STOP and report rather than editing its expectation.

- [ ] **Step 5: Run the full suite**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add native/src/companion/wire.rs
git commit -m "feat(companion): carry bracketed-paste and mouse mode in the snapshot"
```

---

### Task 5: A raw-byte input sink for peers

**Files:**
- Modify: `native/src/companion/server.rs`
- Modify: `native/src/companion/input.rs`
- Test: `native/src/companion/e2e_tests.rs`

**Interfaces:**
- Consumes: `auth::Principal` (Task 1).
- Produces: `POST /peer-input/<id>` accepting `{"bytes": [u8]}`, admitted for `Principal::Peer` only.

The phone's `/input` is deliberately symbolic — text plus about ten named keys — which is right for a keypad and useless for a desktop pane (no modified arrows, function keys, home/end/page, alt/meta). Rather than growing that vocabulary, an attached pane runs `keys.rs::key_to_bytes` (the same encoder a local pane writes to its PTY) and ships the resulting bytes.

- [ ] **Step 1: Write the failing tests**

Add to `e2e_tests.rs`, following its existing raw-TCP style:

This file drives the real server over raw TCP. Follow the existing style —
`post_text` at the top of the file shows the exact request shape, and the
`TOKEN` constant is already defined.

```rust
#[test]
fn the_peer_byte_sink_rejects_the_phone_token() {
    // Route admission, not just authentication. The phone authenticates
    // fine; it must still be refused this route, because it has its own
    // symbolic endpoint. This is what proves Task 1's table is ENFORCED
    // and not merely defined.
    let harness = /* start the server exactly as the other tests here do */;
    let body = serde_json::json!({ "bytes": [104, 105] }).to_string();
    let mut stream = TcpStream::connect(&harness.host).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    stream
        .write_all(
            format!(
                "POST /peer-input/t1 HTTP/1.1\r\nHost: {}\r\nX-Companion-Token: {TOKEN}\r\nContent-Type: {INPUT_CONTENT_TYPE}\r\nContent-Length: {}\r\n\r\n{body}",
                harness.host,
                body.len()
            )
            .as_bytes(),
        )
        .unwrap();
    let mut out = String::new();
    let _ = std::io::Read::read_to_string(&mut stream, &mut out);
    assert!(
        out.starts_with("HTTP/1.1 404"),
        "phone token reached the peer sink: {out}"
    );
}
```

Copy the harness construction from whichever test in this file starts a server
(`phone_input_round_trips_to_sse_snapshot` does it first) rather than inventing
one — the `Hub`, `ServerConfig`, and `TOKEN` wiring must match exactly.

Also add a body-cap test in the same style, asserting a `413` for a payload over
`http::MAX_BODY` (4096). A peer is authenticated but still untrusted input.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p superterminal-native companion`
Expected: FAIL — the route does not exist.

- [ ] **Step 3: Implement**

Add the route with the same discipline as `/input`: cap the body via the existing `MAX_BODY`, reject a malformed array rather than interpreting it, and write the bytes through the same `InputSink` the symbolic path uses. Admission is `Principal::Peer` only — add `("/peer-input", Method::Post)` to Task 1's table in the peer arm, NOT the shared arm.

`admits` must be updated in the same commit, or the route is unreachable.

- [ ] **Step 4: Run the full suite**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add native/src/companion/server.rs native/src/companion/input.rs native/src/companion/auth.rs native/src/companion/e2e_tests.rs
git commit -m "feat(companion): raw-byte input sink admitted for peers only"
```

---

### Task 6: Protocol version and capabilities

**Files:**
- Modify: `native/src/companion/server.rs`
- Test: `native/src/companion/e2e_tests.rs`

**Interfaces:**
- Produces: `/version` gains `protocol: u32` and `capabilities: [&str]`.

Two instances may run different builds. `/version` exists; the design requires refusing on protocol mismatch and tolerating build differences only when the protocol matches. A peer that cannot understand this instance must be told why, in the pane, rather than failing obscurely.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn version_advertises_a_protocol_and_capabilities() {
    let harness = /* start the server exactly as the other tests here do */;
    let mut stream = TcpStream::connect(&harness.host).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    stream
        .write_all(
            format!(
                "GET /version?t={TOKEN} HTTP/1.1\r\nHost: {}\r\n\r\n",
                harness.host
            )
            .as_bytes(),
        )
        .unwrap();
    let mut body = String::new();
    let _ = std::io::Read::read_to_string(&mut stream, &mut body);
    assert!(body.contains("\"protocol\":1"), "{body}");
    assert!(body.contains("\"capabilities\""), "{body}");
    assert!(body.contains("peer-input"), "{body}");
}
```

The `GET /sessions?t={TOKEN}` line already in this file is the exact request
shape to copy.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p superterminal-native companion`
Expected: FAIL.

- [ ] **Step 3: Implement**

Add `protocol: 1` and a capability list naming what this build serves (at minimum `peer-input`, `origin`, `principals`). Keep every field `/version` already returns — the phone may read it.

- [ ] **Step 4: Run the full suite**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add native/src/companion/server.rs native/src/companion/e2e_tests.rs
git commit -m "feat(companion): advertise a protocol version and capabilities"
```

---

## Done criteria

- `cargo test` green from the repo root; `cargo fmt --check` clean.
- **Every pre-existing companion test passes with its expected values unchanged** — the phone is byte-identical.
- No UI path constructs a `Principal::Peer` or an `Origin::Attached`; both are reachable only from tests in this phase.
- The Codex gate passes on the full commit range before pushing.
