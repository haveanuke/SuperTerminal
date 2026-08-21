# Remote Access to Terminals — Feasibility Research

**Date:** 2026-08-21
**Status:** Deferred — assessed, not started. Blocked on nothing technical; parked until Tomas has tried Tailscale.
**Verdict:** Easy for the chosen scope (phone-first companion over Tailscale): a few days of work. Full remote access (second Mac, grid negotiation, full client-side emulation) would be 2–4 weeks and is explicitly out of scope here.

## Chosen scope

A **phone-first companion**: open a page on the phone, see live terminal output for any session, send quick input (Enter, Ctrl+C, arrows, y/n, a text box). Read-mostly monitoring — not a full terminal on the phone.

Network path is **Tailscale only**. The app binds its server to the tailnet IP, so reachability, encryption, and authentication all come from the VPN. No TLS, no login page, no port forwarding.

## Why the codebase cooperates

These findings are from a full architecture pass on 2026-08-21 (all against the native app):

- **The terminal core is already headless.** `native/src/term_session.rs` is documented "no gpui" and exposes exactly what a server needs: `sync_and_snapshot()` returns `RenderableSnapshot` — a plain owned struct of `Vec<Vec<SnapshotCell>>` + cursor + modes. It derives `Clone` but not `Serialize`; adding serde derives is mechanical.
- **Multi-writer input already works in production.** The broadcast feature (`BroadcastHub`, `pane.rs:20-46`) keeps a global `Mutex<HashMap<String, (bool, EventLoopSender)>>` of cloned input senders. `TermSession::input_sender()` (`term_session.rs:377-379`) hands out clonable senders. Remote keystrokes ride this existing mechanism — zero changes to the terminal core.
- **`keys.rs` (506 lines) is deliberately gpui-free** — reusable if the phone page ever needs real key translation beyond canned buttons.
- **`layout.rs` is pure serde data** with a stable camelCase JSON schema — tab labels there give sessions human-readable names for the session list.
- **No damage tracking needed at this scope.** `sync_and_snapshot` copies the whole viewport (alacritty damage events are discarded at `term_session.rs:225-227`). Fine here: a throttled full snapshot (a few Hz) is tens of KB — trivial over WireGuard. Diffing is an optimization for later, not a prerequisite.
- **Resize never comes up.** The phone shows the host's grid as-is (tmux-attach semantics). The resize-authority question — whose font metrics decide the grid (`pane.rs:459-539`, debounce at `pane.rs:147-185`) — is the genuinely hard design problem in *full* remote access, and this scope sidesteps it entirely.

## Recommended shape

1. **Publish hook** (~50–100 lines). Each pane's existing 16ms pump loop (`pane.rs:137-263`) already tracks dirtiness. When a remote viewer is attached, it drops the latest snapshot into a shared `Arc<Mutex<HashMap<String, Snapshot>>>` and registers its input sender there. This *avoids* extracting a `SessionRegistry` out of `workspace.rs` (4,295 lines) — the companion observes; it doesn't own.
2. **Embedded server** (~300–500 lines, hand-rolled on `std::net::TcpListener`, zero new dependencies). Endpoints:
   - `GET /` → one embedded HTML page (`include_str!`)
   - `GET /sessions` → JSON list (id + tab label + alive)
   - `GET /stream/:id` → **Server-Sent Events** pushing snapshots (SSE, not WebSocket: no handshake/framing code, works in every mobile browser)
   - `POST /input/:id` → bytes to the session via its cloned input sender
   - Runs on a plain thread or gpui's background executor (smol; already used for every timer — tokio not required).
3. **Phone page** (one self-contained HTML file). Renders the grid as styled rows of spans — no xterm.js, the host already did the emulation. Quick-input row underneath. Follows the repo icon set (`icons.tsx` convention: SVG, no emoji).
4. **Bottom-bar toggle** (same pattern as the keep-awake toggle, commit `f202cb9`) that starts/stops the server and shows the tailnet URL.

### v1 cuts

- Viewport only — no scrollback paging from the phone.
- No creating sessions from the phone (PTY spawn is main-thread-only per contract rev 2 §4, `term_session.rs:255-257`; would need a main-thread hop — defer).
- Server refuses to start if no Tailscale interface is found (no LAN fallback, no auth token to build).

### Open decisions for when this resumes

- Snapshot wire format: raw cells vs. coalescing styled runs server-side (the local renderer already coalesces by fg/bg/bold/italic/underline, `pane.rs:829-921` — reuse that idea to shrink payloads).
- Update cadence: dirty-flag-driven with a floor (e.g. min 200ms between sends) is probably right.
- Whether the toggle shows a QR code for the URL (nice, but the tailnet URL is stable enough to bookmark).

## Tailscale primer (for first-time setup)

Tailscale is a zero-config WireGuard mesh VPN. Each device you enroll gets a stable private IP in `100.64.0.0/10` (the "tailnet"), reachable from your other enrolled devices from anywhere — home Wi-Fi, LTE, hotel networks — with the traffic end-to-end encrypted. Nothing is exposed to the public internet.

Setup is genuinely small:

1. Mac: `brew install --cask tailscale` (or the Mac App Store app), sign in (Google/GitHub/Apple account works), toggle on.
2. Phone: install the Tailscale app from the App Store, sign in with the same account, toggle the VPN on.
3. That's it — the Mac now has a `100.x.x.x` address (visible in the Tailscale menu bar item) the phone can reach. `MagicDNS` (on by default) also gives it a name like `your-mac.tailnet-name.ts.net`.

Free tier covers personal use (up to 100 devices, 3 users). Why it matters for this feature: the companion server can skip TLS certificates, login pages, and token management entirely — binding to the tailnet IP means only your enrolled devices can connect, and WireGuard encrypts the wire. That is most of the reason the estimate is days instead of weeks.

## What full remote access would add (out of scope, for the record)

The 2–4 week version — a second Mac attaching natively, or a full interactive web terminal — needs: extracting session ownership from `workspace.rs` into a real `SessionRegistry` (pump included, so sessions outlive views), damage/diff plumbing instead of full-viewport copies, a resize-authority design (host-wins vs. smallest-viewer negotiation against the SIGWINCH debounce), main-thread spawn routing for remote session creation, and real auth/TLS the moment anything is reachable off-tailnet. None of it blocks the companion; all of it layers on top if the companion proves the concept.
