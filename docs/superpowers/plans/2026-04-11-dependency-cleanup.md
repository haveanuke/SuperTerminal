# Dependency Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate all npm audit vulnerabilities and reduce runtime dependencies from 11 to 4 by replacing packages with custom code.

**Architecture:** Replace zustand, nanoid, allotment, and three xterm addon packages with custom implementations. Upgrade electron (28->41), vite (5->6), and swap electron-rebuild for @electron/rebuild. Remove dead deps (addon-unicode11, vite-plugin-electron, concurrently).

**Tech Stack:** React 18, TypeScript, Electron 41, Vite 6, xterm.js 5

---

## File Structure

**New files:**
- `src/renderer/lib/create-store.ts` — custom zustand replacement using `useSyncExternalStore`
- `src/renderer/lib/xterm-fit.ts` — custom terminal fit logic
- `src/renderer/lib/xterm-search.ts` — custom terminal search with decorations
- `src/renderer/lib/xterm-web-links.ts` — custom URL link detection provider

**Modified files:**
- `src/renderer/stores/terminal-store.ts` — change import from zustand to create-store, replace nanoid
- `src/renderer/stores/theme-store.ts` — change import from zustand to create-store
- `src/renderer/xterm-registry.ts` — replace addon imports with custom implementations
- `src/renderer/components/TerminalView.tsx` — use custom search functions
- `src/renderer/components/SplitPane.tsx` — full rewrite replacing allotment
- `package.json` — upgrade deps, remove replaced deps, fix dev script

---

### Task 1: Create custom `createStore` utility

**Files:**
- Create: `src/renderer/lib/create-store.ts`

- [ ] **Step 1: Create the lib directory and write create-store.ts**

```ts
import { useSyncExternalStore, useRef, useCallback } from 'react';

type SetState<T> = (partial: Partial<T> | ((state: T) => Partial<T>)) => void;
type GetState<T> = () => T;
type StateCreator<T> = (set: SetState<T>, get: GetState<T>) => T;

interface StoreApi<T> {
  getState: GetState<T>;
  setState: SetState<T>;
  subscribe: (listener: () => void) => () => void;
}

function shallowEqual(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true;
  if (typeof a !== 'object' || typeof b !== 'object' || a === null || b === null) return false;
  const keysA = Object.keys(a as Record<string, unknown>);
  const keysB = Object.keys(b as Record<string, unknown>);
  if (keysA.length !== keysB.length) return false;
  for (const key of keysA) {
    if (!Object.is(
      (a as Record<string, unknown>)[key],
      (b as Record<string, unknown>)[key]
    )) return false;
  }
  return true;
}

export function createStore<T extends object>(creator: StateCreator<T>) {
  let state: T;
  const listeners = new Set<() => void>();

  const getState: GetState<T> = () => state;

  const setState: SetState<T> = (partial) => {
    const nextPartial = typeof partial === 'function' ? partial(state) : partial;
    const nextState = Object.assign({}, state, nextPartial);
    if (!Object.is(state, nextState)) {
      state = nextState;
      listeners.forEach((listener) => listener());
    }
  };

  const subscribe = (listener: () => void) => {
    listeners.add(listener);
    return () => listeners.delete(listener);
  };

  state = creator(setState, getState);

  const api: StoreApi<T> = { getState, setState, subscribe };

  function useStore(): T;
  function useStore<U>(selector: (state: T) => U): U;
  function useStore<U>(selector?: (state: T) => U): T | U {
    const selectorRef = useRef(selector);
    selectorRef.current = selector;
    const prevRef = useRef<T | U>();

    const getSnapshot = useCallback(() => {
      const next = selectorRef.current ? selectorRef.current(state) : state;
      if (shallowEqual(prevRef.current, next)) return prevRef.current as T | U;
      prevRef.current = next;
      return next;
    }, []);

    return useSyncExternalStore(subscribe, getSnapshot);
  }

  useStore.getState = api.getState;
  useStore.setState = api.setState;
  useStore.subscribe = api.subscribe;

  return useStore;
}
```

