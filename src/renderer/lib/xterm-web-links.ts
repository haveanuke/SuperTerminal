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
