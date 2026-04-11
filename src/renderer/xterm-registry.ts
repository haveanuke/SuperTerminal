import { Terminal } from '@xterm/xterm';
import { fitTerminal, invalidateCharCache } from './lib/xterm-fit';
import { registerWebLinks } from './lib/xterm-web-links';
import { useThemeStore } from './stores/theme-store';
import { useTerminalStore } from './stores/terminal-store';

export interface XtermEntry {
  xterm: Terminal;
  element: HTMLDivElement;
  removeDataListener: () => void;
  removeExitListener: () => void;
  removeLinkProvider: () => void;
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
    allowTransparency: !!themeState.backgroundImage,
    theme: {
      background: themeState.backgroundImage ? 'transparent' : themeState.theme.background,
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

  const linkDisposable = registerWebLinks(xterm);

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

  const entry: XtermEntry = { xterm, element, removeDataListener, removeExitListener, removeLinkProvider: () => linkDisposable.dispose() };
  xtermRegistry.set(terminalId, entry);
  return entry;
}

/** Call fit() while preserving the terminal's scroll position */
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

export { invalidateCharCache } from './lib/xterm-fit';
