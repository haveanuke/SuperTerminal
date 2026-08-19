import { Terminal } from '@xterm/xterm';
import { WebglAddon } from '@xterm/addon-webgl';
import { fitTerminal } from './lib/xterm-fit';
import { registerWebLinks } from './lib/xterm-web-links';
import { useThemeStore } from './stores/theme-store';
import { useUIStore } from './stores/ui-store';
import { useTerminalStore } from './stores/terminal-store';
import { toastError } from './stores/toast-store';

export interface XtermEntry {
  xterm: Terminal;
  element: HTMLDivElement;
  webglAddon: WebglAddon | null;
  removeDataListener: () => void;
  removeExitListener: () => void;
  removeLinkProvider: () => void;
}

/**
 * GPU renderer toggle. WebGL draws glyphs (and the cursor) into a canvas —
 * fast, and immune to the WebKit CSS-painting quirks of the DOM renderer
 * (root cause of the invisible-cursor bug: with a background image active,
 * the DOM renderer's CSS-painted cursor never showed in WKWebView). The
 * addon supports allowTransparency, so it stays on even with background
 * images; DOM renderer remains only as a no-GL fallback.
 */
export function setWebglEnabled(entry: XtermEntry, enabled: boolean) {
  if (enabled && !entry.webglAddon) {
    try {
      const addon = new WebglAddon();
      addon.onContextLoss(() => {
        addon.dispose();
        entry.webglAddon = null;
      });
      entry.xterm.loadAddon(addon);
      entry.webglAddon = addon;
    } catch {
      entry.webglAddon = null; // no GL context -> stay on DOM renderer
    }
  } else if (!enabled && entry.webglAddon) {
    entry.webglAddon.dispose();
    entry.webglAddon = null;
  }
}

export const xtermRegistry = new Map<string, XtermEntry>();

export function getOrCreateXterm(terminalId: string): XtermEntry {
  const existing = xtermRegistry.get(terminalId);
  if (existing) return existing;

  const themeState = useThemeStore.getState();
  const uiState = useUIStore.getState();

  const xterm = new Terminal({
    fontSize: uiState.fontSize,
    fontFamily: uiState.fontFamily,
    scrollback: 10000,
    cursorBlink: true,
    cursorStyle: 'bar',
    // WebKit fails to paint the default 1px hairline bar (inset box-shadow);
    // 2px paints reliably and reads better on Retina anyway.
    cursorWidth: 2,
    // Option+Click moves the shell cursor via synthesized arrow keys
    // (iTerm/VS Code convention).
    altClickMovesCursor: true,
    allowProposedApi: true,
    allowTransparency: !!uiState.backgroundImage,
    theme: {
      // Zero-alpha hex, not the 'transparent' keyword: the WebGL renderer's
      // color parser treats the keyword as opaque black.
      background: uiState.backgroundImage ? '#00000000' : themeState.theme.background,
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

  const writeToPty = (data: string) => {
    const store = useTerminalStore.getState();
    if (store.broadcastMode && store.broadcastTargets.size > 0) {
      window.superTerminal.pty.writeBroadcast([...store.broadcastTargets], data);
    } else {
      window.superTerminal.pty.write(terminalId, data);
    }
  };

  xterm.onData(writeToPty);

  xterm.attachCustomKeyEventHandler((e) => {
    if (e.type !== 'keydown') return true;
    const { metaKey, altKey, ctrlKey, key } = e;

    if (metaKey && !ctrlKey && !altKey) {
      switch (key) {
        case 'Backspace':
          writeToPty('\x15');
          e.preventDefault();
          return false;
        case 'ArrowLeft':
          writeToPty('\x01');
          e.preventDefault();
          return false;
        case 'ArrowRight':
          writeToPty('\x05');
          e.preventDefault();
          return false;
        case 'Delete':
          writeToPty('\x0b');
          e.preventDefault();
          return false;
        case 'Enter':
          writeToPty('\x1b\r');
          e.preventDefault();
          return false;
      }
    }

    if (altKey && !metaKey && !ctrlKey) {
      switch (key) {
        case 'Backspace':
          writeToPty('\x17');
          e.preventDefault();
          return false;
        case 'Delete':
          writeToPty('\x1bd');
          e.preventDefault();
          return false;
        case 'ArrowLeft':
          writeToPty('\x1bb');
          e.preventDefault();
          return false;
        case 'ArrowRight':
          writeToPty('\x1bf');
          e.preventDefault();
          return false;
      }
    }

    return true;
  });

  xterm.onTitleChange((title) => {
    useTerminalStore.getState().setTerminalTitle(terminalId, title);
  });

  // Plain-click cursor move: clicking within the line being typed jumps the
  // shell cursor there via synthesized arrow keys (Option+Click also works,
  // handled natively by xterm). Guards keep clicks harmless everywhere else:
  // only on the cursor's own row, only in the normal buffer at the bottom,
  // never while an app is tracking the mouse, never after a text selection.
  element.addEventListener('click', (e) => {
    if (e.altKey || e.metaKey || e.ctrlKey || e.shiftKey) return;
    if (xterm.hasSelection()) return;
    const buf = xterm.buffer.active;
    if (buf.type !== 'normal') return;
    if (buf.viewportY < buf.baseY) return; // scrolled into history
    if (xterm.modes.mouseTrackingMode !== 'none') return; // app owns the mouse
    const screen = element.querySelector('.xterm-screen');
    if (!screen) return;
    const rect = screen.getBoundingClientRect();
    if (rect.width === 0 || rect.height === 0) return;
    const col = Math.min(
      xterm.cols - 1,
      Math.max(0, Math.floor(((e.clientX - rect.left) / rect.width) * xterm.cols))
    );
    const row = Math.floor(((e.clientY - rect.top) / rect.height) * xterm.rows);
    if (row !== buf.cursorY) return; // same-line moves only
    const delta = col - buf.cursorX;
    if (delta === 0) return;
    const arrow = xterm.modes.applicationCursorKeysMode
      ? delta > 0 ? '\x1bOC' : '\x1bOD'
      : delta > 0 ? '\x1b[C' : '\x1b[D';
    writeToPty(arrow.repeat(Math.abs(delta)));
  });

  const { cols, rows } = xterm;
  const cwd = useTerminalStore.getState().terminals.get(terminalId)?.cwd;
  window.superTerminal.pty.create(terminalId, cols, rows, cwd).catch((err) => {
    const msg = err instanceof Error ? err.message : String(err);
    xterm.write(`\r\n[Failed to start terminal: ${msg}]\r\n`);
    toastError(`Failed to start terminal: ${msg}`);
  });

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

  const entry: XtermEntry = { xterm, element, webglAddon: null, removeDataListener, removeExitListener, removeLinkProvider: () => linkDisposable.dispose() };
  setWebglEnabled(entry, true);
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
