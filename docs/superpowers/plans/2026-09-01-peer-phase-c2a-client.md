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

Check every field. `history` is the only one the server skips when empty
(`wire.rs:26`), so it needs `#[serde(default)]` alongside its existing
`skip_serializing_if`. `Option` fields such as `cursor` and a run's `bg` already
deserialize an absent value as `None`. The two mode booleans are always
serialized and should stay required — a missing `bracketedPaste` means the peer
is running a build that predates it, and defaulting that to `false` would
silently mis-handle paste rather than failing loudly.

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

### Task 2: A bounded ONE-SHOT HTTP client

**Files:**
- Create: `native/src/peer_client/mod.rs` (and `mod peer_client;` in `native/src/main.rs`)
- Test: inline

**Interfaces:**
- Produces: `peer_client::Endpoint { addr: SocketAddr, secret: String }`; `peer_client::get(&Endpoint, path: &str, deadline: Duration) -> Result<Vec<u8>, PeerError>`; `peer_client::post(&Endpoint, path: &str, body: &[u8], deadline: Duration) -> Result<(), PeerError>`; `peer_client::PeerError`.

**Scope: one-shot requests only** — `/sessions` and `/peer-input/<id>`. A TOTAL
round-trip deadline is correct for these and is what `blender.rs:55` does.
It is NOT correct for `/stream/<id>`, which the server deliberately keeps open
forever; that is Task 4 and it has different rules. Do not reuse this function
for the stream.

Hand-rolled, matching the server's own HTTP handling. The secret may go in the
`X-Companion-Token` header OR the `?t=` query parameter — `token_of`
(`server.rs:421`) accepts either. Use the header for one-shot requests. Note the
phone is NOT a single precedent here: its `fetch` calls use the header, but
`EventSource` cannot set headers so its streams use `?t=`. Do not describe the
header as "what the phone does"; it is what the phone does for half its traffic.

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

### Task 4: A stream connection, bounded by liveness rather than duration

**Files:**
- Create: `native/src/peer_client/stream.rs`
- Test: inline

**Interfaces:**
- Produces: `stream::open(&Endpoint, session_id, connect_deadline) -> Result<StreamConn, PeerError>`; `StreamConn::next_frame(idle_gap) -> Result<WireSnapshot, PeerError>`.

**This is where the plan was originally wrong and it matters.** Task 2's total
round-trip deadline is right for a one-shot request and WRONG here: the server
holds `/stream/<id>` open indefinitely by design, sending snapshots plus a `:hb`
heartbeat every 2 seconds (`SSE_HEARTBEAT`, `server.rs:931`). A total deadline
would kill a perfectly healthy stream.

Three separate bounds, each with a different job:

- **Connect + headers: `CONNECT_DEADLINE` (5s), total.** A peer that accepts the
  TCP connection and then sends nothing must fail HERE. This is the hang the
  original plan cared about, and it belongs at stream-open, not on the stream.
