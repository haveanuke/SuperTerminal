# SuperTerminal

A modern multi-terminal manager built with Electron, React, and xterm.js.

## Setup

```bash
npm install
```

## Development

Start the Vite dev server and Electron together:

```bash
# Terminal 1 - start Vite
npx vite

# Terminal 2 - compile main process and launch Electron
npx tsc -p tsconfig.main.json && npx electron dist/main/index.js --dev
```

Or use the combined command (compiles main first, then runs both):

```bash
npx tsc -p tsconfig.main.json && npm run dev
```

## Production Build

```bash
npm run build
npm start
```

## Usage

- **Add tabs** - Click `+` in the tab bar
- **Rename tabs** - Double-click a tab label
- **Split panes** - Use `│` (horizontal) or `─` (vertical) buttons on each pane toolbar
- **Switch layouts** - Click `Tabs`, `Splits`, or `Grid` in the top-right
- **Resize grid** - Use `+`/`-` buttons in the status bar (visible in Grid mode)
- **Broadcast input** - Click `BC` to type in all terminals at once
- **Search** - Click the magnifying glass icon to search terminal output
- **Themes & font size** - Click `Settings` in the bottom-right status bar
- **Save/restore sessions** - In the Settings panel, name and save your layout
