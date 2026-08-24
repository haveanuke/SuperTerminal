# Phone Companion over Tailscale — Design Spec

**Date:** 2026-08-24
**Status:** Approved to build (Tomas delegated approval to Claude + Codex;
Codex design review 2026-08-24 conditionally approved — every condition is
folded in below).
**Precursor:** `docs/superpowers/research/2026-08-21-remote-access-research.md`

## Scope

A phone-first companion for the native app: open a bookmarked page on a phone
in the same tailnet, see live output of any terminal session, send quick
input. Read-mostly monitoring, not a full terminal.

**Non-goals (v1 cuts, confirmed safe):** scrollback paging, creating sessions
from the phone, QR code, second-Mac native attach, off-tailnet access, TLS.

## Security model

Network reachability, wire encryption, and device identity come from
Tailscale (bind ONLY to a `100.64.0.0/10` interface address; refuse to start
without one). Application-level authorization comes from a capability token,
because tailnet membership does not authenticate the *browser page* making a
request (cross-origin `text/plain` POSTs are sent without preflight).

- 128-bit random token, generated once, persisted in settings (regenerable
  from the toggle; regeneration invalidates old bookmarks).
- The advertised URL is `http://<tailnet-ip>:<port>/#<token>` — the fragment
  never appears in HTTP requests or referrers. The page reads it from
  `location.hash` and supplies it on every API call:
  - `GET /sessions?t=<token>` and `GET /stream/<id>?t=<token>` (EventSource
    cannot set headers),
  - `POST /input/<id>` with header `X-Companion-Token: <token>` and
    `Content-Type: application/companion-input` (non-safelisted → a browser
    cannot send it cross-origin without a preflight, which we reject).
- Server rejects: any request with a missing/wrong token (404, constant-time
  compare), all `OPTIONS`, any `POST` whose `Origin` is present and not the
  companion's exact origin, any request whose `Host` is not exactly the bound
  `ip:port` (closes DNS rebinding).
- No CORS allow headers, ever. Response headers on everything:
  `Referrer-Policy: no-referrer`, `Cross-Origin-Resource-Policy: same-origin`,
  `X-Content-Type-Options: nosniff`, and a CSP allowing only inline resources
  of the embedded page (`default-src 'none'; style-src 'unsafe-inline';
  script-src 'unsafe-inline'; connect-src 'self'`).

## Publish pipeline (native/src/pane.rs, workspace.rs)

Snapshot sync moves from render-time to an explicit pane operation so hidden
panes publish and render cost is unchanged:

- Pane holds `published: Option<Arc<CompanionHub>>` (None when server off —
  zero cost).
- In the 16ms pump: when publishing is enabled AND (`take_dirty()` fired OR
  the hub's initial-publish generation is newer than the pane's), call
  `sync_and_snapshot()` on the gpui thread, store the snapshot in the pane's
  cache, and swap an `Arc<RenderableSnapshot>` into the hub. Render consumes
  the cached snapshot and only forces a sync itself when it has queued UI ops
  (resize, selection) — preserving the one-lock-owner-per-frame contract.
- Lock discipline (hard rules): the hub mutex only ever guards map get/insert
  and `Arc` swaps. No serialization, no `foreground_busy`, no PTY sends, no
  socket I/O under it. Input senders are cloned out under the lock, used
  after release.

`Workspace` owns companion metadata:

- On server start: registers every live pane (id, label, input sender) and
  bumps the initial-publish generation so idle sessions populate immediately.
- Labels come from tabs (`layout.rs` data); multiple panes under one tab get
  "label · n". Rename/load/close/move refresh the metadata map.
- Every pane-shutdown path unregisters from the hub (mirroring the existing
  broadcast unregister). `alive` flips false rather than the entry vanishing
  mid-request; entries are removed on the next metadata refresh.
- `busy` is sampled by the workspace on its existing ~900ms cadence, only
  while the server runs, and cached into the hub.

## Server (native/src/companion.rs)

Hand-rolled HTTP/1.1 on `std::net::TcpListener`, zero new dependencies.

- One acceptor thread + per-connection worker threads, bounded: max 8
  concurrent connections, max 4 of them SSE streams; over-limit connections
  get 503 and close. Request read deadline 10s; SSE write timeout 10s.
- Shared `Arc<AtomicBool>` cancellation. Toggle-off / app quit: set the flag,
  unblock the acceptor (self-connect), close streams, join workers off the
  UI thread — integrated BEFORE pane shutdown in the quit path.
- One ordinary request per connection, then close. SSE connections live until
  client drop, session death, write failure/timeout, or cancellation.
- SSE stream: on connect, send the full latest snapshot immediately (browser
  `EventSource` auto-reconnect then always recovers with a complete state —
  no delta protocol); afterwards dirty-driven sends with a 200ms floor and a
  2s heartbeat comment.
