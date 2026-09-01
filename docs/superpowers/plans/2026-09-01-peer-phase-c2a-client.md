# Peer Instances Phase C2a: The Peer Client — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Teach SuperTerminal to be a CLIENT of another SuperTerminal's companion server — list what a peer has shared, stream a session's frames, and send raw key bytes back. No UI, no pane.

**Architecture:** This is the first time this app makes outbound HTTP. Everything here is a library with real tests: the companion server can be started in-process (the e2e suite already does it), so the client is tested against the real server rather than mocks. Phase C2b renders it into a pane.

**Tech Stack:** Rust 2021, serde/serde_json, `std::net`. Hand-rolled HTTP, matching how the server side is written. **No new crate** — no reqwest, no hyper, no ureq.

**Spec:** `docs/superpowers/specs/2026-08-31-peer-instances-design.md` — D1 (raw byte input), D2 (broadcaster-owned geometry), D5 (the degraded contract).

## Global Constraints

- `gpui` and `alacritty_terminal` are pinned with `=` and must never be forked or patched.
- Hand-rolled `extern "C"` FFI, never the `libc` crate.
- No emoji in rendered UI.
- No attribution trailers in commit messages.
- `cargo fmt` before every commit; `cargo test` from the repo root. Baseline: **560 passing, 0 failing.**
- No gpui test harness. Everything in this phase is testable without one — say so if you find otherwise rather than introducing one.
- **The phone must stay byte-identical.** This phase adds a client; it must not change the server.
- Every outbound operation is bounded: a total deadline and a response cap, following `companion/blender.rs`'s temperament — one attempt, absent on failure, no hot retry.

## The temperament this client must have

It talks to another machine over a network that will drop. Model it on `blender.rs`, which is the existing precedent for outbound I/O in this codebase: a total round-trip deadline (not just per-read socket timeouts, which a peer trickling bytes could hold open forever), a hard response cap, one attempt per interval, and failure meaning "absent" rather than an error path that retries hot.

A peer that is slow, hostile, or wedged must never be able to hang the caller or exhaust memory.

---

### Task 1: The wire type round-trips

**Files:**
- Modify: `native/src/companion/wire.rs`
- Test: `native/src/companion/wire.rs` (inline)

**Interfaces:**
- Produces: `WireSnapshot`, `WireCursor`, `WireRun` all deriving `Deserialize` in addition to `Serialize`.

