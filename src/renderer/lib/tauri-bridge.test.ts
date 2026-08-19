// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest';

// vi.mock factories are hoisted above module-level consts — shared state must
// come from vi.hoisted or the factories throw ReferenceError.
const h = vi.hoisted(() => {
  const invokeMock = vi.fn();
  const holder: { lastChannel: { onmessage: (msg: unknown) => void } | null } = { lastChannel: null };
  class MockChannel {
    onmessage: (msg: unknown) => void = () => {};
    constructor() {
      holder.lastChannel = this;
    }
  }
  return { invokeMock, holder, MockChannel };
});

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => h.invokeMock(...args),
  Channel: h.MockChannel,
  convertFileSrc: (p: string) => `asset://converted${p}`,
}));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));

import { installTauriBridge } from './tauri-bridge';

function utf8(bytes: number[]): ArrayBuffer {
  return new Uint8Array(bytes).buffer;
}

describe('tauri-bridge pty', () => {
  beforeEach(() => {
    h.invokeMock.mockReset().mockResolvedValue(true);
    h.holder.lastChannel = null;
    installTauriBridge();
  });

  it('buffers data until the first data subscriber, then flushes in order', async () => {
    await window.superTerminal.pty.create('a', 80, 24);
    h.holder.lastChannel!.onmessage(utf8([104, 105])); // "hi"
    const seen: string[] = [];
    window.superTerminal.pty.onData('a', (d) => seen.push(d));
    expect(seen).toEqual(['hi']);
    h.holder.lastChannel!.onmessage(utf8([33])); // "!"
    expect(seen).toEqual(['hi', '!']);
  });

  it('reassembles split multibyte sequences across frames', async () => {
    await window.superTerminal.pty.create('b', 80, 24);
    const seen: string[] = [];
    window.superTerminal.pty.onData('b', (d) => seen.push(d));
    // "é" = 0xC3 0xA9 split across two frames
    h.holder.lastChannel!.onmessage(utf8([0xc3]));
    h.holder.lastChannel!.onmessage(utf8([0xa9]));
    expect(seen.join('')).toBe('é');
  });

  it('latches exit until an exit subscriber attaches, after flushing data', async () => {
    await window.superTerminal.pty.create('c', 80, 24);
    h.holder.lastChannel!.onmessage(utf8([120])); // "x"
    h.holder.lastChannel!.onmessage({ exit: 3 });
    const data: string[] = [];
    const exits: number[] = [];
    window.superTerminal.pty.onData('c', (d) => data.push(d));
    window.superTerminal.pty.onExit('c', (code) => exits.push(code));
    expect(data).toEqual(['x']);
    expect(exits).toEqual([3]);
  });

  it('fans out to multiple data subscribers and honors unsubscribe', async () => {
    await window.superTerminal.pty.create('d', 80, 24);
    const a: string[] = [];
    const b: string[] = [];
    const unsubA = window.superTerminal.pty.onData('d', (d) => a.push(d));
    window.superTerminal.pty.onData('d', (d) => b.push(d));
    h.holder.lastChannel!.onmessage(utf8([49]));
    unsubA();
    h.holder.lastChannel!.onmessage(utf8([50]));
    expect(a).toEqual(['1']);
    expect(b).toEqual(['1', '2']);
  });

  it('duplicate create keeps the original channel; create failure cleans up', async () => {
    await window.superTerminal.pty.create('e', 80, 24);
    const first = h.holder.lastChannel;
    await window.superTerminal.pty.create('e', 80, 24);
    expect(h.holder.lastChannel).toBe(first); // no second Channel constructed
    h.invokeMock.mockRejectedValueOnce(new Error('spawn failed'));
    await expect(window.superTerminal.pty.create('f', 80, 24)).rejects.toThrow('spawn failed');
    // after failed create, a retry constructs a fresh channel
    h.invokeMock.mockResolvedValueOnce(true);
    await window.superTerminal.pty.create('f', 80, 24);
    expect(h.holder.lastChannel).not.toBe(first);
  });

  it('passes through write/resize/dispose and session calls', async () => {
    await window.superTerminal.pty.write('a', 'ls\n');
    expect(h.invokeMock).toHaveBeenCalledWith('pty_write', { id: 'a', data: 'ls\n' });
    await window.superTerminal.session.save('s', { x: 1 });
    expect(h.invokeMock).toHaveBeenCalledWith('session_save', { name: 's', layout: { x: 1 } });
  });
});