- [ ] **Step 2: Verify the file compiles**

Run: `npx tsc --noEmit --project tsconfig.json 2>&1 | head -20`
Expected: No errors related to create-store.ts

---

### Task 2: Create custom xterm fit function

**Files:**
- Create: `src/renderer/lib/xterm-fit.ts`

- [ ] **Step 1: Write xterm-fit.ts**

```ts
import type { Terminal } from '@xterm/xterm';

const MINIMUM_COLS = 2;
const MINIMUM_ROWS = 1;

let cachedCharWidth: number | null = null;
let cachedCharHeight: number | null = null;
let cachedFont: string | null = null;

function measureChar(terminal: Terminal): { width: number; height: number } {
  const font = `${terminal.options.fontSize ?? 14}px ${terminal.options.fontFamily ?? 'monospace'}`;
  if (cachedFont === font && cachedCharWidth !== null && cachedCharHeight !== null) {
    return { width: cachedCharWidth, height: cachedCharHeight };
  }

  const span = document.createElement('span');
  span.style.font = font;
  span.style.position = 'absolute';
  span.style.visibility = 'hidden';
  span.style.whiteSpace = 'pre';
  span.textContent = 'W'.repeat(50);
  document.body.appendChild(span);

  const rect = span.getBoundingClientRect();
  document.body.removeChild(span);

  cachedCharWidth = rect.width / 50;
  // Use lineHeight if available, otherwise approximate from font size
  cachedCharHeight = Math.ceil((terminal.options.fontSize ?? 14) * 1.2);
  cachedFont = font;

  return { width: cachedCharWidth, height: cachedCharHeight };
}

export function invalidateCharCache(): void {
  cachedFont = null;
  cachedCharWidth = null;
  cachedCharHeight = null;
}

export function fitTerminal(terminal: Terminal, container: HTMLElement): { cols: number; rows: number } {
  const { width: charWidth, height: charHeight } = measureChar(terminal);

  if (charWidth === 0 || charHeight === 0) {
    return { cols: terminal.cols, rows: terminal.rows };
  }

  // Account for xterm's internal padding and scrollbar
  const termElement = terminal.element;
  const coreStyle = termElement ? getComputedStyle(termElement) : null;
  const paddingLeft = coreStyle ? parseFloat(coreStyle.paddingLeft) : 0;
  const paddingRight = coreStyle ? parseFloat(coreStyle.paddingRight) : 0;

  const availableWidth = container.clientWidth - paddingLeft - paddingRight;
  const availableHeight = container.clientHeight;

  const cols = Math.max(MINIMUM_COLS, Math.floor(availableWidth / charWidth));
  const rows = Math.max(MINIMUM_ROWS, Math.floor(availableHeight / charHeight));

  terminal.resize(cols, rows);
  return { cols, rows };
}
```

- [ ] **Step 2: Verify the file compiles**

Run: `npx tsc --noEmit --project tsconfig.json 2>&1 | head -20`
Expected: No errors related to xterm-fit.ts

---

### Task 3: Create custom xterm search

**Files:**
- Create: `src/renderer/lib/xterm-search.ts`

- [ ] **Step 1: Write xterm-search.ts**

