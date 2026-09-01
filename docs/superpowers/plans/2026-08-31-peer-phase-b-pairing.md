# Peer Instances Phase B: Pairing, Scoping and Discovery — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make peers real — per-peer secrets with individual revocation, three explicit grants, and session-visibility scoping — so that by the end of this phase a paired instance can authenticate and see exactly what it was granted, and nothing more.

**Architecture:** The task order is a safety property, not a preference. Scoping and grants are built and enforced FIRST, while `principal_for` still cannot return `Peer`. Only after both exist does Task 4 make peer authentication possible. No commit in this branch's history has peer auth without scoping.

**Tech Stack:** Rust 2021, gpui `=0.2.2`, alacritty_terminal `=0.26.0`, serde/serde_json.

**Spec:** `docs/superpowers/specs/2026-08-31-peer-instances-design.md`

## Global Constraints

- `gpui` and `alacritty_terminal` are pinned with `=` and must never be forked or patched.
- Hand-rolled `extern "C"` FFI, never the `libc` crate.
- No emoji in rendered UI — use the SVG set in `native/src/icons.rs`.
- No attribution trailers in commit messages.
- `cargo fmt` before every commit; `cargo test` from the repo root. Baseline: **488 passing, 0 failing.**
- No gpui test harness exists and none may be introduced. Test pure functions; UI-adjacent logic gets extracted into a pure function and tested there.
- **The phone must stay byte-identical.** Every pre-existing companion test passes with expected values unchanged.
- Secrets are 128-bit from `/dev/urandom` via the existing `companion/auth.rs` pattern. No new crate for randomness, hashing, or crypto.

## The binding requirement this phase must satisfy

Spec D3b: route admission and session-visibility scoping are **two separate checks and both are required.** Phase A implemented admission and left scoping unwired: `hub.visible_to()` and `hub.publishable_ids()` exist with no callers, while `/sessions`, `/stream/<id>` and `/peer-input/<id>` all admit `Peer`. **No task in this phase may enable peer authentication before Tasks 2 and 3 have landed.**

---

### Task 1: `PeerRecord` and permissive loading

**Files:**
- Create: `native/src/peers.rs`
- Modify: `native/src/main.rs` (add `mod peers;`)
- Modify: `native/src/settings.rs`
- Test: `native/src/peers.rs` (inline)

**Interfaces:**
- Consumes: `companion::auth::PeerId`.
- Produces: `peers::Grants { view: bool, type_: bool, spawn: bool }`; `peers::PeerRecord { id: PeerId, label: String, secret: String, grants: Grants }`; `peers::new_peer_secret() -> String`; `peers::load_peers(&serde_json::Value) -> (Vec<PeerRecord>, Vec<PeerProblem>)`; `Settings::peers()`.

