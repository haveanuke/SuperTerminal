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