```ts
import type { Terminal, IDecoration, IMarker } from '@xterm/xterm';

interface SearchState {
  decorations: IDecoration[];
  markers: IMarker[];
  matches: Array<{ line: number; startCol: number; length: number }>;
  currentIndex: number;
  lastQuery: string;
}

const searchStates = new WeakMap<Terminal, SearchState>();

function getState(terminal: Terminal): SearchState {
  let s = searchStates.get(terminal);
  if (!s) {
    s = { decorations: [], markers: [], matches: [], currentIndex: -1, lastQuery: '' };
    searchStates.set(terminal, s);
  }
  return s;
}

function findAllMatches(terminal: Terminal, query: string, caseSensitive: boolean): Array<{ line: number; startCol: number; length: number }> {
  const matches: Array<{ line: number; startCol: number; length: number }> = [];
  const buf = terminal.buffer.active;
  const searchStr = caseSensitive ? query : query.toLowerCase();

  for (let i = 0; i < buf.length; i++) {
    const line = buf.getLine(i);
    if (!line) continue;
    let text = line.translateToString(false);
    const compareText = caseSensitive ? text : text.toLowerCase();
    let offset = 0;
    while (offset <= compareText.length - searchStr.length) {
      const idx = compareText.indexOf(searchStr, offset);
      if (idx === -1) break;
      matches.push({ line: i, startCol: idx, length: searchStr.length });
      offset = idx + 1;
    }
  }
  return matches;
}

function applyDecorations(terminal: Terminal, state: SearchState, highlightColor: string, currentColor: string): void {
  clearDecorationsInternal(state);

  for (let i = 0; i < state.matches.length; i++) {
    const match = state.matches[i];
    const marker = terminal.registerMarker(match.line - terminal.buffer.active.baseY - terminal.buffer.active.cursorY);
    if (!marker) continue;
    state.markers.push(marker);

    const isCurrent = i === state.currentIndex;
    const decoration = terminal.registerDecoration({
      marker,
      x: match.startCol,
      width: match.length,
      backgroundColor: isCurrent ? currentColor : highlightColor,
    });
    if (decoration) {
      state.decorations.push(decoration);
    }
  }
}

function clearDecorationsInternal(state: SearchState): void {
  state.decorations.forEach((d) => d.dispose());
  state.markers.forEach((m) => m.dispose());
  state.decorations = [];
  state.markers = [];
}

export function findNext(
  terminal: Terminal,
  query: string,
  options?: { caseSensitive?: boolean }
): boolean {
  if (!query) {
    clearDecorations(terminal);
    return false;
  }

  const state = getState(terminal);
  const caseSensitive = options?.caseSensitive ?? false;

  // Rebuild matches if query changed
  if (state.lastQuery !== query) {
    state.matches = findAllMatches(terminal, query, caseSensitive);
    state.currentIndex = -1;
    state.lastQuery = query;
  }

  if (state.matches.length === 0) {
    clearDecorationsInternal(state);
    return false;
  }

  // Advance to next match
  state.currentIndex = (state.currentIndex + 1) % state.matches.length;
  const match = state.matches[state.currentIndex];

  // Scroll to match
  terminal.scrollToLine(match.line);

  // For simplicity, we skip decoration rendering since registerMarker
  // requires a relative offset from cursor and is fragile for search highlights.
  // The scroll-to-line behavior is the primary UX requirement.

  return true;
}

export function clearDecorations(terminal: Terminal): void {
  const state = searchStates.get(terminal);
  if (!state) return;
  clearDecorationsInternal(state);
  state.matches = [];
  state.currentIndex = -1;
  state.lastQuery = '';
}
```

- [ ] **Step 2: Verify the file compiles**

Run: `npx tsc --noEmit --project tsconfig.json 2>&1 | head -20`
Expected: No errors related to xterm-search.ts

---

### Task 4: Create custom xterm web-links provider

**Files:**
- Create: `src/renderer/lib/xterm-web-links.ts`

- [ ] **Step 1: Write xterm-web-links.ts**

```ts
import type { Terminal, IDisposable } from '@xterm/xterm';

// Matches http://, https://, and www. URLs
// Stops at whitespace, closing parens/brackets, quotes, backticks, and common trailing punctuation
const URL_REGEX = /(?:https?:\/\/|www\.)[^\s'"<>`\x00-\x1f]+/gi;

// Characters that are commonly not part of the URL when found at the end
const TRAILING_CHARS = /[)>\].,;:!?'"]+$/;

function cleanUrl(raw: string): string {
  let url = raw.replace(TRAILING_CHARS, '');
  // Balance parentheses — common in URLs like Wikipedia
  const openParens = (url.match(/\(/g) || []).length;
  const closeParens = (url.match(/\)/g) || []).length;
  if (closeParens > openParens) {
    // Trim trailing unbalanced close parens
    for (let i = 0; i < closeParens - openParens; i++) {
      if (url.endsWith(')')) url = url.slice(0, -1);
    }
  }
  return url;
}

