# Dependency Cleanup Design

**Date:** 2026-04-11
**Goal:** Eliminate all npm vulnerabilities and reduce runtime dependencies from 11 to 4 by replacing packages with custom code.

## Target State

### Runtime Dependencies (4, down from 11)
- `@xterm/xterm` — terminal emulator, irreplaceable
- `node-pty` — native PTY bindings, irreplaceable
- `react` — UI framework
- `react-dom` — React DOM renderer

### Dev Dependencies (9, down from 10)
- `electron` ^41 — upgraded from ^28, clears 17 advisories
- `electron-builder` — packaging
- `@electron/rebuild` — replaces deprecated `electron-rebuild`
- `vite` ^6 — upgraded from ^5, clears esbuild advisory
- `@vitejs/plugin-react` — JSX compilation
- `typescript` — type checking
- `@types/node`, `@types/react`, `@types/react-dom` — type definitions

### Removed Packages
| Package | Replacement |
|---------|-------------|
| `zustand` | Custom `createStore` using `useSyncExternalStore` |
| `allotment` | Custom `SplitPane` component |
| `nanoid` | `crypto.randomUUID()` |
| `@xterm/addon-fit` | Custom fit function |
| `@xterm/addon-search` | Custom search over xterm buffer |
| `@xterm/addon-web-links` | Custom link provider |
| `@xterm/addon-unicode11` | Nothing (was never imported) |
| `electron-rebuild` | `@electron/rebuild` (drop-in rename) |
| `vite-plugin-electron` | Nothing (was never imported) |
| `concurrently` | Shell background jobs in dev script |

## Custom Implementations

### 1. `createStore` utility (replaces zustand)

**File:** `src/renderer/lib/create-store.ts` (~60 lines)

A generic `createStore<T>` function that preserves the exact API currently used throughout the app:

```ts
// Store creation — identical signature to zustand's `create`
export const useTerminalStore = createStore<TerminalStore>((set, get) => ({ ... }));

// Selector-based subscription in components
const tabs = useTerminalStore(s => s.tabs);

// Imperative access outside React
useTerminalStore.getState().setTerminalTitle(id, title);
```

**Implementation details:**
- State held in a closure
- `subscribe(listener)` for change notifications
- `getState()` / `setState()` for imperative use
- The returned function is a React hook using `useSyncExternalStore(subscribe, getSnapshot)`
- Selector results compared with shallow equality to prevent unnecessary re-renders
- Shallow equality: compare each key of the selected value if it's an object, or use `===` for primitives

**Files that change imports:**
- `src/renderer/stores/terminal-store.ts` — `import { create } from 'zustand'` becomes `import { createStore } from '../lib/create-store'`
- `src/renderer/stores/theme-store.ts` — same import change
- Store body code does not change at all

### 2. Custom SplitPane (replaces allotment)

**File:** `src/renderer/components/SplitPane.tsx` (rewrite, ~300-400 lines)