This mirrors `hosts::load_profiles` deliberately — same permissive shape, same duplicate-quarantine rule, same reason. `settings.rs:201` is `serde_json::from_str(&text).unwrap_or_default()`, so a typed field would let one hand-edited peer reset every setting the user has.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn raw(json: &str) -> serde_json::Value {
        serde_json::from_str(json).unwrap()
    }

    const OK: &str = r#"[{"id":"p1","label":"work","secret":"aabbccddeeff00112233445566778899","grants":{"view":true,"type":false,"spawn":false}}]"#;

    #[test]
    fn a_well_formed_peer_loads_with_its_grants() {
        let (ok, problems) = load_peers(&raw(OK));
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].label, "work");
        assert!(ok[0].grants.view);
        assert!(!ok[0].grants.type_);
        assert!(!ok[0].grants.spawn);
        assert!(problems.is_empty());
    }

    #[test]
    fn every_grant_defaults_off_when_absent() {
        // A peer record missing its grants must not silently mean "allow".
        let (ok, _) = load_peers(&raw(
            r#"[{"id":"p1","label":"work","secret":"aabbccddeeff00112233445566778899"}]"#,
        ));
        assert_eq!(ok.len(), 1);
        assert!(!ok[0].grants.view);
        assert!(!ok[0].grants.type_);
        assert!(!ok[0].grants.spawn);
    }

    #[test]
    fn duplicate_ids_quarantine_every_colliding_peer() {
        // Never first-wins or last-wins: an ambiguous id must not be able to
        // resolve to the wrong machine's grants.
        let (ok, problems) = load_peers(&raw(
            r#"[{"id":"dup","label":"a","secret":"aabbccddeeff00112233445566778899"},
                {"id":"dup","label":"b","secret":"99887766554433221100ffeeddccbbaa"},
                {"id":"solo","label":"c","secret":"0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f"}]"#,
        ));
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].label, "c");
        assert_eq!(problems.len(), 2);
    }

    #[test]
    fn a_duplicate_secret_quarantines_every_peer_sharing_it() {
        // Two peers with the same secret are indistinguishable at auth time,
        // so neither may be trusted to carry its own grants.
        let (ok, problems) = load_peers(&raw(
            r#"[{"id":"p1","label":"a","secret":"aabbccddeeff00112233445566778899"},
                {"id":"p2","label":"b","secret":"aabbccddeeff00112233445566778899"}]"#,
        ));
        assert!(ok.is_empty());
        assert_eq!(problems.len(), 2);
    }

    #[test]
    fn a_malformed_container_yields_no_peers_rather_than_an_error() {
        let (ok, problems) = load_peers(&raw("5"));
        assert!(ok.is_empty());
        assert_eq!(problems.len(), 1);
    }

    #[test]
    fn a_short_or_non_hex_secret_is_refused() {
        for bad in ["", "abc", "ZZZZccddeeff00112233445566778899", "aabbccddeeff0011223344556677889"] {
            let json = format!(
                r#"[{{"id":"p1","label":"a","secret":"{bad}"}}]"#
            );
            let (ok, problems) = load_peers(&raw(&json));
            assert!(ok.is_empty(), "accepted secret {bad:?}");
            assert_eq!(problems.len(), 1);
        }
    }

    #[test]
    fn generated_secrets_are_unique_and_full_width() {
        let a = new_peer_secret();
        let b = new_peer_secret();
        assert_ne!(a, b);
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p superterminal-native peers`
Expected: FAIL — the module does not exist.

- [ ] **Step 3: Implement `peers.rs`**

```rust
//! Paired peer instances: another SuperTerminal on another machine.
//!
//! Pairing rather than tailnet-membership because the tailnet includes a WORK
//! MacBook, which may carry MDM or IT admin access. Per-peer secrets give
//! individual revocation and a label the user established rather than one a
//! peer asserts about itself.
//!
//! A bearer secret is a capability, not an identity proof: an administrator on
//! the peer machine who can read the stored secret can replay it. Keypairs
//! would prevent that; this is a stated limit, not an oversight.

use serde::{Deserialize, Serialize};

use crate::companion::auth::PeerId;

/// What a peer is allowed to do here. Every grant defaults OFF: a record
/// missing its grants must never mean "allow".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Grants {
    /// See broadcast sessions at all.
    pub view: bool,
    /// Send input to them. Named `type_` because `type` is a keyword.
    #[serde(rename = "type")]
    pub type_: bool,
    /// Create new terminals here.
    pub spawn: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerRecord {
    pub id: PeerId,
    pub label: String,
    /// 32 lowercase hex chars. Compared in constant time at auth.
    pub secret: String,
    #[serde(default)]
    pub grants: Grants,
}

#[derive(Debug, PartialEq)]
pub struct PeerProblem {
    pub label: String,
    pub reason: String,
}

const SECRET_LEN: usize = 32;

pub fn new_peer_secret() -> String {
    crate::companion::auth::generate_token()
}

fn secret_ok(secret: &str) -> bool {
    secret.len() == SECRET_LEN
        && secret
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// Permissive loading, mirroring `hosts::load_profiles`' reasoning: settings fall
/// back to `Settings::default()` on ANY serde error, so one hand-edited peer
/// must not be able to reset every unrelated setting.
///
/// Quarantines EVERY member of a duplicate id — and every member of a duplicate
/// SECRET, because two peers sharing a secret are indistinguishable at auth
/// time and neither could be trusted to carry its own grants.
pub fn load_peers(raw: &serde_json::Value) -> (Vec<PeerRecord>, Vec<PeerProblem>) {
    let mut problems = Vec::new();
    let Some(items) = raw.as_array() else {
        if !raw.is_null() {
            problems.push(PeerProblem {
                label: String::new(),
                reason: "peers is not a list".to_string(),
            });
        }
        return (Vec::new(), problems);
    };
    let mut candidates: Vec<PeerRecord> = Vec::new();
    for item in items {
        let label = item
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or("(unnamed)")
            .to_string();
        let parsed: PeerRecord = match serde_json::from_value(item.clone()) {
            Ok(peer) => peer,
            Err(error) => {
                problems.push(PeerProblem { label, reason: error.to_string() });
                continue;
            }
        };
        if parsed.id.0.is_empty() {
            problems.push(PeerProblem { label, reason: "empty id".into() });
            continue;
        }
        if !secret_ok(&parsed.secret) {
            problems.push(PeerProblem { label, reason: "bad secret".into() });
            continue;
        }
        candidates.push(parsed);
    }
    let mut kept = Vec::new();
    for peer in &candidates {
        let id_dupes = candidates.iter().filter(|o| o.id == peer.id).count();
        let secret_dupes = candidates.iter().filter(|o| o.secret == peer.secret).count();
        if id_dupes > 1 {
            problems.push(PeerProblem {
                label: peer.label.clone(),
                reason: format!("duplicate id {}", peer.id.0),
            });
        } else if secret_dupes > 1 {
            problems.push(PeerProblem {
                label: peer.label.clone(),
                reason: "duplicate secret".to_string(),
            });
        } else {
            kept.push(peer.clone());
        }
    }
    (kept, problems)
}
```

Add `mod peers;` to `native/src/main.rs`.

- [ ] **Step 4: Store peers in settings**

In `native/src/settings.rs`, add — raw, for the same reason `remote_profiles` is raw:

```rust
    /// Raw, so a malformed entry can never fail whole-settings serde and trip
    /// the `unwrap_or_default()` fallback at load.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub peers: serde_json::Value,
```

and a resolver:

```rust
    pub fn peers(&self) -> (Vec<crate::peers::PeerRecord>, Vec<crate::peers::PeerProblem>) {
        crate::peers::load_peers(&self.peers)
    }
```

Add a settings test proving a malformed peer leaves unrelated settings intact, modelled on the existing `a_malformed_profile_does_not_reset_unrelated_settings`.

- [ ] **Step 5: Run the full suite**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add native/src/peers.rs native/src/main.rs native/src/settings.rs
git commit -m "feat(native): peer records with per-peer secrets and default-off grants"
```

---

### Task 2: Scope what a peer may see — wire `visible_to`

**Files:**
- Modify: `native/src/companion/hub.rs`
- Modify: `native/src/companion/server.rs`
- Test: `native/src/companion/hub.rs` (inline), `native/src/companion/e2e_tests.rs`

**Interfaces:**
- Consumes: `hub::{Origin, visible_to, publishable_ids}`, `auth::{Principal, PeerId}`.
- Produces: `CompanionHub::sessions_for(&Principal) -> Vec<SessionInfo>`; `CompanionHub::may_touch(&Principal, id) -> bool`.

**This task satisfies the spec's binding requirement.** It lands BEFORE peer authentication exists, so at no point in this branch's history can a peer authenticate without being scoped.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn the_phone_still_sees_every_session() {
        // Scoping must not change what the phone sees. This is the
        // regression guard for the whole task.
        let hub = TestHub::new();
        let (tx, _rx) = std::sync::mpsc::channel();
        hub.register_with_origin("a", "one", tx.clone(), Origin::LocalPty);
        hub.register_with_origin("b", "two", tx, Origin::LocalPty);
        let seen: Vec<String> = hub
            .sessions_for(&Principal::Phone)
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(seen.len(), 2);
    }

    #[test]
    fn a_peer_sees_only_what_it_was_made_visible() {
        let hub = TestHub::new();
        let (tx, _rx) = std::sync::mpsc::channel();
        hub.register_with_origin("a", "one", tx.clone(), Origin::LocalPty);
        hub.register_with_origin("b", "two", tx, Origin::LocalPty);
        let p1 = PeerId("p1".into());
        hub.set_visible_to("a", &p1, true);
        let seen: Vec<String> = hub
            .sessions_for(&Principal::Peer(p1.clone()))
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(seen, vec!["a".to_string()]);
    }

    #[test]
    fn a_peer_sees_nothing_before_anything_is_broadcast() {
        let hub = TestHub::new();
        let (tx, _rx) = std::sync::mpsc::channel();
        hub.register_with_origin("a", "one", tx, Origin::LocalPty);
        assert!(hub
            .sessions_for(&Principal::Peer(PeerId("p1".into())))
            .is_empty());
    }

    #[test]
    fn a_peer_may_not_touch_a_session_it_cannot_see() {
        let hub = TestHub::new();
        let (tx, _rx) = std::sync::mpsc::channel();
        hub.register_with_origin("a", "one", tx.clone(), Origin::LocalPty);
        hub.register_with_origin("b", "two", tx, Origin::LocalPty);
        let p1 = PeerId("p1".into());
        hub.set_visible_to("a", &p1, true);
        let peer = Principal::Peer(p1);
        assert!(hub.may_touch(&peer, "a"));
        assert!(!hub.may_touch(&peer, "b"), "peer reached an unshared session");
        assert!(hub.may_touch(&Principal::Phone, "b"), "phone lost access");
    }

    #[test]
    fn a_peer_may_never_touch_an_attached_session() {
        // Defence in depth: set_visible_to already refuses Attached, but
        // may_touch must not depend on that having been called correctly.
        let hub = TestHub::new();
        let (tx, _rx) = std::sync::mpsc::channel();
        hub.register_with_origin("mirror", "two", tx, Origin::Attached);
        let p1 = PeerId("p1".into());
        hub.set_visible_to("mirror", &p1, true);
        assert!(!hub.may_touch(&Principal::Peer(p1), "mirror"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p superterminal-native companion::hub`
Expected: FAIL — `sessions_for` / `may_touch` do not exist.

- [ ] **Step 3: Implement**

`sessions_for` returns everything for `Phone`, and for `Peer(id)` only sessions whose `origin == LocalPty` AND whose `visible_to` contains that id. `may_touch` answers the same question for a single id. Both check origin independently rather than trusting `set_visible_to` to have refused — the two guards are deliberately redundant.

- [ ] **Step 4: Use them at all three handlers**

Replace the Phase A comments with real calls:
- `/sessions` returns `hub.sessions_for(&principal)`.
- `/stream/<id>` refuses with the same 404 shape when `!hub.may_touch(&principal, id)`.
- `/peer-input/<id>` likewise.

The 404 must be indistinguishable from a bad token and a denied route — do not introduce a distinguishable "you may not see this" response.

- [ ] **Step 5: Add an e2e test proving the phone is unaffected**

Following `e2e_tests.rs`'s raw-TCP style, assert the phone still gets its full session list from `/sessions`. Copy the harness from `phone_input_round_trips_to_sse_snapshot`.

- [ ] **Step 6: Run the full suite**

Run: `cargo test`
Expected: PASS, every pre-existing assertion unchanged.

- [ ] **Step 7: Commit**

```bash
cargo fmt
git add native/src/companion/hub.rs native/src/companion/server.rs native/src/companion/e2e_tests.rs
git commit -m "feat(companion): scope what a peer may see and touch to what was shared with it"
```

---

### Task 3: Enforce the three grants

**Files:**
- Modify: `native/src/companion/auth.rs`
- Modify: `native/src/companion/server.rs`
- Test: `native/src/companion/auth.rs` (inline)

**Interfaces:**
- Consumes: `peers::Grants`, `auth::Principal`.
- Produces: `auth::admits_with_grants(path, method, principal, grants: Option<Grants>) -> bool`.

Admission now has two questions: may this PRINCIPAL use this route (Phase A's table), and does this PEER hold the grant that route needs. Both must pass.

- [ ] **Step 1: Write the failing tests**

```rust
    fn all_off() -> crate::peers::Grants {
        crate::peers::Grants::default()
    }

    #[test]
    fn a_peer_with_no_grants_can_do_nothing() {
        let p = peer();
        for (path, method) in [
            ("/sessions", Method::Get),
            ("/stream", Method::Get),
            ("/peer-input", Method::Post),
            ("/spawn", Method::Post),
        ] {
            assert!(
                !admits_with_grants(path, method, &p, Some(all_off())),
                "no-grant peer reached {path}"
            );
        }
    }

    #[test]
    fn view_alone_permits_looking_but_not_typing_or_spawning() {
        let g = crate::peers::Grants { view: true, ..Default::default() };
        let p = peer();
        assert!(admits_with_grants("/sessions", Method::Get, &p, Some(g)));
        assert!(admits_with_grants("/stream", Method::Get, &p, Some(g)));
        assert!(!admits_with_grants("/peer-input", Method::Post, &p, Some(g)));
        assert!(!admits_with_grants("/spawn", Method::Post, &p, Some(g)));
    }

    #[test]
    fn typing_requires_view_as_well() {
        // Input into a session you cannot see is meaningless; requiring both
        // stops a half-configured peer from typing blind.
        let g = crate::peers::Grants { type_: true, ..Default::default() };
        assert!(!admits_with_grants("/peer-input", Method::Post, &peer(), Some(g)));
    }

    #[test]
    fn spawn_is_independent_of_view() {
        let g = crate::peers::Grants { spawn: true, ..Default::default() };
        assert!(admits_with_grants("/spawn", Method::Post, &peer(), Some(g)));
    }

    #[test]
    fn the_phone_is_unaffected_by_grants() {
        // Grants describe peers. The phone's table is unchanged.
        for (path, method) in [
            ("/sessions", Method::Get),
            ("/close", Method::Post),
            ("/previews", Method::Get),
        ] {
            assert!(admits_with_grants(path, method, &Principal::Phone, None));
        }
    }

    #[test]
    fn a_peer_with_no_record_is_refused_everything() {
        // `None` grants means the peer resolved to no record at all.
        assert!(!admits_with_grants("/sessions", Method::Get, &peer(), None));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p superterminal-native companion::auth`
Expected: FAIL — `admits_with_grants` does not exist.

- [ ] **Step 3: Implement**

`admits_with_grants` first calls Phase A's `admits`; if that denies, deny. Then for `Principal::Peer`, require the grant the route needs: `/sessions` and `/stream` need `view`; `/peer-input` needs `view && type_`; `/spawn` needs `spawn`. `Principal::Phone` ignores grants entirely. `None` grants for a peer denies everything.

Wire it at the dispatcher in place of `admits`, resolving the peer's grants from settings.

- [ ] **Step 4: Run the full suite**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add native/src/companion/auth.rs native/src/companion/server.rs
git commit -m "feat(companion): enforce per-peer view/type/spawn grants alongside route admission"
```

---

### Task 4: Make peers authenticate

**Files:**
- Modify: `native/src/companion/auth.rs`
- Modify: `native/src/companion/server.rs`
- Test: `native/src/companion/auth.rs`, `native/src/companion/e2e_tests.rs`

**Interfaces:**
- Produces: `principal_for` resolving a presented secret against the peer list to `Principal::Peer(id)`.

**This is the commit that makes peers real.** It lands only after Tasks 2 and 3, so scoping and grants already exist. Do not reorder it.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn a_paired_secret_resolves_to_that_peer() {
        let peers = vec![("p1", "aabbccddeeff00112233445566778899")];
        assert_eq!(
            principal_for_with_peers("phone-token", "aabbccddeeff00112233445566778899", &peers),
            Some(Principal::Peer(PeerId("p1".into())))
        );
    }

    #[test]
    fn the_phone_token_still_wins_and_is_unchanged() {
        let peers = vec![("p1", "aabbccddeeff00112233445566778899")];
        assert_eq!(
            principal_for_with_peers("phone-token", "phone-token", &peers),
            Some(Principal::Phone)
        );
    }

    #[test]
    fn an_unknown_secret_resolves_to_nobody() {
        let peers = vec![("p1", "aabbccddeeff00112233445566778899")];
        assert_eq!(principal_for_with_peers("phone-token", "nope", &peers), None);
        assert_eq!(principal_for_with_peers("phone-token", "", &peers), None);
    }
```

Adapt the helper's shape to however you plumb the peer list; the property is what matters.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p superterminal-native companion::auth`
Expected: FAIL.

- [ ] **Step 3: Implement**

Compare against every peer secret in CONSTANT time — use the existing `token_matches`, and do not early-return on the first mismatch in a way that leaks how many peers exist or which matched. Check the phone token first, preserving today's behaviour exactly.

- [ ] **Step 4: Add the e2e proof**

A peer secret with `view` granted can `GET /sessions` and sees only what was shared; the same secret is refused `/close` (phone-only) and `/previews`. This is the first end-to-end proof that admission, grants and scoping compose.

- [ ] **Step 5: Run the full suite**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add native/src/companion/auth.rs native/src/companion/server.rs native/src/companion/e2e_tests.rs
git commit -m "feat(companion): resolve a paired peer secret to its principal"
```

---

### Task 5: Settle `/input` for peers

**Files:**
- Modify: `native/src/companion/auth.rs`
- Modify: `docs/superpowers/specs/2026-08-31-peer-instances-design.md`
- Test: `native/src/companion/auth.rs`

Spec D3b records an unresolved question raised by the pre-push review: Phase A's table put `/input` in the SHARED arm, so a peer is admitted to the phone's symbolic endpoint as well as the raw sink. D1 argues a peer should ship raw bytes because the symbolic vocabulary is phone-grade.

**Decision to implement: move `/input` to the phone-only arm.** A peer has `/peer-input`, which is strictly more expressive; leaving it two ways in means two code paths to keep in step and two places to get scoping wrong. If a peer ever needs the symbolic form, that is a deliberate change with its own reasoning.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn a_peer_uses_the_raw_sink_not_the_symbolic_one() {
        let g = crate::peers::Grants { view: true, type_: true, ..Default::default() };
        assert!(!admits_with_grants("/input", Method::Post, &peer(), Some(g)),
            "peer reached the phone's symbolic endpoint");
        assert!(admits_with_grants("/peer-input", Method::Post, &peer(), Some(g)));
    }
```

Also assert the phone KEEPS `/input` — that route moving must not cost the phone anything.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p superterminal-native companion::auth`
Expected: FAIL — `/input` is currently in the shared arm.

- [ ] **Step 3: Implement and record**

Move `("/input", Method::Post)` from `shared` to `phone_only`. Update spec D3b from an open question to a recorded decision with this reasoning.

- [ ] **Step 4: Run the full suite**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add native/src/companion/auth.rs docs/superpowers/specs/2026-08-31-peer-instances-design.md
git commit -m "feat(companion): peers use the raw input sink, not the phone's symbolic one"
```

---

### Task 6: Retire the delegating shims

**Files:**
- Modify: `native/src/companion/hub.rs`
- Modify: `native/src/companion/server.rs` (tests)

Phase A left `request_spawn` and `take_spawns` as `#[cfg_attr(not(test), allow(dead_code))]` wrappers purely so pre-existing tests stayed byte-identical while the phase proved the phone unchanged. That proof is done and merged. The suite now pins an API production never calls, while the real path gets thinner coverage.

- [ ] **Step 1: Migrate the tests**

Rewrite `spawn_queue_caps_and_drains` and `spawn_queues_with_guards_and_caps` (and any other caller) onto `request_spawn_by(Principal::Phone)` and `drain_spawns()`. Assertions must keep the SAME expected values — only the API called changes. If an assertion would have to change, STOP and report it, because that would mean the wrappers were not behaviourally identical after all.

- [ ] **Step 2: Delete the wrappers**

Remove `request_spawn` and `take_spawns` entirely.

- [ ] **Step 3: Run the full suite and check warnings**

Run: `cargo test` and `cargo check -p superterminal-native`
Expected: PASS, and no new warnings.

- [ ] **Step 4: Commit**

```bash
cargo fmt
git add native/src/companion/hub.rs native/src/companion/server.rs
git commit -m "refactor(companion): retire the spawn-queue shims now the phone proof is merged"
```

---

### Task 7: Discovery and pairing surface

**Files:**
- Modify: `native/src/peers.rs`
- Modify: `native/src/workspace/settings_ui.rs`
- Test: `native/src/peers.rs` (inline)

**Interfaces:**
- Produces: `peers::Candidate { host: String, addr: String, os: String }`; `peers::parse_tailscale_status(&str) -> Vec<Candidate>`; a settings surface listing candidates, pairing one, and revoking a peer.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn tailscale_peers_become_candidates() {
        let json = r#"{"Peer":{"k1":{"HostName":"work-mbp","TailscaleIPs":["100.64.0.2"],"OS":"macOS","Online":true}}}"#;
        let found = parse_tailscale_status(json);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].host, "work-mbp");
        assert_eq!(found[0].addr, "100.64.0.2");
    }

    #[test]
    fn offline_and_non_desktop_peers_are_not_offered() {
        // The tailnet also holds an Android phone, which is not a peer host.
        let json = r#"{"Peer":{
            "k1":{"HostName":"pixel","TailscaleIPs":["100.64.0.3"],"OS":"android","Online":true},
            "k2":{"HostName":"off","TailscaleIPs":["100.64.0.4"],"OS":"macOS","Online":false}}}"#;
        assert!(parse_tailscale_status(json).is_empty());
    }

    #[test]
    fn malformed_status_yields_no_candidates_rather_than_panicking() {
        for bad in ["", "null", "{}", "not json", r#"{"Peer":5}"#] {
            assert!(parse_tailscale_status(bad).is_empty(), "panicked or accepted {bad:?}");
        }
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p superterminal-native peers`
Expected: FAIL.

- [ ] **Step 3: Implement the parser**

Parse with the serde_json already in the tree. Never panic on malformed input — a missing or wrongly-typed field yields no candidate, following `companion/blender.rs`'s temperament (absent on failure, no hot retry).

- [ ] **Step 4: Run the scan safely**

Shelling `tailscale status --json` gets a total deadline and an output cap, exactly as `blender.rs` bounds its probe. `tailscale` missing from PATH means no candidates and the feature is simply absent, not an error dialog.

- [ ] **Step 5: Revocation must take effect immediately (BLOCKING acceptance criterion)**

`ServerConfig.peers` is a snapshot resolved once at server start (`server.rs:62`)
and never refreshed. Nothing restarts the companion when settings change. So a
peer deleted from the list would KEEP WORKING until the server is next toggled —
potentially the whole app session.

That is not acceptable in the task that ships "delete = revocation". The entire
argument for per-peer pairing over one shared token was that the work Mac can be
cut off without rotating the token the phone uses; revocation that silently waits
for a restart is not revocation.

`regenerate_companion_token` (`companion_ui.rs:220-231`) already has the pattern:
it calls `stop_companion()` then `toggle_companion()` around the settings write,
so the change is live immediately. Apply the same treatment to every peer
mutation — delete, and any grant toggle, since narrowing a grant is also a
revocation of part of that peer's authority.

Add a test asserting that whatever function applies a peer change routes through
that restart path. If a pure test is not possible because it is UI-bound, extract
the decision ("does this settings change require a companion restart?") into a
pure predicate in `peers.rs` and test THAT — do not ship it untested and do not
introduce a gpui harness.

- [ ] **Step 6: The pairing surface**

In the settings UI: list candidates; pairing one generates a `PeerRecord` with a fresh id and secret and ALL GRANTS OFF, then shows the secret for transfer (reuse `companion/qr::matrix`, as the phone link already does). Paired peers list with their three grants as individual toggles and a delete action that is the revocation.

No emoji — use `icons.rs`. Any decision logic here (which candidates are offerable, what a toggle means) goes in `peers.rs` as a pure function and is tested there; the UI only renders it.

- [ ] **Step 7: Run the full suite**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
cargo fmt
git add native/src/peers.rs native/src/workspace/settings_ui.rs native/src/workspace/companion_ui.rs
git commit -m "feat(native): discover tailnet candidates and pair peers with explicit grants"
```

---

## Done criteria

- `cargo test` green from the repo root; `cargo fmt --check` clean; no new `cargo check` warnings.
- Every pre-existing companion test passes with expected values unchanged — the phone is byte-identical.
- **No commit in this branch enables peer authentication before scoping and grants exist.** Task 4 is the enabling commit and it follows Tasks 2 and 3.
- A peer with no grants can reach nothing; `view` alone cannot type or spawn; a peer never sees a session that was not shared with it, and never an `Origin::Attached` one.
- The Codex gate passes on the full commit range before pushing.