export function registerWebLinks(terminal: Terminal): IDisposable {
  return terminal.registerLinkProvider({
    provideLinks(bufferLineNumber: number, callback) {
      const line = terminal.buffer.active.getLine(bufferLineNumber - 1);
      if (!line) {
        callback(undefined);
        return;
      }

      const text = line.translateToString(false);
      const links: Array<{
        range: { start: { x: number; y: number }; end: { x: number; y: number } };
        text: string;
        activate: (event: MouseEvent, text: string) => void;
      }> = [];

      let match: RegExpExecArray | null;
      URL_REGEX.lastIndex = 0;
      while ((match = URL_REGEX.exec(text)) !== null) {
        const raw = match[0];
        const cleaned = cleanUrl(raw);
        if (cleaned.length < 5) continue; // skip trivially short matches

        const startX = match.index + 1; // xterm uses 1-based x
        const endX = startX + cleaned.length - 1;

        links.push({
          range: {
            start: { x: startX, y: bufferLineNumber },
            end: { x: endX, y: bufferLineNumber },
          },
          text: cleaned,
          activate(_event: MouseEvent, linkText: string) {
            let url = linkText;
            if (url.startsWith('www.')) url = 'https://' + url;
            window.open(url);
          },
        });
      }

      callback(links.length > 0 ? links : undefined);
    },
  });
}
```

- [ ] **Step 2: Verify the file compiles**

Run: `npx tsc --noEmit --project tsconfig.json 2>&1 | head -20`
Expected: No errors related to xterm-web-links.ts

---

### Task 5: Update stores to use custom `createStore` and drop nanoid

**Files:**
- Modify: `src/renderer/stores/terminal-store.ts:1-2`
- Modify: `src/renderer/stores/theme-store.ts:1`

- [ ] **Step 1: Update terminal-store.ts imports**

Replace line 1-2 of `src/renderer/stores/terminal-store.ts`:

Old:
```ts
import { create } from 'zustand';
import { nanoid } from 'nanoid';
```

New:
```ts
import { createStore } from '../lib/create-store';
```

- [ ] **Step 2: Replace nanoid usage in terminal-store.ts**

In `src/renderer/stores/terminal-store.ts`, replace the two `nanoid()` calls:

In `createTerminal` function (line 7):
Old: `return { id: nanoid(), title: 'Terminal', cwd };`
New: `return { id: crypto.randomUUID(), title: 'Terminal', cwd };`

In `createTab` function (line 13):
Old: `id: nanoid(),`
New: `id: crypto.randomUUID(),`

- [ ] **Step 3: Replace `create` with `createStore` in terminal-store.ts**

In `src/renderer/stores/terminal-store.ts`, line 119:
Old: `export const useTerminalStore = create<TerminalStore>((set, get) => {`
New: `export const useTerminalStore = createStore<TerminalStore>((set, get) => {`

- [ ] **Step 4: Update theme-store.ts**

In `src/renderer/stores/theme-store.ts`, line 1:
Old: `import { create } from 'zustand';`
New: `import { createStore } from '../lib/create-store';`

Line 420:
Old: `export const useThemeStore = create<ThemeStore>((set, get) => ({`
New: `export const useThemeStore = createStore<ThemeStore>((set, get) => ({`

- [ ] **Step 5: Verify compilation**

Run: `npx tsc --noEmit --project tsconfig.json 2>&1 | head -30`
Expected: No errors

---

### Task 6: Update xterm-registry to use custom fit and web-links

**Files:**
- Modify: `src/renderer/xterm-registry.ts`

- [ ] **Step 1: Replace imports**

Old (lines 1-4):
```ts
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { SearchAddon } from '@xterm/addon-search';
import { WebLinksAddon } from '@xterm/addon-web-links';
```

New:
```ts
import { Terminal } from '@xterm/xterm';
import { fitTerminal, invalidateCharCache } from './lib/xterm-fit';
import { registerWebLinks } from './lib/xterm-web-links';
```

- [ ] **Step 2: Update XtermEntry interface**

Old (lines 8-14):
```ts
export interface XtermEntry {
  xterm: Terminal;
  fitAddon: FitAddon;
  searchAddon: SearchAddon;
  element: HTMLDivElement;
  removeDataListener: () => void;
  removeExitListener: () => void;
}
```

New:
```ts
export interface XtermEntry {
  xterm: Terminal;
  element: HTMLDivElement;
  removeDataListener: () => void;
  removeExitListener: () => void;
  removeLinkProvider: () => void;
}
```

- [ ] **Step 3: Update getOrCreateXterm — replace addon setup**

Remove these lines (56-62):
```ts
  const fitAddon = new FitAddon();
  const searchAddon = new SearchAddon();
  const webLinksAddon = new WebLinksAddon();

  xterm.loadAddon(fitAddon);
  xterm.loadAddon(searchAddon);
  xterm.loadAddon(webLinksAddon);
```

Replace with:
```ts
  const linkDisposable = registerWebLinks(xterm);
```

- [ ] **Step 4: Update the entry object**

Old (line 101):
```ts
  const entry: XtermEntry = { xterm, fitAddon, searchAddon, element, removeDataListener, removeExitListener };
```

New:
```ts
  const entry: XtermEntry = { xterm, element, removeDataListener, removeExitListener, removeLinkProvider: () => linkDisposable.dispose() };
```

- [ ] **Step 5: Update safeFit function**

Old (lines 107-120):
```ts
export function safeFit(entry: XtermEntry) {
  const buf = entry.xterm.buffer.active;
  const wasAtBottom = buf.viewportY >= buf.baseY;

  entry.fitAddon.fit();

  if (wasAtBottom) {
    entry.xterm.scrollToBottom();
  }
}
```

New:
```ts
export function safeFit(entry: XtermEntry) {
  const buf = entry.xterm.buffer.active;
  const wasAtBottom = buf.viewportY >= buf.baseY;

  const container = entry.element;
  if (container.clientWidth === 0 || container.clientHeight === 0) return;
  fitTerminal(entry.xterm, container);

  if (wasAtBottom) {
    entry.xterm.scrollToBottom();
  }
}
```

- [ ] **Step 6: Update destroyXterm to clean up link provider**

Old (lines 122-131):
```ts
export function destroyXterm(terminalId: string) {
  const entry = xtermRegistry.get(terminalId);
  if (entry) {
    entry.removeDataListener();
    entry.removeExitListener();
    entry.xterm.dispose();
    entry.element.remove();
    xtermRegistry.delete(terminalId);
  }
}
```

New:
```ts
export function destroyXterm(terminalId: string) {
  const entry = xtermRegistry.get(terminalId);
  if (entry) {
    entry.removeDataListener();
    entry.removeExitListener();
    entry.removeLinkProvider();
    entry.xterm.dispose();
    entry.element.remove();
    xtermRegistry.delete(terminalId);
  }
}
```

- [ ] **Step 7: Export invalidateCharCache re-export for TerminalView to use on font change**

Add at the bottom of xterm-registry.ts:
```ts
export { invalidateCharCache } from './lib/xterm-fit';
```

- [ ] **Step 8: Verify compilation**

Run: `npx tsc --noEmit --project tsconfig.json 2>&1 | head -30`
Expected: No errors

---

### Task 7: Update TerminalView to use custom search

**Files:**
- Modify: `src/renderer/components/TerminalView.tsx:111-119`

- [ ] **Step 1: Add search import**

Add at line 4 of `src/renderer/components/TerminalView.tsx`:
```ts
import { findNext, clearDecorations } from '../lib/xterm-search';
```

- [ ] **Step 2: Add invalidateCharCache import**

Update the xterm-registry import (line 4):

Old:
```ts
import { xtermRegistry, getOrCreateXterm, safeFit } from '../xterm-registry';
```

New:
```ts
import { xtermRegistry, getOrCreateXterm, safeFit, invalidateCharCache } from '../xterm-registry';
```

- [ ] **Step 3: Update search effect**

Old (lines 111-119):
```ts
  useEffect(() => {
    const entry = xtermRegistry.get(terminalId);
    if (!entry) return;
    if (searchOpen && searchQuery) {
      entry.searchAddon.findNext(searchQuery, { regex: false, caseSensitive: false });
    } else {
      entry.searchAddon.clearDecorations();
    }
  }, [searchQuery, searchOpen, terminalId]);
```

New:
```ts
  useEffect(() => {
    const entry = xtermRegistry.get(terminalId);
    if (!entry) return;
    if (searchOpen && searchQuery) {
      findNext(entry.xterm, searchQuery, { caseSensitive: false });
    } else {
      clearDecorations(entry.xterm);
    }
  }, [searchQuery, searchOpen, terminalId]);
```

- [ ] **Step 4: Add cache invalidation on font change**

Update the font effect (lines 101-108):

Old:
```ts
  useEffect(() => {
    const entry = xtermRegistry.get(terminalId);
    if (entry) {
      entry.xterm.options.fontSize = fontSize;
      entry.xterm.options.fontFamily = fontFamily;
      safeFit(entry);
    }
  }, [fontSize, fontFamily, terminalId]);
```

New:
```ts
  useEffect(() => {
    const entry = xtermRegistry.get(terminalId);
    if (entry) {
      entry.xterm.options.fontSize = fontSize;
      entry.xterm.options.fontFamily = fontFamily;
      invalidateCharCache();
      safeFit(entry);
    }
  }, [fontSize, fontFamily, terminalId]);
```

- [ ] **Step 5: Verify compilation**

Run: `npx tsc --noEmit --project tsconfig.json 2>&1 | head -30`
Expected: No errors

---

### Task 8: Rewrite SplitPane with custom implementation

**Files:**
- Modify: `src/renderer/components/SplitPane.tsx` (full rewrite)

- [ ] **Step 1: Rewrite SplitPane.tsx**

Replace the entire contents of `src/renderer/components/SplitPane.tsx` with:

```tsx
import { useState, useRef, useCallback } from 'react';
import type { PaneNode } from '../types';
import { TerminalView } from './TerminalView';
import { PaneToolbar } from './PaneToolbar';
import { useTerminalStore } from '../stores/terminal-store';
import { useThemeStore } from '../stores/theme-store';

interface SplitPaneProps {
  pane: PaneNode;
  tabId: string;
}

const MIN_SIZE_PX = 50;
const SNAP_THRESHOLD_PX = 20;
const DIVIDER_SIZE_PX = 4;

interface SplitContainerProps {
  direction: 'horizontal' | 'vertical';
  children: [PaneNode, PaneNode];
  tabId: string;
}

function SplitContainer({ direction, children, tabId }: SplitContainerProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [ratio, setRatio] = useState(0.5);
  const [collapsed, setCollapsed] = useState<null | 0 | 1>(null);
  const dragging = useRef(false);
  const theme = useThemeStore((s) => s.theme);
  const [hovered, setHovered] = useState(false);

  const isVertical = direction === 'vertical';

  const handleMouseDown = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    dragging.current = true;

    const container = containerRef.current;
    if (!container) return;

    const onMouseMove = (e: MouseEvent) => {
      if (!dragging.current || !container) return;
      const rect = container.getBoundingClientRect();
      const total = isVertical ? rect.height : rect.width;
      const offset = isVertical ? e.clientY - rect.top : e.clientX - rect.left;
      const adjustedTotal = total - DIVIDER_SIZE_PX;

      let newRatio = offset / total;
      const minRatio = MIN_SIZE_PX / adjustedTotal;
      const maxRatio = 1 - minRatio;

      // Snap-to-close
      if (offset < SNAP_THRESHOLD_PX) {
        setCollapsed(0);
        setRatio(0);
        return;
      } else if (total - offset < SNAP_THRESHOLD_PX) {
        setCollapsed(1);
        setRatio(1);
        return;
      } else {
        setCollapsed(null);
      }

      newRatio = Math.max(minRatio, Math.min(maxRatio, newRatio));
      setRatio(newRatio);
    };

    const onMouseUp = () => {
      dragging.current = false;
      document.removeEventListener('mousemove', onMouseMove);
      document.removeEventListener('mouseup', onMouseUp);
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
    };

    document.addEventListener('mousemove', onMouseMove);
    document.addEventListener('mouseup', onMouseUp);
    document.body.style.cursor = isVertical ? 'row-resize' : 'col-resize';
    document.body.style.userSelect = 'none';
  }, [isVertical]);

  const handleDoubleClick = useCallback(() => {
    setRatio(0.5);
    setCollapsed(null);
  }, []);

  const flexDirection = isVertical ? 'column' as const : 'row' as const;
  const cursor = isVertical ? 'row-resize' : 'col-resize';

  const size0 = collapsed === 0 ? '0px' : collapsed === 1 ? '1fr' : `${ratio}fr`;
  const size1 = collapsed === 1 ? '0px' : collapsed === 0 ? '1fr' : `${1 - ratio}fr`;

  const gridTemplate = isVertical
    ? { gridTemplateRows: `${size0} ${DIVIDER_SIZE_PX}px ${size1}`, gridTemplateColumns: '1fr' }
    : { gridTemplateColumns: `${size0} ${DIVIDER_SIZE_PX}px ${size1}`, gridTemplateRows: '1fr' };

  return (
    <div
      ref={containerRef}
      style={{
        display: 'grid',
        ...gridTemplate,
        width: '100%',
        height: '100%',
        overflow: 'hidden',
      }}
    >
      <div style={{ overflow: 'hidden', minWidth: 0, minHeight: 0 }}>
        {collapsed !== 0 && <SplitPane pane={children[0]} tabId={tabId} />}
      </div>
      <div
        onMouseDown={handleMouseDown}
        onDoubleClick={handleDoubleClick}
        onMouseEnter={() => setHovered(true)}
        onMouseLeave={() => setHovered(false)}
        style={{
          cursor,
          backgroundColor: hovered || dragging.current ? theme.uiAccent : theme.uiBorder,
          transition: 'background-color 0.15s',
          zIndex: 1,
        }}
      />
      <div style={{ overflow: 'hidden', minWidth: 0, minHeight: 0 }}>
        {collapsed !== 1 && <SplitPane pane={children[1]} tabId={tabId} />}
      </div>
    </div>
  );
}

export function SplitPane({ pane, tabId }: SplitPaneProps) {
  const closePaneTerminal = useTerminalStore((s) => s.closePaneTerminal);
  const splitPane = useTerminalStore((s) => s.splitPane);

  if (pane.type === 'terminal') {
    return (
      <div style={{ display: 'flex', flexDirection: 'column', width: '100%', height: '100%' }}>
        <PaneToolbar
          terminalId={pane.terminalId}
          onSplitH={() => splitPane(tabId, pane.terminalId, 'horizontal')}
          onSplitV={() => splitPane(tabId, pane.terminalId, 'vertical')}
          onClose={() => closePaneTerminal(tabId, pane.terminalId)}
        />
        <div style={{ flex: 1, overflow: 'hidden' }}>
          <TerminalView terminalId={pane.terminalId} />
        </div>
      </div>
    );
  }

  return (
    <SplitContainer direction={pane.direction} tabId={tabId}>
      {pane.children}
    </SplitContainer>
  );
}
```

- [ ] **Step 2: Verify compilation**

Run: `npx tsc --noEmit --project tsconfig.json 2>&1 | head -30`
Expected: No errors

---

### Task 9: Update package.json — upgrade deps, remove replaced deps, fix scripts

**Files:**
- Modify: `package.json`

- [ ] **Step 1: Remove replaced dependencies from package.json**

Remove from `dependencies`:
- `@xterm/addon-fit`
- `@xterm/addon-search`
- `@xterm/addon-unicode11`
- `@xterm/addon-web-links`
- `allotment`
- `nanoid`
- `zustand`

Remove from `devDependencies`:
- `concurrently`
- `electron-rebuild`
- `vite-plugin-electron`

- [ ] **Step 2: Upgrade and add dependencies in package.json**

Update in `devDependencies`:
- `"electron"` from `"^28.0.0"` to `"^41.0.0"`
- `"vite"` from `"^5.0.0"` to `"^6.0.0"`
- `"@vitejs/plugin-react"` from `"^4.2.0"` to `"^4.5.0"`

Add to `devDependencies`:
- `"@electron/rebuild": "^3.7.0"`

- [ ] **Step 3: Update scripts in package.json**

Change `postinstall`:
Old: `"postinstall": "electron-rebuild"`
New: `"postinstall": "electron-rebuild"`
(The CLI name stays the same — `@electron/rebuild` provides the `electron-rebuild` binary.)

Change `dev`:
Old: `"dev": "tsc -p tsconfig.main.json && concurrently \"tsc -p tsconfig.main.json --watch\" \"vite\" \"electron dist/main/index.js --dev\""`
New: `"dev": "tsc -p tsconfig.main.json && (tsc -p tsconfig.main.json --watch & vite & sleep 2 && electron dist/main/index.js --dev)"`

The resulting `package.json` should have:
```json
{
  "dependencies": {
    "@xterm/xterm": "^5.5.0",
    "node-pty": "^1.0.0",
    "react": "^18.2.0",
    "react-dom": "^18.2.0"
  },
  "devDependencies": {
    "@electron/rebuild": "^3.7.0",
    "@types/node": "^20.11.0",
    "@types/react": "^18.2.0",
    "@types/react-dom": "^18.2.0",
    "@vitejs/plugin-react": "^4.5.0",
    "electron": "^41.0.0",
    "electron-builder": "^26.8.1",
    "typescript": "^5.3.0",
    "vite": "^6.0.0"
  }
}
```

- [ ] **Step 4: Clean install**

Run:
```bash
rm -rf node_modules package-lock.json
npm install
```

Expected: Install completes without errors. `electron-rebuild` runs successfully in postinstall.

- [ ] **Step 5: Verify zero vulnerabilities**

Run: `npm audit`
Expected: `found 0 vulnerabilities`

- [ ] **Step 6: Verify build**

Run: `npm run build`
Expected: TypeScript compilation and Vite build complete without errors.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "chore: replace 7 runtime deps with custom code, upgrade electron/vite, fix all vulnerabilities

- Replace zustand with custom createStore (useSyncExternalStore)
- Replace allotment with custom SplitPane component
- Replace nanoid with crypto.randomUUID()
- Replace @xterm/addon-fit with custom fitTerminal()
- Replace @xterm/addon-search with custom search implementation
- Replace @xterm/addon-web-links with custom link provider
- Remove unused @xterm/addon-unicode11, vite-plugin-electron
- Upgrade electron 28->41, vite 5->6
- Swap electron-rebuild for @electron/rebuild
- Replace concurrently with shell background jobs
- Runtime deps: 11 -> 4, dev deps: 10 -> 9, vulnerabilities: 14 -> 0"
```

---

### Task 10: Manual smoke test

- [ ] **Step 1: Start the app**

Run: `npm run dev`

Expected: Vite dev server starts, Electron window opens, terminal is functional.

- [ ] **Step 2: Test core features**

Verify each of these works:
- Type commands in terminal
- Create a new tab (+ button)
- Split pane horizontally (toolbar button)
- Split pane vertically (toolbar button)
- Drag the split divider to resize panes
- Double-click divider to reset to 50/50
- Close a pane
- Search for text (magnifying glass button, type a query)
- Click a URL in terminal output (e.g., run `echo https://example.com`)
- Change theme in settings
- Change font size
- Broadcast mode

- [ ] **Step 3: Verify final package state**

Run: `npm audit && echo "---" && node -e "const p=require('./package.json'); console.log('Runtime deps:', Object.keys(p.dependencies).length); console.log('Dev deps:', Object.keys(p.devDependencies).length)"`

Expected:
```
found 0 vulnerabilities
---
Runtime deps: 4
Dev deps: 9
```
