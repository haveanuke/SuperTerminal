# Native v1 — Architecture Contract Sheet

Binding interface/naming contract for plan drafting and implementation. Resolves the research critique's gaps as decisions. Read together with `2026-08-19-native-brief.md` (API recipes) and `../specs/2026-08-19-native-rewrite-design.md` (scope/design).

## Crate layout

- `core/` = `superterminal-core` (lib): `session.rs`, `shell_env.rs`, `proc_cwd.rs`, `git/` — moved from src-tauri per the extraction map; src-tauri keeps thin `#[tauri::command]` wrappers.
- `native/` = `superterminal-native` (bin): `main.rs`, `terminal/mod.rs` (engine entity), `terminal/element.rs` (custom Element), `terminal/keys.rs` (input translation), `workspace.rs` (tabs + pane tree), `ui/bar.rs` (tmux bar), `ui/overlay.rs` (pane hover overlay + theme picker), `themes.rs`, `settings.rs`.
- Workspace: per the research recommendation (root virtual workspace with members `src-tauri`, `core`, `native`) — plan Task 0 verifies `tauri dev/build` still works and adjusts CI.

## Pins (Task 0 records them)

- `gpui = "=0.2.2"` (crates.io), `alacritty_terminal = "=0.26.0"`, Rust ≥ 1.85, real Xcode required.
- **Zed reference rev**: Task 0 finds the zed commit where `crates/gpui/Cargo.toml` version == 0.2.2, records it in `native/README.md`; all "port from Zed" references use that rev.

## Entity architecture (gap 4)

Zed-style split:
- `Terminal` entity: owns `Arc<FairMutex<Term<Listener>>>`, `Notifier`, the event-pump task (detached, entity-scoped), current `TermSize`, title, last-known cwd. API: `new(cx, cwd: Option<PathBuf>) -> Entity<Terminal>`, `write(&self, bytes)`, `resize(&mut self, size, cx)`, `snapshot(&self) -> RenderableSnapshot` (grid cells + cursor + selection copied under one short lock), `shutdown(&mut self)`.
- `TerminalPane` view entity: `FocusHandle`, `Entity<Terminal>`, hover state for the overlay; renders `TerminalElement` (captures the `Entity<Terminal>` handle at render; reads the snapshot in prepaint).
- `Workspace` root view: `tabs: Vec<Tab>` where `Tab { id, label, pane: PaneNode }`, `PaneNode = Terminal(Entity<TerminalPane>) | Split { axis, children, ratios }` — direct port of the TS `PaneNode` model; same session JSON schema via `superterminal-core::session`.

## PTY bootstrap (gap 5)

Spawn at 80×24 with a pixel-size estimate; first element layout calls `resize` with real cell metrics. `window_id = 0`. `tty::Options { shell: None, working_directory: <inherited-or-None>, env: superterminal-core shell_env capture + TERM handled by setup_env, drain_on_exit: true }`. `setup_env()` once at startup.

## Fonts (gap 3)

`text_system().resolve_font(&Font { family: settings.font_family, ... })`; default family "JetBrains Mono" with fallback chain to "Menlo"; size default 14, line-height ×1.3 rounded; per-cell bold/italic map to `FontWeight::BOLD` / `FontStyle::Italic` in `TextRun`s. Nerd-glyph fallback = gpui default behavior (accepted risk, verified in spike/milestone 1).

## Themes & settings (gap 6)

`themes.rs`: all presets from `src/renderer/stores/theme-presets.ts` transliterated (same `ThemeConfig` fields: 16 ANSI + fg/bg/cursor/selection + ui tokens). 256-color: standard xterm cube/grayscale computed; true-color passthrough. `settings.rs`: `settings.json` in `~/Library/Application Support/com.tomaspinal.superterminal.native/` — `{ theme, fontSize, fontFamily }`, loaded at boot, written by the theme picker. `option_as_meta = true`.

## Input (from existing app, `xterm-registry.ts`)

`terminal/keys.rs` ports: Cmd+Backspace→0x15, Cmd+←/→→0x01/0x05, Cmd+Delete→0x0b, Cmd+Enter→ESC CR, Alt+Backspace/Delete/←/→→ESC-prefixed; plain-click + Option+Click cursor move with the same guards (cursor row only, normal buffer, at bottom, no selection, no mouse-tracking mode); Cmd+1..9/T/W tab & pane management via gpui actions/keymap.

## Shutdown & child exit (gap 8)

Window-close / app-quit hook (exact 0.2.2 API verified in Task 0: `on_window_closed` / `on_app_quit` family) → each `Terminal` sends `Msg::Shutdown`, joins the IO thread. `Event::ChildExit`/`Exit` → pane renders "[process exited]" and closes on next keypress; last pane closing closes the tab; last tab keeps an empty-state pane.