**Features (matching allotment's used behavior + full feature set):**
- Horizontal and vertical splits via `direction` prop
- Draggable divider handles between panes
- Proportional resizing: when the container resizes, panes maintain their proportional sizes
- Configurable minimum pane size (default 50px)
- Snap-to-close: if a pane is dragged below a threshold (e.g., 20px), it collapses to 0
- Double-click divider to reset panes to equal sizes
- Nested splits (already handled by recursive component structure)

**Implementation approach:**
- Flexbox container with `flex-direction` based on split direction
- Pane sizes stored as fractional proportions (e.g., `[0.5, 0.5]`) in component state
- Divider is an absolutely-positioned `<div>` with `cursor: col-resize` / `row-resize`
- `onMouseDown` on divider starts drag; `mousemove`/`mouseup` on `document` handle drag
- `ResizeObserver` on container to maintain proportions on window resize
- CSS: divider styled as a thin bar (4px) with hover highlight matching `theme.uiBorder`

**Styles:** Inline styles consistent with the rest of the codebase (no CSS file needed since allotment's `style.css` import goes away).

### 3. Custom xterm fit (replaces @xterm/addon-fit)

**File:** `src/renderer/lib/xterm-fit.ts` (~50 lines)

**Function:** `fitTerminal(xterm: Terminal, container: HTMLElement): { cols: number; rows: number }`

**Implementation:**
- Create a temporary `<span>` with the terminal's font, measure a character's width and height
- Divide container's `clientWidth` by char width for cols, `clientHeight` by char height for rows
- Clamp to minimum 2 cols, 1 row
- Call `xterm.resize(cols, rows)`
- Cache character dimensions and invalidate when font changes

**Integration:** `safeFit()` in `xterm-registry.ts` calls `fitTerminal()` instead of `fitAddon.fit()`. The `FitAddon` instance is removed from `XtermEntry`.

### 4. Custom xterm search (replaces @xterm/addon-search)

**File:** `src/renderer/lib/xterm-search.ts` (~100-150 lines)

**Exports:**
- `findNext(xterm: Terminal, query: string, options?: { caseSensitive?: boolean }): boolean` — finds and highlights next match, returns whether a match was found
- `clearDecorations(xterm: Terminal): void` — removes all search highlights

**Implementation:**
- Walk the active buffer line by line via `xterm.buffer.active.getLine(i).translateToString()`
- Build a flat text representation, track line/col offsets
- Find all matches of the query string (case-insensitive by default)
- Use `xterm.registerDecoration()` to highlight matches
- Track current match index; `findNext` advances to the next one and scrolls to it
- Store decoration state per-terminal in a `WeakMap<Terminal, ...>`

**Integration:** `searchAddon.findNext(...)` and `searchAddon.clearDecorations()` in TerminalView.tsx become `findNext(entry.xterm, ...)` and `clearDecorations(entry.xterm)`. The `SearchAddon` is removed from `XtermEntry`.

### 5. Custom xterm web-links (replaces @xterm/addon-web-links)

**File:** `src/renderer/lib/xterm-web-links.ts` (~100-150 lines)

**Export:** `registerWebLinks(xterm: Terminal): void`

**Implementation:**
- Use `xterm.registerLinkProvider()` with a custom provider
- Provider scans each line (via `provideLinks(bufferLineNumber, callback)`) for URLs
- URL regex: matches `https?://`, `www.`, and common URL patterns, stopping at whitespace/closing-brackets
- Each detected link gets an `ILink` object with `text`, `range`, and `activate` callback
- `activate` calls `window.open(url)` (or `shell.openExternal` via IPC if preferred — but current codebase already handles this via `setWindowOpenHandler`)

**Integration:** Replace `xterm.loadAddon(webLinksAddon)` in xterm-registry.ts with `registerWebLinks(xterm)`.

### 6. nanoid replacement

**File change:** `src/renderer/stores/terminal-store.ts`

Replace `import { nanoid } from 'nanoid'` with `crypto.randomUUID()`. This is available in all modern browsers and in Electron's renderer process.

```ts
// Before
import { nanoid } from 'nanoid';
function createTerminal(cwd?: string): TerminalInstance {
  return { id: nanoid(), title: 'Terminal', cwd };
}

// After
function createTerminal(cwd?: string): TerminalInstance {
  return { id: crypto.randomUUID(), title: 'Terminal', cwd };
}
```

### 7. Dev script (replaces concurrently)

**Before:**
```json
"dev": "tsc -p tsconfig.main.json && concurrently \"tsc -p tsconfig.main.json --watch\" \"vite\" \"electron dist/main/index.js --dev\""
```

**After:**
```json
"dev": "tsc -p tsconfig.main.json && (tsc -p tsconfig.main.json --watch & vite & sleep 2 && electron dist/main/index.js --dev)"
```

The `sleep 2` gives vite time to start before electron tries to connect to `localhost:5173`.

### 8. Package upgrades

**electron ^28 to ^41:**
- API usage in `src/main/index.ts`: `app`, `BrowserWindow`, `ipcMain`, `shell` — all stable, no breaking changes
- `titleBarStyle: 'hiddenInset'` — still supported
- `webPreferences.preload`, `contextIsolation`, `nodeIntegration: false` — still supported, still recommended
- Preload uses `contextBridge.exposeInMainWorld` and `ipcRenderer.invoke`/`on` — stable API
- No use of deprecated APIs (`remote`, `dialog` from renderer, etc.)

**vite ^5 to ^6:**
- `vite.config.ts` uses `defineConfig`, `react()` plugin, basic options — no breaking changes expected
- `@vitejs/plugin-react` may need a compatible version bump

**electron-rebuild to @electron/rebuild:**
- Same API, just a package rename
- `postinstall` script: `electron-rebuild` becomes `electron-rebuild` (the CLI name stays the same, it's provided by `@electron/rebuild`)

## Execution Order (Single Branch, Big Bang)

1. Create `src/renderer/lib/create-store.ts`
2. Create `src/renderer/lib/xterm-fit.ts`
3. Create `src/renderer/lib/xterm-search.ts`
4. Create `src/renderer/lib/xterm-web-links.ts`
5. Update `terminal-store.ts`: replace zustand import, replace nanoid with `crypto.randomUUID()`
6. Update `theme-store.ts`: replace zustand import
7. Update `xterm-registry.ts`: replace addon imports with custom implementations
8. Update `TerminalView.tsx`: use custom search functions
9. Rewrite `SplitPane.tsx`: custom split pane component (remove allotment)
10. Update `package.json`: upgrade electron/vite, swap electron-rebuild, remove all replaced deps, fix dev script
11. `npm install` with clean `node_modules`
12. Build and verify: `npm run build`
13. Verify: `npm audit` shows 0 vulnerabilities

## Success Criteria

- `npm audit` reports 0 vulnerabilities
- `npm run build` succeeds
- `npm run dev` starts the app successfully
- All existing features work: terminal creation, splits, tabs, search, clickable links, themes, broadcast mode, resize, swap panes
- Runtime `dependencies` in package.json: 4 packages
- Dev `devDependencies` in package.json: 9 packages
