// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

const h = vi.hoisted(() => ({
  bridge: {
    resolveRepo: vi.fn(),
    status: vi.fn(),
    graph: vi.fn(),
    stage: vi.fn(),
    stageAll: vi.fn(),
    unstage: vi.fn(),
    unstageAll: vi.fn(),
    discard: vi.fn(),
    commit: vi.fn(),
    push: vi.fn(),
    pull: vi.fn(),
    fetch: vi.fn(),
  },
  ptyCwd: vi.fn(),
}));

vi.mock('../lib/git-bridge', () => ({ gitBridge: h.bridge }));
vi.mock('../xterm-registry', () => ({ destroyXterm: vi.fn() }));

import { resetGitEngineForTests, useGitStore } from './git-store';
import { useTerminalStore } from './terminal-store';

function report(overrides: Partial<Record<string, unknown>> = {}) {
  return {
    branch: 'main',
    detached: null,
    upstream: 'origin/main',
    ahead: 0,
    behind: 0,
    unborn: false,
    entries: [],
    ...overrides,
  };
}

const repoA = { repoId: 'repo-0', displayName: 'a', root: '/a' };
const repoB = { repoId: 'repo-1', displayName: 'b', root: '/b' };

function flush() {
  // settle chained microtasks from the async follow/refresh pipeline
  return new Promise((r) => setTimeout(r, 0));
}

