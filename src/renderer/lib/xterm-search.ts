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
