# SuperTerminal

A modern multi-terminal manager built with Tauri 2, Rust, React, and xterm.js.

> **Migrating from the Electron build?** Saved terminal sessions carry over
> automatically on first launch. Theme/font/buddy settings do not (the webview
> storage is different) — re-pick them once in Settings.

## Setup

Requires Node 20+ and a Rust toolchain (`brew install rust` or [rustup](https://rustup.rs)).

```bash
npm install
```

## Development

One command to start Vite and launch the app:

```bash
npm run dev
```

## Packaging

Build a distributable `.app`/`.dmg` for macOS (Apple Silicon):

```bash
npm run package
```

Output lands in `src-tauri/target/release/bundle/` (gitignored).

> **Note on unsigned builds:** Without an Apple Developer signing identity, macOS Gatekeeper will block the app on first launch ("Apple cannot check this app for malicious software"). Users can either right-click → **Open** to bypass once, or run `xattr -cr /Applications/SuperTerminal.app` after installing. Proper signing + notarization requires a paid Apple Developer account.

## Releases

A GitHub Actions workflow (`.github/workflows/release.yml`) builds and uploads DMGs to a GitHub release whenever a `v*` tag is pushed:

```bash
# Bump the version fields (package.json, src-tauri/tauri.conf.json,
# src-tauri/Cargo.toml) first, then:
git tag v0.3.0
git push origin v0.3.0
```

The workflow runs on `macos-latest` (Apple Silicon), and attaches the DMG to a release with auto-generated release notes. Builds are unsigned — see the note above.

## Tests & Lint

```bash
npm test          # vitest run
npm run lint      # eslint
npm run format    # prettier --write
```

## Usage

- **Add tabs** - Click `+` in the tab bar (each tab is an independent workspace)
- **Rename tabs** - Double-click a tab label
- **Split panes** - Use `|` (horizontal) or `-` (vertical) buttons on each pane toolbar
- **Swap panes** - Click the swap button on one pane, then click "Swap here" on another
- **Broadcast input** - Click `BC` to type in all terminals at once
- **Search** - Click the magnifying glass icon to search terminal output
- **Auto-run** - Click the stopwatch icon to repeat a command on a timer
- **Themes & font size** - Click `Settings` in the bottom-right status bar
- **Save/restore sessions** - In the Settings panel, name and save your layout
