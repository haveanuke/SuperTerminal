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