## Element trait (gap 7)

`terminal/element.rs` implements gpui 0.2.2 `Element` (exact associated types/method signatures verified against docs.rs/gpui/0.2.2 and the pinned zed rev's `terminal_element.rs` in Task 0 and recorded in the plan before coding).

---

# Rev 2 — Codex review resolutions (BINDING; override rev 1 where they conflict)

1. **One threading interface** (replaces rev-1 `resize()`/`snapshot()`): `Terminal` exposes `write(bytes)`, `queue_resize(TermSize, WindowSize)`, `queue_scroll(delta)`, `set_selection(Option<Selection>)` — all deferred ops — and `sync_and_snapshot() -> RenderableSnapshot`, called once per frame: drains deferred ops, takes the FairMutex once, applies ops, snapshots (cells, cursor, display offset, selection), releases. No other method touches the lock from the UI thread.
2. **Producer-side wake coalescing:** `EventProxy` holds `dirty: Arc<AtomicBool>`; on `Wakeup` it sends a channel message only on false→true transition; the pump stores `false` before syncing. Bounded queue under output floods (spike-proven dirty-flag pattern + channel wake). Non-Wakeup events (Title, Bell, ChildExit, PtyWrite, ColorRequest, TextAreaSizeRequest) always send; PtyWrite/ColorRequest/TextAreaSizeRequest answered inline on the PTY thread via Notifier (spike-proven).
3. **Shutdown:** on quit: send `Msg::Shutdown` to ALL terminals first; join IO threads on ONE background thread with a 3s deadline; on expiry SIGKILL the shell process group and detach. Never join on the UI thread; `on_app_quit`'s 100ms budget only triggers this sequence.
4. **Signal mask:** PTY construction happens on the foreground (main) thread always — codified; no sigmask juggling (spike-proven).
5. **Core inventory (final):** core = `session` (SessionManager + validation + NEW atomic write via tmp+rename), `shell_env`, `proc_cwd`, `git/` (all logic), `buddy` (logic). src-tauri keeps: pty.rs (portable-pty impl), bg.rs, and thin `#[tauri::command]` wrappers for session/git/buddy. Native v1 consumes session/shell_env/proc_cwd only; git/buddy consumption is v1.x.
6. **Session sharing between the two apps:** atomic writes (tmp+rename) in core; last-writer-wins; no locking (explicit v1 decision).
7. **Session UI + reconstruction (v1):** tmux bar gains a sessions overlay (list, save-as-name, load). Load: fresh terminal entity per layout leaf (new uuid, default cwd), preserving tree shape/`sizes`/labels/`activeTabId`. Serializable schema stays the DTO (`layout.rs` PaneNode with String ids); the runtime tree maps DTO ids -> entities in `workspace.rs` (two representations, one conversion — no Entity in the DTO).
8. **Cwd features concretely:** shell pid = `tcgetpgrp(master_fd)` at query time (foreground process), fall back to spawned child pid; cwd = `proc_cwd(pid)`; overlay refreshes on hover-show + 5s while visible; split inheritance queries at split time; Finder reveal = `/usr/bin/open -R <path>` via std Command. Pid invalid after ChildExit.
9. **Fonts:** explicit fallback chain ["JetBrains Mono", "Menlo", "Monaco"] via gpui FontFallbacks (Menlo guaranteed on macOS); nothing bundled in v1; nerd-glyph fidelity is fallback-dependent (acceptance reworded accordingly).
10. **IME-correct input:** printable text flows through gpui's `EntityInputHandler` (`replace_text_in_range` -> PTY write; marked-text ranges held, not sent); `key_down` handles ONLY chords/controls (returns unhandled for printables). Dead-key/CJK verified by manual smoke; pure translation logic unit-tested.
11. **Rendering:** full repaint per frame in v1 (no damage tracking); DIM = scale fg RGB by 2/3 (never alpha); styled-run batching per row.
12. **Packaging:** `native/bundle.sh` hand-builds `SuperTerminal Native.app` (Contents/MacOS binary + Info.plist + existing icon.icns) + ad-hoc `codesign`; no bundler dependency. Size target: release `.app` ≤ 20 MB uncompressed.
13. **Acceptance rewrites:** "single process" -> "one native UI host process plus PTY children; zero WebKit/WebView helper processes". "No emoji" scoped to application chrome (terminal cell content renders whatever programs emit, incl. wide glyphs/emoji via font fallback).
14. Spike is DONE (passed); all conditional-spike language in spec/brief is void. Brief statements that native consumes git/buddy refer to v1.x, not v1.
