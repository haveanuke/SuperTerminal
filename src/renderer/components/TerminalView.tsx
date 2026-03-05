import { useEffect, useRef, useCallback } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { SearchAddon } from '@xterm/addon-search';
import { WebLinksAddon } from '@xterm/addon-web-links';
import { useThemeStore } from '../stores/theme-store';
import { useTerminalStore } from '../stores/terminal-store';

interface TerminalViewProps {
  terminalId: string;
}

export function TerminalView({ terminalId }: TerminalViewProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const xtermRef = useRef<Terminal | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  const searchAddonRef = useRef<SearchAddon | null>(null);
  const cleanupRef = useRef<(() => void) | null>(null);

  const theme = useThemeStore((s) => s.theme);
  const fontSize = useThemeStore((s) => s.fontSize);
  const fontFamily = useThemeStore((s) => s.fontFamily);
  const broadcastMode = useTerminalStore((s) => s.broadcastMode);
  const broadcastTargets = useTerminalStore((s) => s.broadcastTargets);
  const setActiveTerminalId = useTerminalStore((s) => s.setActiveTerminalId);
  const setTerminalTitle = useTerminalStore((s) => s.setTerminalTitle);
  const searchQuery = useTerminalStore((s) => s.searchQuery);
  const searchOpen = useTerminalStore((s) => s.searchOpen);

  const handleFocus = useCallback(() => {
    setActiveTerminalId(terminalId);
  }, [terminalId, setActiveTerminalId]);

  useEffect(() => {
    if (!containerRef.current) return;

    const xterm = new Terminal({
      fontSize,
      fontFamily,
      cursorBlink: true,
      cursorStyle: 'bar',
      allowProposedApi: true,
      theme: {
        background: theme.background,
        foreground: theme.foreground,
        cursor: theme.cursor,
        selectionBackground: theme.selection,
        black: theme.black,
        red: theme.red,
        green: theme.green,
        yellow: theme.yellow,
        blue: theme.blue,
        magenta: theme.magenta,
        cyan: theme.cyan,
        white: theme.white,
        brightBlack: theme.brightBlack,
        brightRed: theme.brightRed,
        brightGreen: theme.brightGreen,
        brightYellow: theme.brightYellow,
        brightBlue: theme.brightBlue,
        brightMagenta: theme.brightMagenta,
        brightCyan: theme.brightCyan,
        brightWhite: theme.brightWhite,
      },
    });

    const fitAddon = new FitAddon();
    const searchAddon = new SearchAddon();
    const webLinksAddon = new WebLinksAddon();

    xterm.loadAddon(fitAddon);
    xterm.loadAddon(searchAddon);
    xterm.loadAddon(webLinksAddon);

    xterm.open(containerRef.current);
    fitAddon.fit();

    xtermRef.current = xterm;
    fitAddonRef.current = fitAddon;
    searchAddonRef.current = searchAddon;

    const { cols, rows } = xterm;
    window.superTerminal.pty.create(terminalId, cols, rows);

    const removeDataListener = window.superTerminal.pty.onData(terminalId, (data) => {
      xterm.write(data);
    });

    const removeExitListener = window.superTerminal.pty.onExit(terminalId, () => {
      xterm.write('\r\n[Process exited]\r\n');
    });

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
      setTerminalTitle(terminalId, title);
    });

    const resizeObserver = new ResizeObserver(() => {
      try {
        fitAddon.fit();
        window.superTerminal.pty.resize(terminalId, xterm.cols, xterm.rows);
      } catch {
        // ignore resize errors during teardown
      }
    });
    resizeObserver.observe(containerRef.current);

    cleanupRef.current = () => {
      removeDataListener();
      removeExitListener();
      resizeObserver.disconnect();
      xterm.dispose();
    };

    return () => {
      cleanupRef.current?.();
    };
  }, [terminalId]);

  // Update theme
  useEffect(() => {
    xtermRef.current?.options.theme && (xtermRef.current.options.theme = {
      background: theme.background,
      foreground: theme.foreground,
      cursor: theme.cursor,
      selectionBackground: theme.selection,
      black: theme.black,
      red: theme.red,
      green: theme.green,
      yellow: theme.yellow,
      blue: theme.blue,
      magenta: theme.magenta,
      cyan: theme.cyan,
      white: theme.white,
      brightBlack: theme.brightBlack,
      brightRed: theme.brightRed,
      brightGreen: theme.brightGreen,
      brightYellow: theme.brightYellow,
      brightBlue: theme.brightBlue,
      brightMagenta: theme.brightMagenta,
      brightCyan: theme.brightCyan,
      brightWhite: theme.brightWhite,
    });
  }, [theme]);

  // Update font
  useEffect(() => {
    if (xtermRef.current) {
      xtermRef.current.options.fontSize = fontSize;
      xtermRef.current.options.fontFamily = fontFamily;
      fitAddonRef.current?.fit();
    }
  }, [fontSize, fontFamily]);

  // Search
  useEffect(() => {
    if (searchOpen && searchQuery) {
      searchAddonRef.current?.findNext(searchQuery, { regex: false, caseSensitive: false });
    } else {
      searchAddonRef.current?.clearDecorations();
    }
  }, [searchQuery, searchOpen]);

  const isBroadcastTarget = broadcastMode && broadcastTargets.has(terminalId);

  return (
    <div
      className="terminal-view"
      onFocus={handleFocus}
      onClick={handleFocus}
      style={{
        position: 'relative',
        width: '100%',
        height: '100%',
        border: isBroadcastTarget ? `2px solid ${theme.uiAccent}` : '2px solid transparent',
        boxSizing: 'border-box',
      }}
    >
      <div ref={containerRef} style={{ width: '100%', height: '100%' }} />
    </div>
  );
}
