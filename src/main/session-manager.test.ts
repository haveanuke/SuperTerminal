import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import fs from 'fs';
import os from 'os';
import path from 'path';

// session-manager imports `electron` for the default ctor path; stub it before importing.
vi.mock('electron', () => ({
  app: { getPath: () => os.tmpdir() },
}));

import { SessionManager, SessionLoadError } from './session-manager';

describe('SessionManager', () => {
  let dir: string;
  let mgr: SessionManager;

  const validLayout = {
    tabs: [
      {
        id: 'tab-1',
        label: 'Main',
        pane: { type: 'terminal', terminalId: 't1' },
      },
    ],
    activeTabId: 'tab-1',
  };

  beforeEach(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'st-session-test-'));
    mgr = new SessionManager(dir);
  });

  afterEach(() => {
    fs.rmSync(dir, { recursive: true, force: true });
  });

  it('saves and loads a session round-trip', () => {
    mgr.save('work', validLayout);
    const loaded = mgr.load('work');
    expect(loaded).not.toBeNull();
    expect(loaded?.name).toBe('work');
    expect(loaded?.layout).toEqual(validLayout);
    expect(typeof loaded?.savedAt).toBe('string');
  });

  it('returns null for a missing session', () => {
    expect(mgr.load('nothing-here')).toBeNull();
  });

  it('throws SessionLoadError(invalid_json) on corrupt JSON', () => {
    fs.writeFileSync(path.join(dir, 'broken.json'), '{ this is not json');
    expect(() => mgr.load('broken')).toThrow(SessionLoadError);
    try {
      mgr.load('broken');
    } catch (err) {
      expect(err).toBeInstanceOf(SessionLoadError);
      expect((err as SessionLoadError).reason).toBe('invalid_json');
    }
  });

  it('throws SessionLoadError(schema_mismatch) on wrong shape', () => {
    fs.writeFileSync(path.join(dir, 'bad.json'), JSON.stringify({ name: 'x', layout: { tabs: 'not-an-array' } }));
    expect(() => mgr.load('bad')).toThrow(SessionLoadError);
    try {
      mgr.load('bad');
    } catch (err) {
      expect((err as SessionLoadError).reason).toBe('schema_mismatch');
    }
  });

  it('rejects sessions containing prototype-pollution keys', () => {
    // Raw JSON — object literals would let JS interpret __proto__ as prototype-set syntax
    // and JSON.stringify would strip it. The whole point is that JSON.parse creates
    // __proto__ as an own enumerable property.
    const malicious =
      '{"name":"evil","savedAt":"now","layout":{"activeTabId":"tab-1","tabs":[' +
      '{"id":"tab-1","label":"x","pane":{"type":"terminal","terminalId":"t1"},"__proto__":{"polluted":true}}' +
      ']}}';
    fs.writeFileSync(path.join(dir, 'evil.json'), malicious);
    expect(() => mgr.load('evil')).toThrow(SessionLoadError);
    try {
      mgr.load('evil');
    } catch (err) {
      expect((err as SessionLoadError).reason).toBe('schema_mismatch');
    }
  });

  it('accepts a nested split pane layout', () => {
    const split = {
      tabs: [
        {
          id: 'tab-1',
          label: 'Split',
          pane: {
            type: 'split',
            direction: 'horizontal',
            children: [
              { type: 'terminal', terminalId: 't1' },
              { type: 'terminal', terminalId: 't2' },
            ],
            sizes: [50, 50],
          },
        },
      ],
      activeTabId: 'tab-1',
    };
    mgr.save('split', split);
    expect(mgr.load('split')?.layout).toEqual(split);
  });

  it('lists and deletes saved sessions', () => {
    mgr.save('a', validLayout);
    mgr.save('b', validLayout);
    expect(mgr.list().sort()).toEqual(['a', 'b']);
    expect(mgr.delete('a')).toBe(true);
    expect(mgr.list()).toEqual(['b']);
    expect(mgr.delete('a')).toBe(false);
  });

  it('sanitizes session names', () => {
    mgr.save('../../etc/passwd', validLayout);
    // Sanitized name replaces non-[a-zA-Z0-9_-] with _
    expect(mgr.list()).toContain('______etc_passwd');
  });
});