- Parser limits: request line ≤ 2KB, headers ≤ 8KB total / 64 count, body ≤
  4KB, session id ≤ 64 bytes. Exactly one valid `Content-Length` on POST;
  reject `Transfer-Encoding`, duplicate/conflicting lengths, absolute-form
  targets, malformed percent-encoding, NUL/control bytes in targets,
  unsupported methods.
- Serialized snapshots are cached per (session, revision) so N phones don't
  re-serialize the same grid; serialized size capped (1MB → stream sends a
  "grid too large" event instead).
- If the bound tailnet address disappears (interface loss), the server stops
  and the toggle reflects it with a reason — never "on" while unreachable.

## Amendments (implementation review, 2026-08-24)

- **Selection and search highlighting stay OFF the wire** (supersedes the
  earlier "selection resolved host-side" condition): the host's local text
  selection is UI state of the Mac, not terminal content — mirroring it to
  the phone adds noise and forces republish-on-mouse-move. Cells are
  resolved for inverse/dim/hidden only; a unit test pins selection's
  absence. Corollary: render-time-only changes (selection, host scrollback
  browsing via display_offset) are not republished — the companion mirrors
  live PTY output, not the host's viewport browsing.
- **Pane closure semantics:** pane shutdown RETIRES its hub entry
  (alive=false → input answers 410); the workspace's ~900ms sweep then
  unregisters entries with no live pane, which ends their streams and turns
  further input into 404.
- **Gate order:** capability token is validated first (constant-time, 404)
  on all protected routes; exact-Host validation second; the static page is
  the only tokenless route and is Host-checked.

## Wire format (native, serde JSON)

- `Snapshot { cols, lines, cursor: {col, row, visible}, app_cursor: bool,
  rows: [[Run]] }`
- `Run { col: u16, width: u16, text: String, fg: "#rrggbb", bg: "#rrggbb",
  b/i/u/s flags }` — colors are RESOLVED RGB (theme palette, inverse, dim,
  hidden, selection all applied host-side before merging; cells differing in
  those attributes never merge unless resolution made them identical). `col`
  and `width` are explicit so phone-side Unicode width can never drift the
  grid; the page positions runs on a fixed grid, not by text flow.
- `/sessions`: `[{id, label, alive, busy}]`.

## Input (mode-aware)

`POST /input/<id>` body is JSON: `{"text": "..."}` for literal text (sent as
UTF-8 bytes) or `{"key": "enter|ctrl-c|up|down|left|right|tab|esc"}` for
symbolic keys, translated server-side by the existing gpui-free `keys.rs`
logic honoring the session's current `app_cursor_mode`. Dead session → 410.

## Phone page (single embedded HTML file)

Session list (label, busy dot) → session view: fixed-grid render of styled
runs, cursor block, connection state, quick-input row (Enter, Ctrl+C, arrows,
Tab, Esc, y/n, text box + send). Dark theme, SVG icons only, thumb-sized
targets. Reads token from `location.hash`; shows a clear "bad link" state if
absent/rejected.

## Bottom-bar toggle

Same pattern as keep-awake: off → starts server (picks port 43110, falling
back upward), shows `http://100.x.x.x:port/#token` as selectable text; on →
stops it. Failure states render the reason ("no Tailscale interface").
Setting `companion_token` persists; `companion_enabled` is NOT persisted (the
server never auto-starts on launch).

## Testing

Pure/unit (native): run coalescing + resolution (inverse/dim/hidden/wide
cells, column arithmetic), HTTP parser (each rejection rule), token compare,
symbolic-key translation incl. app-cursor mode, tailnet address selection
from synthetic interface records (CGNAT candidates, down flags).

Integration (native/tests, loopback bind override): headless TermSession
end-to-end — sessions list, SSE snapshot, text + symbolic input round trip,
initial publish of an idle session, two simultaneous SSE clients with
/input still responsive, blocked-writer stream teardown, reconnect gets full
snapshot, pane closure mid-stream (410 + stream end), toggle-off joins
workers, oversized/conflicting-length/malformed requests rejected, wrong
token/Origin/Host/OPTIONS rejected, text/plain cross-origin-style POST
rejected.

Manual (Pixel): bookmark flow, quick inputs against a claude session, phone
lock/unlock reconnect, LTE (off home Wi-Fi) reachability.

## Module layout

- `native/src/companion.rs` — hub types, server, parser, wire serializer,
  token, tailnet detection (submodules if it grows past ~800 lines).
- `pane.rs` — pump publish hook + snapshot-cache refactor.
- `workspace.rs` — toggle UI, metadata registration, busy sampling, shutdown
  integration.
- `settings.rs` — `companion_token: Option<String>`.
