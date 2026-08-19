# SuperTerminal Native — Design Spec

**Date:** 2026-08-19 (rev 1)
**Status:** Draft — Codex review loop; Tomas reviews milestone builds
**Prior art:** This succeeds the Tauri app (0.3.4, working). Motivation: no webview anywhere — Rust end to end, GPU-rendered ("just write it in Rust").

## Goal

A fully native macOS terminal app: **gpui** (Zed's Metal-backed UI framework) + **`alacritty_terminal`** (terminal emulation core, no GUI deps). No webview, no HTML/JS/CSS at runtime.

**v1 = lean core** (first build Tomas can show his friend):
terminal grid, tabs, splits, the tmux-style UI, themes, session save/restore, cwd features (live path display + click-to-reveal + splits inherit cwd), plain-click & Option+Click cursor move.

**Fast-followers (v1.x, explicitly out of v1):** buddy — **reworked as a change reviewer, not a playful pet** (watches output, reviews the focused repo's working-tree diff via the existing git engine, concise actionable notes, minimal visual presence); git sidebar (port of the existing engine + UI); background images; TTS; broadcast input; auto-run; search. Windows/Linux: non-goals.

## Design (settled with Tomas)

- **tmux-style bottom bar, no top tab strip.** Terminal content runs edge-to-edge up to the traffic lights (small drag strip under them). Bottom bar: numbered tab segments (`1:main  2:server*`), then status (git branch/ahead-behind later), settings glyph. Active tab = accent segment; keyboard-first: `Cmd+1..9` switch, `Cmd+T` new, `Cmd+W` close pane/tab.
- **Chromeless panes.** No persistent per-pane toolbar; a slim overlay (title, cwd path, split/close/swap controls) fades in on hover near the pane's top edge. Focused pane gets a subtle accent border; cwd path click reveals in Finder.
- **No emoji anywhere; vector icons only** (gpui path/svg drawing, same visual language as the current SVG set).
- **Themes:** port the existing presets (`theme-presets.ts` values become a Rust module; same palette struct as `ThemeConfig`). Default Tokyo Night. Settings UI in v1 is minimal: theme picker + font size (a simple overlay panel).
- Cursor: bar, 2px, blinking; block when unfocused-hollow per gpui idioms.

## Architecture

```
Cargo workspace (repo root)
├── src-tauri/            — existing Tauri app (untouched, keeps working)
├── core/                 — NEW shared crate: superterminal-core
│   ├── session.rs        — SessionManager + validation (moved from src-tauri, pure)
│   ├── shell_env.rs      — login-shell env capture (moved, pure)
│   ├── proc_cwd.rs       — proc_pidinfo cwd lookup (extracted from pty.rs)
│   └── git/              — process/status/actions/network/graph (moved; the
│                           thin #[tauri::command] wrappers STAY in src-tauri
│                           and call into core)
└── native/               — NEW binary crate: superterminal-native (gpui app)
    ├── main.rs           — gpui Application, window (overlay titlebar), theme load
    ├── term/             — alacritty_terminal integration:
    │   ├── session.rs    — one PTY+Term per terminal id (alacritty_terminal's
    │   │                   tty + EventLoop; replaces portable-pty here)
    │   └── view.rs       — grid rendering (cells, colors, cursor, selection,
    │                       scrollback), input handling (keys incl. the
    │                       Cmd/Alt keybindings ported from xterm-registry,
    │                       click/Option-click cursor move w/ same guards)
    ├── layout.rs         — tab/split tree (port of terminal-store's PaneNode
    │                       model, same session JSON schema for save/restore)
    ├── ui/               — tmux bar, pane hover overlay, theme picker overlay
    └── themes.rs         — ported presets
```

- **Terminal emulation:** `alacritty_terminal::Term` + its PTY/event-loop per pane; grid snapshots rendered by gpui on its layout/paint cycle; damage-driven redraw. Reference implementation: Zed's `terminal` / `terminal_view` crates (gpui + alacritty_terminal in production).
- **Sessions:** same JSON schema/directory as the Tauri app (both apps read/write the same sessions; migration already handled).
- **Framework risk & licensing (validated in research):** `alacritty_terminal` is a supported standalone library (crates.io/docs.rs, no GUI deps). gpui ships on crates.io, Apache-2.0, but is pre-1.0 with breaking churn and "built for Zed" support expectations — **pin the version**; documented fallbacks: the Apache-2.0 `open-gpui` fork, or iced. First plan task is a spike proving: window + text grid + PTY echo round-trip in gpui; if the spike fails on font/IME/API grounds, stop and re-decide the framework with Tomas.
- **Distribution during development:** bundle id `com.tomaspinal.superterminal.native`, app name "SuperTerminal Native" — installed alongside the Tauri app, which remains Tomas's daily driver until native reaches parity. Retirement of the Tauri app is a later, explicit decision (not part of v1).

## v1 acceptance (what "show the friend" means)

Launch → full-bleed terminal with tmux bar; type with visible 2px bar cursor; click/Option-click cursor move; split H/V (inherits cwd, hover overlay works); Cmd+1..9/T/W; theme picker applies live; save/load a session layout; quit kills shells; `ps` shows a single native process, **no webview**; bundle ≤ ~15 MB.

## Process

Same pipeline as prior phases: this spec → Codex loop → implementation plan (full-code, TDD) → Codex loop → milestone implementation with Codex reviews; every milestone build installed as SuperTerminal Native.app for Tomas. Emulation logic (escape handling, grid state) comes from alacritty_terminal and is NOT re-tested; our tests cover layout tree, session round-trip, keybinding translation, click-to-move math, theme mapping.
