# Phone Companion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Embedded tailnet-only HTTP server + single phone page giving live read-mostly access to terminal sessions with quick input.

**Architecture:** Observer pattern — panes publish `Arc<RenderableSnapshot>`s into a shared `CompanionHub`; a plain-thread server (bounded workers, hand-rolled HTTP/1.1, SSE) serves them. Capability token in the URL fragment is the auth layer on top of Tailscale.

**Tech Stack:** Rust (std only — zero new dependencies), serde/serde_json (already workspace deps), one embedded HTML file.

**Spec:** `docs/superpowers/specs/2026-08-24-phone-companion-design.md` — every requirement there is binding; this plan implements it.

## Global Constraints

- Zero new dependencies (memory rule: fewer deps always better).
- No emoji anywhere in rendered UI (SVG/text only).
- TDD: every pure unit is test-first; run `cargo test --workspace` green before every commit.
- `cargo fmt` + clippy-clean before every commit; commit messages have NO attribution trailers.
- The hub mutex never guards serialization, `foreground_busy`, PTY sends, or socket I/O.
- PTY spawn stays main-thread-only (no session creation from the phone).
- Server binds ONLY to a `100.64.0.0/10` address (tests may use the loopback override constructor).

---

### Task 1: Wire format + snapshot serializer

**Files:**
- Create: `native/src/companion/mod.rs` (`pub mod wire; pub mod net; pub mod auth; pub mod http; pub mod input; pub mod hub; pub mod server;` — added as tasks land)
- Create: `native/src/companion/wire.rs`
- Modify: `native/src/main.rs` (add `mod companion;`), `native/src/pane.rs` (make `ansi_256` `pub(crate)`)

**Interfaces:**
- Produces: `wire::serialize_snapshot(&RenderableSnapshot, &Theme) -> WireSnapshot`;
  `WireSnapshot { cols: u16, lines: u16, cursor: Option<WireCursor>, app_cursor: bool, rows: Vec<Vec<WireRun>> }` (serde Serialize, camelCase);
  `WireCursor { col: u16, row: u16 }`;
  `WireRun { col: u16, width: u16, text: String, fg: String, bg: Option<String>, b: bool, i: bool, u: bool }`.

- [ ] **Step 1: Failing tests** in `wire.rs` `#[cfg(test)]`: helper `cell(ch, fg: CellColor, bg: CellColor, flags…) -> SnapshotCell`; tests:
  - `same_style_neighbors_merge_into_one_run` — 3 default-style cells → one run, `col=0,width=3,text="abc"`.
  - `inverse_swaps_resolved_colors_before_merge` — plain cell then inverse cell: two runs; inverse run's `fg` == resolved bg (theme background hex), `bg` == resolved fg.
  - `dim_darkens_and_hidden_blanks` — dim cell fg == 50% of theme.foreground channels; hidden cell renders `text:" "` with fg == bg.
  - `wide_glyph_spans_two_columns_spacer_skipped` — `世` + wide_spacer cell → one run `width=2, text="世"`; following cell has `col=2`.
  - `default_bg_serializes_as_none_and_rgb_as_hex` — `CellColor::Rgb(255,0,0)` → `"#ff0000"`.
  - `cursor_out_of_viewport_is_none` — `SnapshotCursor{row: None,..}` → `cursor: None`; hidden style → None.
  - `selection_and_search_are_not_on_the_wire` — snapshot with selection/search cells serializes identically to one without (local UI state stays local).