- **Frame and line caps** (reuse Task 3's): memory safety, unchanged.
- **Idle gap: `IDLE_GAP` (6s), rolling.** Once established, the stream is healthy
  as long as SOMETHING arrives — a frame or a heartbeat — within the gap. Six
  seconds is three heartbeat intervals: tight enough to notice a dead peer
  quickly, loose enough that one dropped heartbeat on a slow link does not flap.
  Exceeding it is an error, not a silent stall.

Tests: a peer that accepts and sends nothing fails at `CONNECT_DEADLINE`; a
stream that delivers a frame then goes silent fails after `IDLE_GAP` and not
before; heartbeats alone keep a stream alive indefinitely (drive at least two
gaps' worth); a frame arriving keeps it alive.

- [ ] **Step 1: Write the failing tests**
- [ ] **Step 2: Run them and observe the failure**
- [ ] **Step 3: Implement, with the three constants named and documented**
- [ ] **Step 4: `cargo test` from the repo root, then commit**

```bash
cargo fmt
git add native/src/peer_client/
git commit -m "feat(peer): stream connections bounded by liveness, not by duration"
```

---

### Task 5: The attachment

**Files:**
- Create: `native/src/peer_client/attach.rs`
- Test: inline

**Interfaces:**
- Produces: `attach::Freshness { Fresh, Stale }`; `attach::Status { Connecting, Live, Refused, Gone, Unavailable }`; `attach::Attachment` with `latest() -> Option<Arc<WireSnapshot>>`, `freshness(now) -> Freshness`, `status() -> Status`, `send(bytes)`; `attach::spawn(endpoint, session_id) -> Arc<Attachment>`.

**A contract the original plan got wrong, corrected here.** It claimed
`activity()` would report the peer's activity. It cannot: `WireSnapshot` carries
geometry, rows, cursor and the two modes — NOT activity. Activity is reported by
`/sessions` as a string (`server.rs:533`), on a different endpoint.

So the attachment does NOT report activity. It reports **freshness**: are frames
still arriving. Activity comes from the peer's session list, which the OWNER
polls once per peer rather than once per attachment — polling `/sessions` per
attachment would multiply requests by the number of open panes for data that is
identical across all of them.

Phase C2b combines the two: **stale attachment wins.** If frames have stopped,
the pane reports `Activity::Unknown` regardless of what the last session list
said, because a cached "busy" from thirty seconds ago is exactly the stale signal
`Unknown` exists to represent. Write that rule down here so C2b inherits it
rather than inventing it.

**Status states, each with a defined cause:**

- `Connecting` — spawned, no successful stream yet.
- `Live` — a stream is open and within `IDLE_GAP`.
- `Refused` — the peer answered 404. The session is not shared with us (or does
  not exist; the server deliberately does not distinguish those, so neither can we).
- `Gone` — the peer answered 410, or the stream ended cleanly. The session is over.
- `Unavailable` — connect failed, or reconnect attempts were exhausted.

`Refused` and `Gone` are terminal: do not reconnect. Retrying a 404 in a loop is
how you turn a revoked share into a request flood against someone else's machine.

**Reconnect policy, fixed here rather than left to judgement:** on an unexpected
drop, wait `RECONNECT_DELAY` (2s) and retry, up to `MAX_RECONNECTS` (5), then
settle on `Unavailable` and stop. No exponential backoff — five attempts over ten
seconds is enough to ride out a wifi blip, and a peer that is off should not be
polled forever by a pane the user has forgotten about.

- [ ] **Step 1: Write the failing tests**

Against a real in-process server (the harness in `e2e_tests.rs` starts one on
`127.0.0.1:0`, configures `PeerRecord`s, and shares sessions with
`hub::set_visible_to` — "pairing" here means constructing a configured record,
not driving the settings UI):

- attaching to a shared session reaches `Live` and receives at least one snapshot
- **`freshness()` becomes `Stale` once frames stop past `IDLE_GAP`, and never
  reports `Fresh` again without a new frame** — drive it by stopping the server
- attaching to a session NOT shared with this peer reaches `Refused` and does not
  reconnect (assert the attempt count does not climb)
- `send()` delivers bytes the server's hub actually receives
- dropping the `Attachment` ends its thread rather than leaking it
- reconnect gives up at `MAX_RECONNECTS` and settles on `Unavailable`

- [ ] **Step 2: Run them and observe the failure**
- [ ] **Step 3: Implement**

One thread per attachment, holding a `Weak` back-reference so it exits when the
attachment is dropped — the `blender.rs:30` lifecycle pattern.

- [ ] **Step 4: `cargo test` from the repo root, then commit**

```bash
cargo fmt
git add native/src/peer_client/
git commit -m "feat(peer): attach to a shared session, tracking freshness and status"
```

---

## Done criteria

- `cargo test` green from the repo root; `cargo fmt --check` clean; no new warnings.
- **No new crate.** No reqwest, hyper, ureq, tokio, or async runtime.
- Every pre-existing test passes with expected values unchanged — this phase adds a client and does not touch the server.
- A peer that accepts a connection and then sends nothing fails at
  `CONNECT_DEADLINE`, on both the one-shot and the stream path.
- A healthy stream carrying only heartbeats survives indefinitely; a silent one
  fails at `IDLE_GAP`.
- Frames stopping produces `Freshness::Stale`, which C2b maps to
  `Activity::Unknown` — never `Idle`.
- `Refused` and `Gone` do not reconnect.
- The Codex gate passes on the full commit range before pushing.
