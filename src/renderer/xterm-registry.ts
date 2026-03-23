import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { SearchAddon } from '@xterm/addon-search';
import { WebLinksAddon } from '@xterm/addon-web-links';
import { useThemeStore } from './stores/theme-store';
import { useTerminalStore } from './stores/terminal-store';

export interface XtermEntry {
  xterm: Terminal;
  fitAddon: FitAddon;
  searchAddon: SearchAddon;
  element: HTMLDivElement;
  removeDataListener: () => void;
  removeExitListener: () => void;
}

export const xtermRegistry = new Map<string, XtermEntry>();

export function getOrCreateXterm(terminalId: string): XtermEntry {
  const existing = xtermRegistry.get(terminalId);
  if (existing) return existing;

  const themeState = useThemeStore.getState();

  const xterm = new Terminal({
    fontSize: themeState.fontSize,
    fontFamily: themeState.fontFamily,
    scrollback: 10000,
    cursorBlink: true,
    cursorStyle: 'bar',
    allowProposedApi: true,
    theme: {
      background: themeState.theme.background,
      foreground: themeState.theme.foreground,
      cursor: themeState.theme.cursor,
      selectionBackground: themeState.theme.selection,
      black: themeState.theme.black,
      red: themeState.theme.red,
      green: themeState.theme.green,
      yellow: themeState.theme.yellow,
      blue: themeState.theme.blue,
      magenta: themeState.theme.magenta,
      cyan: themeState.theme.cyan,
      white: themeState.theme.white,
      brightBlack: themeState.theme.brightBlack,
      brightRed: themeState.theme.brightRed,
      brightGreen: themeState.theme.brightGreen,
      brightYellow: themeState.theme.brightYellow,
      brightBlue: themeState.theme.brightBlue,
      brightMagenta: themeState.theme.brightMagenta,
      brightCyan: themeState.theme.brightCyan,
      brightWhite: themeState.theme.brightWhite,
    },
  });

  const fitAddon = new FitAddon();
  const searchAddon = new SearchAddon();
  const webLinksAddon = new WebLinksAddon();

  xterm.loadAddon(fitAddon);
  xterm.loadAddon(searchAddon);
  xterm.loadAddon(webLinksAddon);

  const element = document.createElement('div');
  element.style.width = '100%';
  element.style.height = '100%';
  xterm.open(element);

  xterm.onData((data) => {
    const store = useTerminalStore.getState();
    if (store.broadcastMode && store.broadcastTargets.size > 0) {
      store.broadcastTargets.forEach((targetId) => {
        window.superTerminal.pty.write(targetId, data);
      });
    } else {
      window.superTerminal.pty.write(terminalId, data);
    }
  });

  xterm.onTitleChange((title) => {
    useTerminalStore.getState().setTerminalTitle(terminalId, title);
  });

  const { cols, rows } = xterm;
  window.superTerminal.pty.create(terminalId, cols, rows);

  const removeDataListener = window.superTerminal.pty.onData(terminalId, (data) => {
    const buf = xterm.buffer.active;
    const wasAtBottom = buf.viewportY >= buf.baseY;
    xterm.write(data, () => {
      if (wasAtBottom) {
        xterm.scrollToBottom();
      }
    });
  });

  const removeExitListener = window.superTerminal.pty.onExit(terminalId, () => {
    xterm.write('\r\n[Process exited]\r\n');
  });

  const entry: XtermEntry = { xterm, fitAddon, searchAddon, element, removeDataListener, removeExitListener };
  xtermRegistry.set(terminalId, entry);
  return entry;
}

/** Call fit() while preserving the terminal's scroll position */
export function safeFit(entry: XtermEntry) {
  const buf = entry.xterm.buffer.active;
  const wasAtBottom = buf.viewportY >= buf.baseY;

  entry.fitAddon.fit();

  // If user was at the bottom, ensure they stay there after reflow.
  // Otherwise let xterm's built-in reflow handle scroll preservation —
  // manually restoring a saved viewportY breaks when line count changes
  // due to reflow (e.g. full scrollback buffer with wrapping lines).
  if (wasAtBottom) {
    entry.xterm.scrollToBottom();
  }
}

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