- [ ] **Step 2:** `cargo test -p superterminal-native wire` → all FAIL (todo!()).
- [ ] **Step 3: Implement.** Resolution first, merge second:
  ```rust
  fn resolve(style: &CellStyle, theme: &Theme) -> (u32, Option<u32>) {
      let mut fg = match style.fg { CellColor::Default => theme.foreground,
          CellColor::Indexed(i) => crate::pane::ansi_256(i, theme),
          CellColor::Rgb(r,g,b) => rgb_u32(r,g,b) };
      let mut bg = match style.bg { CellColor::Default => None, /* indexed/rgb as above */ };
      if style.dim { fg = halve_channels(fg); }
      if style.inverse { let old_fg = fg; fg = bg.unwrap_or(theme.background); bg = Some(old_fg); }
      if style.hidden { fg = bg.unwrap_or(theme.background); }
      (fg, bg)
  }
  ```
  Merge loop per row: skip `wide_spacer` cells (they extend the previous run's `width`); start a new run when resolved `(fg,bg,bold,italic,underline)` differs; hidden cells emit spaces. Hex via `format!("#{:06x}", v)`.
- [ ] **Step 4:** tests pass. **Step 5:** commit `feat(native): companion wire format + snapshot serializer`.

### Task 2: Tailnet interface detection

**Files:** Create `native/src/companion/net.rs`.

**Interfaces:**
- Produces: `net::Candidate { addr: Ipv4Addr, up: bool }`; `net::pick_tailnet(&[Candidate]) -> Option<Ipv4Addr>` (pure); `net::tailnet_ipv4() -> Option<Ipv4Addr>` (getifaddrs via libc extern-C, matching the repo's existing unsafe style, feeding pick_tailnet).

- [ ] **Step 1: Failing tests** for `pick_tailnet`: in-CGNAT up address wins; down interfaces skipped; non-CGNAT (192.168.x) never chosen; two candidates → lowest address wins deterministically; empty → None. (CGNAT test: `100.64.0.0/10` means 100.64.0.0–100.127.255.255; include a 100.128.x candidate that must be rejected.)
- [ ] **Step 2:** fail. **Step 3:** implement `pick_tailnet` (mask check `(octets[0]==100) && (64..128).contains(&octets[1])`), then `tailnet_ipv4()` with `getifaddrs`/`freeifaddrs` extern declarations, AF_INET filter, `IFF_UP` flag → Candidate list.
- [ ] **Step 4:** pass. **Step 5:** commit `feat(native): tailnet interface detection`.

### Task 3: Capability token

**Files:** Create `native/src/companion/auth.rs`. Modify `native/src/settings.rs` (+ its round-trip test): add `companion_token: Option<String>` (default None).

**Interfaces:**
- Produces: `auth::generate_token() -> String` (32 lowercase hex chars from /dev/urandom, 128 bits); `auth::token_matches(expected: &str, presented: &str) -> bool` (constant-time: XOR-accumulate over max-length, length folded in).

- [ ] **Step 1: Failing tests:** generated token is 32 chars, hex-only, two generations differ; `token_matches` true on equal, false on differing char / prefix / empty; settings round-trip preserves `companion_token`.
- [ ] **Step 2:** fail. **Step 3:** implement (`File::open("/dev/urandom")`, read 16 bytes; compare accumulates `diff |= a ^ b` over the longer length using 0 pads, plus length inequality).
- [ ] **Step 4:** pass. **Step 5:** commit `feat(native): companion capability token + setting`.

### Task 4: HTTP request parser

**Files:** Create `native/src/companion/http.rs`.

**Interfaces:**
- Produces:
  ```rust
  pub struct Request { pub method: Method, pub path: String,           // decoded, no query
                       pub query: Vec<(String,String)>, pub headers: Vec<(String,String)>, // lowercased names
                       pub body: Vec<u8> }
  pub enum Method { Get, Post }
  pub enum ParseError { BadRequest(&'static str), TooLarge, Unsupported }
  pub fn parse_request(reader: &mut impl std::io::BufRead) -> Result<Request, ParseError>
  ```
  Limits (consts): request line ≤ 2048B, headers ≤ 8192B total / 64 count, body ≤ 4096B, path segment (session id) ≤ 64B.

- [ ] **Step 1: Failing tests** (drive with `std::io::Cursor`): simple GET parses path+query; POST with exactly-one Content-Length yields body; rejects: missing/duplicate/conflicting Content-Length on POST, any `Transfer-Encoding`, oversized request line/headers/body, >64 headers, absolute-form target (`GET http://…`), `%zz` malformed percent-encoding, NUL/control in target, methods other than GET/POST (incl. OPTIONS → Unsupported), header without colon.
- [ ] **Step 2:** fail. **Step 3:** implement (read_line with cap, split request line, decode percent-escapes strictly, header loop with running byte budget, body via `take(len)` exact read).
- [ ] **Step 4:** pass. **Step 5:** commit `feat(native): companion http parser with hard limits`.

### Task 5: Symbolic input translation

**Files:** Create `native/src/companion/input.rs`.

**Interfaces:**
- Produces: `input::symbolic_bytes(key: &str, app_cursor: bool) -> Option<Vec<u8>>` for `enter, ctrl-c, tab, esc, up, down, left, right, y, n`; and `input::parse_body(body: &[u8]) -> Option<InputMsg>` where `InputMsg::Text(String) | InputMsg::Key(String)` (serde from `{"text":…}` / `{"key":…}`, rejecting both-or-neither).

- [ ] **Step 1: Failing tests:** arrows are `ESC [ A…D` normally and `ESC O A…D` with `app_cursor: true`; enter `\r`; ctrl-c `\x03`; tab `\t`; esc `\x1b`; unknown → None; body parse accepts each form, rejects `{}` and `{"text":"a","key":"up"}`.
- [ ] **Step 2:** fail. **Step 3:** implement. **Step 4:** pass. **Step 5:** commit `feat(native): companion symbolic input`.

### Task 6: Companion hub

**Files:** Create `native/src/companion/hub.rs`.

**Interfaces:**
- Produces:
  ```rust
  pub struct SessionInfo { pub id: String, pub label: String, pub alive: bool, pub busy: bool }
  pub struct Published { pub snapshot: Arc<RenderableSnapshot>, pub revision: u64,
                         pub info: SessionInfo, pub sender: EventLoopSender }
  pub struct CompanionHub { inner: Mutex<HashMap<String, Published>>, pub generation: AtomicU64,
                            cache: Mutex<HashMap<String,(u64, Arc<String>)>> }
  impl CompanionHub {
    pub fn publish_snapshot(&self, id: &str, snap: Arc<RenderableSnapshot>);   // bumps revision
    pub fn register(&self, id, label, sender); pub fn unregister(&self, id: &str);
    pub fn set_meta(&self, id, label, alive, busy);
    pub fn sessions(&self) -> Vec<SessionInfo>;
    pub fn snapshot_json(&self, id: &str, theme: &Theme) -> Option<(u64, Arc<String>)>; // serialize OUTSIDE inner lock, memoized by revision, cap 1MB -> Err marker json
    pub fn input_sender(&self, id: &str) -> Option<(bool /*alive*/, EventLoopSender)>;
    pub fn bump_generation(&self);                                             // server start: forces initial publish
  }
  ```
- [ ] **Step 1: Failing tests** (EventLoopSender is constructible only from a session — for hub tests define the sender generic: make `Published`/`CompanionHub` generic `CompanionHub<S: Clone>` with `type Hub = CompanionHub<EventLoopSender>` alias; tests instantiate `CompanionHub<std::sync::mpsc::Sender<Vec<u8>>>`): publish bumps revision; snapshot_json memoizes (same revision → same Arc ptr), re-serializes after publish; unregister removes; set_meta on missing id is a no-op; sessions() sorted by label then id; input_sender reflects alive flag.
- [ ] **Step 2:** fail. **Step 3:** implement with the lock rules from Global Constraints (snapshot_json: clone Arc + revision under `inner` lock, release, serialize, store under `cache` lock).
- [ ] **Step 4:** pass. **Step 5:** commit `feat(native): companion hub`.

### Task 7: Server

**Files:** Create `native/src/companion/server.rs`; test `native/tests/companion_server.rs`.

**Interfaces:**
- Consumes: everything from Tasks 1–6.
- Produces:
  ```rust
  pub struct ServerHandle { pub url: String, /* cancellation + join handles */ }
  pub struct ServerConfig { pub bind: SocketAddr, pub token: String, pub page: &'static str }
  pub fn start(hub: Arc<Hub>, theme: &'static Theme, cfg: ServerConfig) -> std::io::Result<ServerHandle>;
  impl ServerHandle { pub fn stop(self); }   // sets flag, self-connects acceptor, joins all workers
  pub fn start_on_tailnet(...) -> Result<ServerHandle, String>  // net::tailnet_ipv4 + port scan 43110..43120
  ```
  Routes/behavior exactly per spec §Server + §Security: token check first (404, constant-time), Host must equal cfg host, OPTIONS → 405-and-close, security headers on every response, caps MAX_CONNS=8 / MAX_SSE=4 (503 over limit), 10s read deadline, 10s SSE write timeout, SSE = initial full snapshot then dirty-driven with 200ms floor + 2s heartbeat `:hb`, `POST /input/:id` → 204 / 410 dead / 404 unknown, one plain request per connection.
- [ ] **Step 1: Failing integration tests** (loopback bind, `CompanionHub<mpsc::Sender<Vec<u8>>>`, a tiny blocking HTTP client helper in the test file): token missing/wrong → 404 with empty body; correct token `GET /sessions` → JSON with registered session; wrong Host → 400; OPTIONS → 405; `text/plain` POST with token header absent → 404; POST input `{"key":"up"}` → 204 and mpsc receives `\x1b[A` (and `\x1bOA` after publishing an app_cursor snapshot); dead session → 410; SSE: connect, assert immediate `data:` event containing serialized grid text, publish new snapshot → second event arrives; second SSE client works while `/sessions` still answers (worker pool); 9th concurrent connection → 503; stop() joins within 5s while an SSE client is attached.
- [ ] **Step 2:** fail (module absent). **Step 3:** implement acceptor thread + `thread::spawn` per connection guarded by an `Arc<Semaphore-like AtomicUsize>` count, `TcpStream::set_read_timeout`/`set_write_timeout`, SSE loop polling hub revision every 50ms (send when revision changed AND ≥200ms since last send; heartbeat when ≥2s idle), cancellation check every loop.
- [ ] **Step 4:** pass. **Step 5:** commit `feat(native): companion server`.

### Task 8: Pane publish hook + snapshot cache refactor

**Files:** Modify `native/src/pane.rs` (pump at 137–263, render at ~803–829, shutdown at ~387).

**Interfaces:**
- Consumes: `CompanionHub::publish_snapshot`, `unregister`, `generation`.
- Produces: `TerminalPane::set_companion(hub: Option<Arc<Hub>>)`; pane field `companion: Option<Arc<Hub>>`, `companion_generation: u64`.

- [ ] **Step 1:** In the pump closure, after the existing `take_dirty()` branch: when `pane.companion` is Some and (dirty fired OR `hub.generation` > `pane.companion_generation`), call `session.sync_and_snapshot()`, store into `pane.snapshot`, `hub.publish_snapshot(&pane.id, Arc::new(pane.snapshot.clone()))`, update `companion_generation`, `cx.notify()`. Render keeps its own `sync_and_snapshot()` call ONLY when `session.has_pending_ops()` (add that cheap accessor to `TermSession`: `!self.deferred.is_empty()`) or when no companion sync happened this frame — implement as: render syncs unconditionally IF `companion.is_none()` (today's behavior), else consumes the cache unless `has_pending_ops()`.
- [ ] **Step 2:** `pane.shutdown()` additionally calls `hub.unregister(&self.id)` when companion is set.
- [ ] **Step 3:** `cargo test --workspace` (pty_roundtrip + all) green; `cargo build` clean; manual smoke: app still renders/selects/resizes normally with companion off (companion None = exactly today's paths).
- [ ] **Step 4:** commit `feat(native): pane publish hook for companion`.

### Task 9: Workspace toggle + metadata + shutdown integration

**Files:** Modify `native/src/workspace.rs`, `native/src/main.rs` (quit path).

**Interfaces:**
- Consumes: `server::start_on_tailnet`, `ServerHandle::stop`, `hub.set_meta/register/bump_generation`, `auth::generate_token`, settings `companion_token`.
- Produces: workspace fields `companion_hub: Option<Arc<Hub>>`, `companion_server: Option<ServerHandle>`, `companion_error: Option<String>`.

- [ ] **Step 1:** Toggle in the bottom bar (pattern: keep-awake toggle from commit f202cb9): off→on: ensure token (generate + save settings once), build hub, register every live pane (id, label from tab data — multi-pane tabs get "label · n", sender via `pane.session.input_sender()`), `bump_generation()`, `set_companion(Some(hub))` on every pane, `start_on_tailnet` → store handle or `companion_error`; on→off: `set_companion(None)` everywhere, `handle.stop()` off the UI thread, clear hub. Bar shows the URL (`handle.url` + `#token`) as selectable text or the error.
- [ ] **Step 2:** Metadata freshness: in the existing ~900ms tick, when server running: `set_meta` per pane (label refresh, alive=true, busy=`foreground_busy()`); pane close/exit paths call `unregister` + `set_meta(alive=false)` first; new panes created while running get `set_companion` + `register`.
- [ ] **Step 3:** Quit path: in `shutdown_all` BEFORE pane teardown: if server running, stop it (join off UI thread with the existing bounded-deadline pattern).
- [ ] **Step 4:** build + full tests green; manual: toggle on shows URL, off stops. **Step 5:** commit `feat(native): companion toggle, metadata, shutdown`.

### Task 10: Phone page + end-to-end test

**Files:** Create `native/src/companion/page.html` (embedded via `include_str!` from server.rs `GET /`); test `native/tests/companion_e2e.rs`.

- [ ] **Step 1: Page** (single file, no external resources, CSP-compatible inline style+script): reads `location.hash` token (absent → "bad link" screen); fetches `/sessions?t=` → list (label + busy dot, SVG circle); tapping opens stream view: `EventSource('/stream/'+id+'?t='+t)`, renders `WireSnapshot` into a `<pre>`-based fixed grid — one absolutely-positioned `span` per run at `left: col*ch; width: width*ch` so phone fonts can't drift columns; cursor as an outlined span at `cursor.col/row`; quick-input row buttons POST `{"key":…}` with `X-Companion-Token` header and `Content-Type: application/companion-input`; text box POSTs `{"text": value + "\r"}`; connection state dot driven by EventSource open/error (auto-reconnect is native). Dark palette, 44px touch targets, no emoji.
- [ ] **Step 2: E2E test** (real `TermSession::spawn` on the test thread — mirrors `pty_roundtrip.rs` — real hub with `EventLoopSender`): loopback server; `printf 'E2E_%s\n' OK` via `POST /input {"text":…}`; poll SSE events until a snapshot's runs contain `E2E_OK`; assert `/sessions` shows the session; kill session → next input 410 and stream ends.
- [ ] **Step 3:** fail→implement→pass. **Step 4:** commit `feat(native): companion phone page + e2e`.

### Task 11: Ship gate

- [ ] `cargo fmt` / clippy clean; `cargo test --workspace` all green.
- [ ] `sh native/bundle.sh` + ditto install to /Applications.
- [ ] Manual Pixel pass per spec §Testing (bookmark, quick inputs against claude, lock/unlock reconnect, LTE) — coordinate with Tomas for the phone half.
- [ ] Codex pre-push gate per the codex skill (pinned SHAs, detached run, full verdict); fix-and-regate loop until explicit clean verdict; push.

## Self-review

- Spec coverage: security model → T3/T4/T7; publish pipeline → T6/T8; server behaviors → T7; wire → T1; input → T5/T7; page → T10; toggle/metadata/shutdown → T9; tailnet detection → T2; testing matrix → distributed across task tests + T10 e2e + T11 manual. Interface-loss detection (spec §Server last bullet) folded into T9 Step 2: the ~900ms tick also re-checks `net::tailnet_ipv4()`; on loss, stop server + set `companion_error`.
- Placeholders: none — every step names concrete behavior, code, or an exact spec section.
- Type consistency: `Hub = CompanionHub<EventLoopSender>` used in T7–T10; `snapshot_json` returns `(u64, Arc<String>)` everywhere; `WireRun.bg: Option<String>` (None = page uses its background).
