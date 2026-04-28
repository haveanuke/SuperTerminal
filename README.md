# SuperTerminal

A modern multi-terminal manager built with Electron, React, and xterm.js.

## Setup

```bash
npm install
```

## Development

One command to compile, start Vite, and launch Electron:

```bash
npm run dev
```

## Production Build

```bash
npm run build
npm start
```

## Packaging

Build a distributable `.dmg` for macOS:

```bash
npm run package
```

Output lands in `release/` (gitignored). The build produces two DMGs — one for Intel (`x64`) and one for Apple Silicon (`arm64`).

> **Note on unsigned builds:** Without an Apple Developer signing identity, macOS Gatekeeper will block the app on first launch ("Apple cannot check this app for malicious software"). Users can either right-click → **Open** to bypass once, or run `xattr -cr /Applications/SuperTerminal.app` after installing. Proper signing + notarization requires a paid Apple Developer account.

## Releases

A GitHub Actions workflow (`.github/workflows/release.yml`) builds and uploads DMGs to a GitHub release whenever a `v*` tag is pushed:

```bash
# Bump the version field in package.json first, then:
git tag v0.2.0
git push origin v0.2.0
```

The workflow runs on `macos-latest`, builds both arches, and attaches the DMGs to a release with auto-generated release notes. Builds are unsigned — see the note above.

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