The client must parse what the server emits. Deriving both directions on the SAME struct — rather than defining a mirror type in the client — means the two can never drift: a field added for the server is automatically understood by the client, and a rename breaks compilation instead of silently producing empty frames.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn a_snapshot_survives_a_round_trip() {
        // The client parses exactly what the server emits. Deriving both
        // directions on one struct is what stops them drifting.
        let mut snapshot = snap(vec![vec![cell('x', style())]]);
        snapshot.bracketed_paste = true;
        let json = serialize_snapshot(&snapshot, theme());
        let parsed: WireSnapshot =
            serde_json::from_str(&json).expect("server output must parse as WireSnapshot");
        assert_eq!(parsed.cols, 1);
        assert_eq!(parsed.lines, 1);
        assert!(parsed.bracketed_paste);
        assert_eq!(parsed.rows.len(), 1);
    }

    #[test]
    fn an_absent_history_field_parses_as_empty() {
        // `history` is skipped when empty, so the client must tolerate it
        // being absent rather than treating that as malformed.
        let json = r#"{"cols":1,"lines":1,"cursor":null,"appCursor":false,"rows":[[]],"bracketedPaste":false,"mouseTracking":false}"#;
        let parsed: WireSnapshot = serde_json::from_str(json).expect("must parse");
        assert!(parsed.history.is_empty());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p superterminal-native companion::wire`
Expected: FAIL — `WireSnapshot` does not implement `Deserialize`.

- [ ] **Step 3: Implement**

Add `Deserialize` to the derive on `WireSnapshot`, `WireCursor`, `WireRun`, and any nested type they contain. Add `#[serde(default)]` where the server skips a field when empty, so an absent field parses rather than erroring.

Check every field: any the server omits conditionally needs a default on the client side.

- [ ] **Step 4: Run the full suite**

Run: `cargo test`
Expected: PASS. The server's output is unchanged — verify no existing wire assertion moved.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add native/src/companion/wire.rs
git commit -m "feat(companion): the wire snapshot parses as well as serialises"
```

---

### Task 2: A bounded HTTP client

**Files:**
- Create: `native/src/peer_client/mod.rs` (and `mod peer_client;` in `native/src/main.rs`)
- Test: inline

**Interfaces:**
- Produces: `peer_client::Endpoint { addr: SocketAddr, secret: String }`; `peer_client::get(&Endpoint, path: &str, deadline: Duration) -> Result<Vec<u8>, PeerError>`; `peer_client::post(&Endpoint, path: &str, body: &[u8], deadline: Duration) -> Result<(), PeerError>`; `peer_client::PeerError`.

Hand-rolled, matching the server's own HTTP handling. The token goes in the `X-Companion-Token` header, exactly as the phone sends it.

- [ ] **Step 1: Write the failing tests**

Start a real companion server in-process — copy the harness from `companion/e2e_tests.rs`'s first server-starting test — and point the client at it. Cover:

- a `GET /sessions` with a valid peer secret returns a body that parses as JSON
- a `GET` with a wrong secret returns the error variant, not a panic and not a success
- a `POST /peer-input/<id>` with a valid secret succeeds
- a response larger than the cap is refused rather than buffered without limit
- a connect to a closed port fails within the deadline rather than blocking
- **a peer that accepts the connection and then sends nothing at all fails at the deadline.** This is the one that matters: a per-read socket timeout alone would let a peer trickling one byte per second hold the caller forever. The deadline must be a TOTAL budget for the round trip, as `blender.rs:60` does.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p superterminal-native peer_client`
Expected: FAIL — module does not exist.

- [ ] **Step 3: Implement**

`TcpStream::connect_timeout`, then write a minimal request, then read the response with the REMAINING budget re-applied before each read so the total cannot be exceeded. Cap the body. Parse the status line and headers only as far as needed; reject anything ambiguous rather than interpreting it, the way `companion/http.rs` does on the server side.

- [ ] **Step 4: Run the full suite**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add native/src/peer_client/ native/src/main.rs
git commit -m "feat(peer): a bounded hand-rolled client for talking to another instance"
```

---

### Task 3: Reading the event stream

**Files:**
- Create: `native/src/peer_client/sse.rs`
- Test: inline

**Interfaces:**
- Produces: `sse::FrameReader` — wraps a reader, yields one complete `data:` payload per call, skipping `:` heartbeat comments.

The server sends SSE: `data: {json}\n\n`, with `:hb\n\n` heartbeats every 2s. The reader must yield snapshots and silently ignore heartbeats.

- [ ] **Step 1: Write the failing tests**

Test the parser against byte slices — no socket needed, so this is pure and exhaustive:

- one well-formed `data:` frame yields its payload
- a `:hb` comment yields nothing and does not end the stream
- two frames back to back yield both, in order
- a frame split across read boundaries is reassembled
- a frame larger than the cap is refused rather than buffered forever
- a stream that ends mid-frame yields nothing rather than a partial payload
- CRLF and LF line endings both work

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p superterminal-native peer_client::sse`
Expected: FAIL.

- [ ] **Step 3: Implement**

Line-oriented, with a cap on both line length and accumulated frame size. A frame that exceeds the cap ends the stream rather than truncating silently — a truncated frame would parse as malformed JSON anyway, and pretending otherwise hides the cause.

- [ ] **Step 4: Run the full suite and commit**

```bash
cargo fmt
git add native/src/peer_client/
git commit -m "feat(peer): parse the companion event stream"
```

---

### Task 4: The attachment

**Files:**
- Create: `native/src/peer_client/attach.rs`
- Test: inline

**Interfaces:**
- Produces: `attach::Attachment` — holds the latest `WireSnapshot` and a status; `attach::spawn(endpoint, session_id) -> Arc<Attachment>`; `Attachment::latest() -> Option<Arc<WireSnapshot>>`; `Attachment::activity(now) -> Activity`; `Attachment::send(bytes)`.

This is what Phase C2b will render. It owns a background thread, exactly like `blender.rs`'s poller, and holds only a `Weak` back-reference so it exits when the attachment is dropped.

**The property that matters most: staleness is not idleness.** If frames stop arriving — network drop, peer asleep, stream cut because sharing was revoked — `activity()` must report `Activity::Unknown`, never `Idle`. Slice 1 built that distinction precisely so a pane with no trustworthy signal cannot be mistaken for a finished job; this is the first code that can actually produce `Unknown` from a real cause.

- [ ] **Step 1: Write the failing tests**

Against a real in-process server:

- attaching to a shared session receives at least one snapshot
- `activity()` reflects the peer's reported activity while frames flow
- **`activity()` becomes `Unknown` once frames stop for longer than the staleness threshold** — drive this by stopping the server, and assert it does NOT become `Idle`
- `send()` delivers bytes that the server's hub actually receives
- dropping the `Attachment` ends its thread rather than leaking it
- attaching to a session the peer has not shared fails cleanly rather than hanging

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p superterminal-native peer_client::attach`
Expected: FAIL.

- [ ] **Step 3: Implement**

One thread per attachment. It connects, reads frames, and publishes each into a `Mutex<Option<Arc<WireSnapshot>>>` alongside the instant it arrived. `activity()` compares that instant against the threshold.

Reconnect policy: on a dropped stream, wait a bounded interval and try again — but do NOT retry hot, and give up after a bounded number of attempts rather than hammering a peer that is off. State the numbers you chose and why.

- [ ] **Step 4: Run the full suite and commit**

```bash
cargo fmt
git add native/src/peer_client/
git commit -m "feat(peer): attach to a shared session and track its staleness"
```

---

## Done criteria

- `cargo test` green from the repo root; `cargo fmt --check` clean; no new warnings.
- **No new crate.** No reqwest, hyper, ureq, tokio, or async runtime.
- Every pre-existing test passes with expected values unchanged — this phase adds a client and does not touch the server.
- A peer that accepts a connection and then sends nothing cannot hang the caller.
- Frames stopping produces `Activity::Unknown`, never `Idle`.
- The Codex gate passes on the full commit range before pushing.
