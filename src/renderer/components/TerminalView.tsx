import { useEffect, useRef, useCallback } from 'react';
import { useThemeStore } from '../stores/theme-store';
import { useUIStore } from '../stores/ui-store';
import { useTerminalStore } from '../stores/terminal-store';
import { findNext, clearDecorations } from '../lib/xterm-search';
import { xtermRegistry, getOrCreateXterm, safeFit, invalidateCharCache } from '../xterm-registry';

interface TerminalViewProps {
  terminalId: string;
}

export function TerminalView({ terminalId }: TerminalViewProps) {
  const containerRef = useRef<HTMLDivElement>(null);

  const theme = useThemeStore((s) => s.theme);
  const fontSize = useUIStore((s) => s.fontSize);
  const fontFamily = useUIStore((s) => s.fontFamily);
  const backgroundImage = useUIStore((s) => s.backgroundImage);
  const broadcastMode = useTerminalStore((s) => s.broadcastMode);
  const broadcastTargets = useTerminalStore((s) => s.broadcastTargets);
  const setActiveTerminalId = useTerminalStore((s) => s.setActiveTerminalId);
  const searchQuery = useTerminalStore((s) => s.searchQuery);
  const searchOpen = useTerminalStore((s) => s.searchOpen);

  const handleFocus = useCallback(() => {
    setActiveTerminalId(terminalId);
  }, [terminalId, setActiveTerminalId]);

  // Attach/detach the persistent xterm element
  useEffect(() => {
    if (!containerRef.current) return;
    const entry = getOrCreateXterm(terminalId);
    const container = containerRef.current;

    // Move the xterm DOM element into this container
    container.appendChild(entry.element);

    // Fit after attaching (new container may have different size)
    requestAnimationFrame(() => {
      try {
        safeFit(entry);
        window.superTerminal.pty.resize(terminalId, entry.xterm.cols, entry.xterm.rows);
      } catch {
        // ignore
      }
    });

    let resizeTimer: ReturnType<typeof setTimeout> | null = null;
    const resizeObserver = new ResizeObserver(() => {
      if (resizeTimer) clearTimeout(resizeTimer);
      resizeTimer = setTimeout(() => {
        try {
          if (entry.element.parentNode === container) {
            safeFit(entry);
            window.superTerminal.pty.resize(terminalId, entry.xterm.cols, entry.xterm.rows);
          }
        } catch {
          // ignore resize errors during teardown
        }
      }, 100);
    });
    resizeObserver.observe(container);

    return () => {
      if (resizeTimer) clearTimeout(resizeTimer);
      resizeObserver.disconnect();
      // Detach but don't destroy — the element stays in the registry
      if (entry.element.parentNode === container) {
        container.removeChild(entry.element);
      }
    };
  }, [terminalId]);

  // Update theme
  useEffect(() => {
    const entry = xtermRegistry.get(terminalId);
    if (entry) {
      entry.xterm.options.allowTransparency = !!backgroundImage;
      entry.xterm.options.theme = {
        background: backgroundImage ? 'transparent' : theme.background,
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
      };
    }
  }, [theme, terminalId, backgroundImage]);

  // Update font
  useEffect(() => {
    const entry = xtermRegistry.get(terminalId);
    if (entry) {
      entry.xterm.options.fontSize = fontSize;
      entry.xterm.options.fontFamily = fontFamily;
      invalidateCharCache();
      safeFit(entry);
    }
  }, [fontSize, fontFamily, terminalId]);

  // Search
  useEffect(() => {
    const entry = xtermRegistry.get(terminalId);
    if (!entry) return;
    if (searchOpen && searchQuery) {
      findNext(entry.xterm, searchQuery, { caseSensitive: false });
    } else {
      clearDecorations(entry.xterm);
    }
  }, [searchQuery, searchOpen, terminalId]);

  // Auto-run
  const autoRun = useTerminalStore((s) => s.terminals.get(terminalId)?.autoRun);
  const autoRunInFlightRef = useRef(false);

  useEffect(() => {
    if (!autoRun?.enabled || !autoRun.command) return;
    const intervalMs = autoRun.intervalSeconds * 1000;
    const escDelay = (autoRun.escapeDelaySecs || 2) * 1000;
    const timers: ReturnType<typeof setTimeout>[] = [];
    autoRunInFlightRef.current = false;

    const runCommand = () => {
      if (autoRunInFlightRef.current) return;
      autoRunInFlightRef.current = true;
      window.superTerminal.pty.write(terminalId, autoRun.command);
      const enterTimer = setTimeout(() => {
        window.superTerminal.pty.write(terminalId, '\r');
        if (!autoRun.sendEscape) autoRunInFlightRef.current = false;
      }, 50);
      timers.push(enterTimer);
      if (autoRun.sendEscape) {
        const escTimer = setTimeout(() => {
          window.superTerminal.pty.write(terminalId, '\x1b');
          autoRunInFlightRef.current = false;
        }, escDelay);
        timers.push(escTimer);
      }
    };

    runCommand();
    const interval = setInterval(runCommand, intervalMs);

    return () => {
      clearInterval(interval);
      timers.forEach(clearTimeout);
      autoRunInFlightRef.current = false;
    };
  }, [terminalId, autoRun?.enabled, autoRun?.command, autoRun?.intervalSeconds, autoRun?.sendEscape, autoRun?.escapeDelaySecs]);

  const isBroadcastTarget = broadcastMode && broadcastTargets.has(terminalId);
  const swapSource = useTerminalStore((s) => s.swapSource);
  const isSwapSource = swapSource === terminalId;
  const isSwapTarget = swapSource !== null && swapSource !== terminalId;

  const borderColor = isSwapSource
    ? theme.yellow
    : isSwapTarget
    ? `${theme.yellow}88`
    : isBroadcastTarget
    ? theme.uiAccent
    : 'transparent';

  return (
    <div
      className={`terminal-view${backgroundImage ? ' terminal-transparent' : ''}`}
      onFocus={handleFocus}
      onClick={handleFocus}
      style={{
        position: 'relative',
        width: '100%',
        height: '100%',
        border: `2px solid ${borderColor}`,
        boxSizing: 'border-box',
      }}
    >
      <div ref={containerRef} style={{ width: '100%', height: '100%' }} />
    </div>
  );
}