describe('git-store', () => {
  beforeEach(() => {
    (window as unknown as { superTerminal: { pty: { cwd: unknown } } }).superTerminal = {
      pty: { cwd: h.ptyCwd },
    } as never;
    Object.values(h.bridge).forEach((fn) => fn.mockReset());
    h.ptyCwd.mockReset();
    h.bridge.graph.mockResolvedValue({ rows: [], laneCount: 0 });
    // ensure closed state + fresh engine between tests
    if (useGitStore.getState().open) useGitStore.getState().toggle();
    resetGitEngineForTests();
    useGitStore.setState({ repo: null, report: null, graph: null, targetPath: null, busy: false });
  });

  afterEach(() => {
    if (useGitStore.getState().open) useGitStore.getState().toggle();
  });

  it('closed sidebar: focus changes cause zero bridge calls', async () => {
    h.ptyCwd.mockResolvedValue('/a');
    useTerminalStore.getState().setActiveTerminalId('t-x');
    await flush();
    expect(h.ptyCwd).not.toHaveBeenCalled();
    expect(h.bridge.resolveRepo).not.toHaveBeenCalled();
  });

  it('opening resolves the focused terminal and loads status + graph', async () => {
    h.ptyCwd.mockResolvedValue('/a');
    h.bridge.resolveRepo.mockResolvedValue(repoA);
    h.bridge.status.mockResolvedValue(report());
    useGitStore.getState().toggle();
    await flush();
    await flush();
    expect(useGitStore.getState().repo?.repoId).toBe('repo-0');
    expect(useGitStore.getState().report?.branch).toBe('main');
    expect(h.bridge.graph).toHaveBeenCalled();
  });

  it('non-repo cwd shows empty state with the path; null cwd keeps last repo', async () => {
    h.ptyCwd.mockResolvedValue('/a');
    h.bridge.resolveRepo.mockResolvedValue(repoA);
    h.bridge.status.mockResolvedValue(report());
    useGitStore.getState().toggle();
    await flush();
    await flush();
    // focus a pane whose cwd is not a repo
    h.ptyCwd.mockResolvedValue('/not-a-repo');
    h.bridge.resolveRepo.mockResolvedValue(null);
    useTerminalStore.getState().setActiveTerminalId('t-2');
    await flush();
    await flush();
    expect(useGitStore.getState().repo).toBeNull();
    expect(useGitStore.getState().targetPath).toBe('/not-a-repo');
    // now a pane whose shell has no cwd (null) -> keep the last state
    h.ptyCwd.mockResolvedValue(null);
    useTerminalStore.getState().setActiveTerminalId('t-3');
    await flush();
    expect(useGitStore.getState().targetPath).toBe('/not-a-repo');
  });

  it('stale status result for a previous repo is dropped', async () => {
    h.ptyCwd.mockResolvedValue('/a');
    h.bridge.resolveRepo.mockResolvedValue(repoA);
    let resolveSlow: (v: unknown) => void = () => {};
    h.bridge.status.mockImplementation(
      () => new Promise((resolve) => (resolveSlow = resolve))
    );
    useGitStore.getState().toggle();
    await flush();
    await flush();
    // switch focus to repo B while A's status hangs
    h.ptyCwd.mockResolvedValue('/b');
    h.bridge.resolveRepo.mockResolvedValue(repoB);
    h.bridge.status.mockResolvedValue(report({ branch: 'b-branch' }));
    useTerminalStore.getState().setActiveTerminalId('t-b');
    await flush();
    await flush();
    // A's slow status resolves late — must not overwrite B's world
    resolveSlow(report({ branch: 'stale-a' }));
    await flush();
    expect(useGitStore.getState().report?.branch).toBe('b-branch');
  });

  it('per-repo inflight: focusing B while A status hangs still loads B immediately', async () => {
    h.ptyCwd.mockResolvedValue('/a');
    h.bridge.resolveRepo.mockResolvedValue(repoA);
    h.bridge.status.mockImplementation(
      (repoId: string) =>
        repoId === 'repo-0'
          ? new Promise(() => {}) // A hangs forever
          : Promise.resolve(report({ branch: 'b-branch' }))
    );
    useGitStore.getState().toggle();
    await flush();
    await flush();
    h.ptyCwd.mockResolvedValue('/b');
    h.bridge.resolveRepo.mockResolvedValue(repoB);
    useTerminalStore.getState().setActiveTerminalId('t-b2');
    await flush();
    await flush();
    expect(useGitStore.getState().report?.branch).toBe('b-branch');
  });

  it('busy status responses are dropped silently; busy gates a second action', async () => {
    h.ptyCwd.mockResolvedValue('/a');
    h.bridge.resolveRepo.mockResolvedValue(repoA);
    h.bridge.status.mockRejectedValue(new Error('busy'));
    useGitStore.getState().toggle();
    await flush();
    await flush();
    expect(useGitStore.getState().report).toBeNull(); // dropped, no crash

    h.bridge.stage.mockImplementation(() => new Promise(() => {})); // hangs
    void useGitStore.getState().runAction('stage', ['x']);
    await flush();
    expect(useGitStore.getState().busy).toBe(true);
    void useGitStore.getState().runAction('commit', 'msg');
    await flush();
    expect(h.bridge.commit).not.toHaveBeenCalled();
  });

  it('branch identity change triggers a graph refresh', async () => {
    h.ptyCwd.mockResolvedValue('/a');
    h.bridge.resolveRepo.mockResolvedValue(repoA);
    h.bridge.status.mockResolvedValue(report());
    useGitStore.getState().toggle();
    await flush();
    await flush();
    const callsAfterOpen = h.bridge.graph.mock.calls.length;
    // same-branch action result -> no extra graph call
    h.bridge.stage.mockResolvedValue({ report: report(), skipped: 0 });
    await useGitStore.getState().runAction('stage', ['f']);
    const callsAfterSame = h.bridge.graph.mock.calls.length;
    expect(callsAfterSame).toBe(callsAfterOpen);
    // branch identity change -> graph refresh
    h.bridge.stage.mockResolvedValue({ report: report({ branch: 'other' }), skipped: 0 });
    await useGitStore.getState().runAction('stage', ['f']);
    await flush();
    expect(h.bridge.graph.mock.calls.length).toBeGreaterThan(callsAfterSame);
  });

  it('action errors keep the previous report', async () => {
    h.ptyCwd.mockResolvedValue('/a');
    h.bridge.resolveRepo.mockResolvedValue(repoA);
    h.bridge.status.mockResolvedValue(report());
    useGitStore.getState().toggle();
    await flush();
    await flush();
    const before = useGitStore.getState().report;
    expect(before).not.toBeNull();
    h.bridge.discard.mockRejectedValue(new Error('state changed, refresh'));
    await useGitStore.getState().runAction('discard', ['gone.txt']);
    expect(useGitStore.getState().report).toBe(before);
    expect(useGitStore.getState().busy).toBe(false);
  });
});
