// @vitest-environment jsdom
import { describe, it, expect, vi } from 'vitest';

vi.mock('../xterm-registry', () => ({ destroyXterm: vi.fn() }));

import { useTerminalStore } from './terminal-store';

describe('splitPane cwd inheritance', () => {
  it('passes the source cwd to the new terminal', () => {
    const s = useTerminalStore.getState();
    const tab = s.tabs[0];
    const sourceId = (tab.pane as { type: 'terminal'; terminalId: string }).terminalId;
    s.splitPane(tab.id, sourceId, 'horizontal', '/tmp/somewhere');
    const terms = [...useTerminalStore.getState().terminals.values()];
    expect(terms.some((t) => t.cwd === '/tmp/somewhere')).toBe(true);
  });

  it('splits without cwd leave the new terminal cwd undefined', () => {
    const s = useTerminalStore.getState();
    const tab = s.tabs[0];
    const before = new Set(useTerminalStore.getState().terminals.keys());
    const anyId = [...before][0];
    s.splitPane(tab.id, anyId, 'vertical');
    const added = [...useTerminalStore.getState().terminals.entries()].filter(([id]) => !before.has(id));
    expect(added.length).toBe(1);
    expect(added[0][1].cwd).toBeUndefined();
  });
});
