import { createStore } from '../lib/create-store';
import {
  gitBridge,
  type ActionResult,
  type GraphData,
  type RepoInfo,
  type StatusReport,
} from '../lib/git-bridge';
import { toastError, toastInfo } from './toast-store';
import { useTerminalStore } from './terminal-store';

const BASE_POLL_MS = 3000;
const MAX_POLL_MS = 15000;
const SLOW_STATUS_MS = 500;
export const GRAPH_STEP = 300;
export const GRAPH_MAX = 1000;

export type GitActionKind =
  | 'stage'
  | 'stageAll'
  | 'unstage'
  | 'unstageAll'
  | 'discard'
  | 'commit'
  | 'push'
  | 'pull'
  | 'fetch'
  | 'sync';

interface GitStore {
  open: boolean;
  repo: RepoInfo | null;
  /** Resolved cwd of the focused pane — shown in the not-a-repo empty state. */
  targetPath: string | null;
  report: StatusReport | null;
  graph: GraphData | null;
  graphLimit: number;
  busy: boolean;

  toggle: () => void;
  refresh: () => void;
  loadMoreGraph: () => void;
  runAction: (kind: GitActionKind, arg?: string[] | string) => Promise<void>;
}

// Module-level engine state (not reactive):
let generation = 0;
const inflight = new Set<string>();
let pollMs = BASE_POLL_MS;
let pollTimer: number | null = null;
let unsubFocus: (() => void) | null = null;
let lastFocused: string | null = null;

/** Test-only: clear module-level engine state between vitest cases. */
export function resetGitEngineForTests() {
  generation += 1;
  inflight.clear();
  pollMs = BASE_POLL_MS;
  stopPoll();
  unsubscribeFocus();
}

function errMsg(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function reportError(context: string, message: string) {
  if (message === 'busy') return; // status skipped while an action runs — silent
  console.warn(`[git] ${context}:`, message);
  const capped = message.length > 200 ? `${message.slice(0, 197)}...` : message;
  toastError(`git ${context}: ${capped}`);
}

function branchIdentity(report: StatusReport): string {
  return `${report.branch}|${report.detached}|${report.unborn}`;
}

async function refreshStatus(): Promise<void> {
  const s = useGitStore.getState();
  const repo = s.repo;
  if (!repo || !s.open || inflight.has(repo.repoId)) return;
  inflight.add(repo.repoId);
  const gen = generation;
  const started = Date.now();
  try {
    const report = await gitBridge.status(repo.repoId);
    const elapsed = Date.now() - started;
    pollMs = elapsed > SLOW_STATUS_MS ? Math.min(pollMs * 2, MAX_POLL_MS) : BASE_POLL_MS;
    applyReport(repo.repoId, gen, report);
  } catch (err) {
    reportError('status', errMsg(err));
  } finally {
    inflight.delete(repo.repoId);
  }
}

function applyReport(repoId: string, gen: number, report: StatusReport) {
  const s = useGitStore.getState();
  if (gen !== generation || !s.repo || s.repo.repoId !== repoId) return; // stale
  const prevIdentity = s.report ? branchIdentity(s.report) : null;
  useGitStore.setState({ report });
  if (prevIdentity !== null && prevIdentity !== branchIdentity(report)) {
    void refreshGraph();
  }
}

async function refreshGraph(): Promise<void> {
  const s = useGitStore.getState();
  const repo = s.repo;
  if (!repo || !s.open) return;
  const gen = generation;
  try {
    const graph = await gitBridge.graph(repo.repoId, s.graphLimit);
    const now = useGitStore.getState();
    if (gen !== generation || !now.repo || now.repo.repoId !== repo.repoId) return;
    useGitStore.setState({ graph });
  } catch (err) {
    reportError('graph', errMsg(err));
  }
}

/** Focused pane changed (or the sidebar just opened): retarget the repo. */
async function follow(terminalId: string | null): Promise<void> {
  if (!terminalId || !useGitStore.getState().open) return;
  const gen = ++generation;
  let cwd: string | null = null;
  try {
    cwd = await window.superTerminal.pty.cwd(terminalId);
  } catch {
    return;
  }
  if (gen !== generation || !useGitStore.getState().open) return;
  if (cwd === null) return; // shell gone — keep last repo
  let info: RepoInfo | null = null;
  try {
    info = await gitBridge.resolveRepo(cwd);
  } catch {
    return;
  }
  if (gen !== generation || !useGitStore.getState().open) return;
  if (!info) {
    // A real cwd that is not inside a repo -> explicit empty state.
    useGitStore.setState({ repo: null, targetPath: cwd, report: null, graph: null });
    return;
  }
  const prev = useGitStore.getState().repo;
  if (prev && prev.repoId === info.repoId) {
    useGitStore.setState({ targetPath: cwd });
    return;
  }
  useGitStore.setState({
    repo: info,
    targetPath: cwd,
    report: null,
    graph: null,
    graphLimit: GRAPH_STEP,
  });
  void refreshStatus();
  void refreshGraph();
}

function subscribeFocus() {
  if (unsubFocus) return;
  lastFocused = useTerminalStore.getState().activeTerminalId;
  unsubFocus = useTerminalStore.subscribe(() => {
    const id = useTerminalStore.getState().activeTerminalId;
    if (id !== lastFocused) {
      lastFocused = id;
      void follow(id);
    }
  });
}

function unsubscribeFocus() {
  unsubFocus?.();
  unsubFocus = null;
}

/** Chained timeout so each cycle uses the latest backoff value. */
function schedulePoll() {
  if (pollTimer !== null) window.clearTimeout(pollTimer);
  pollTimer = window.setTimeout(async () => {
    pollTimer = null;
    if (!useGitStore.getState().open) return;
    await refreshStatus();
    if (useGitStore.getState().open) schedulePoll();
  }, pollMs);
}

function stopPoll() {
  if (pollTimer !== null) window.clearTimeout(pollTimer);
  pollTimer = null;
}

async function pushFlow(repoId: string): Promise<ActionResult | null> {
  try {
    return await gitBridge.push(repoId, false);
  } catch (err) {
    if (errMsg(err) === 'no upstream') {
      if (window.confirm('This branch has no upstream. Publish it to origin?')) {
        return await gitBridge.push(repoId, true);
      }
      return null;
    }
    throw err;
  }
}

function applyActionResult(repoId: string, result: ActionResult) {
  const s = useGitStore.getState();
  if (!s.repo || s.repo.repoId !== repoId) return;
  const prevIdentity = s.report ? branchIdentity(s.report) : null;
  useGitStore.setState({ report: result.report });
  if (prevIdentity !== null && prevIdentity !== branchIdentity(result.report)) {
    void refreshGraph();
  }
  if (result.skipped > 0) {
    toastInfo(`${result.skipped} ${result.skipped === 1 ? 'entry' : 'entries'} skipped (non-actionable paths)`);
  }
}

export const useGitStore = createStore<GitStore>((set, get) => ({
  open: false,
  repo: null,
  targetPath: null,
  report: null,
  graph: null,
  graphLimit: GRAPH_STEP,
  busy: false,

  toggle: () => {
    const open = !get().open;
    set({ open });
    if (open) {
      subscribeFocus();
      void follow(useTerminalStore.getState().activeTerminalId);
      pollMs = BASE_POLL_MS;
      schedulePoll();
    } else {
      unsubscribeFocus();
      stopPoll();
    }
  },

  refresh: () => {
    void refreshStatus();
    void refreshGraph();
  },

  loadMoreGraph: () => {
    const next = Math.min(get().graphLimit + GRAPH_STEP, GRAPH_MAX);
    if (next === get().graphLimit) return;
    set({ graphLimit: next });
    void refreshGraph();
  },

  runAction: async (kind, arg) => {
    const repo = get().repo;
    if (!repo || get().busy) return;
    set({ busy: true });
    const paths = Array.isArray(arg) ? arg : [];
    const message = typeof arg === 'string' ? arg : '';
    try {
      let result: ActionResult | null = null;
      switch (kind) {
        case 'stage':
          result = await gitBridge.stage(repo.repoId, paths);
          break;
        case 'stageAll':
          result = await gitBridge.stageAll(repo.repoId);
          break;
        case 'unstage':
          result = await gitBridge.unstage(repo.repoId, paths);
          break;
        case 'unstageAll':
          result = await gitBridge.unstageAll(repo.repoId);
          break;
        case 'discard':
          result = await gitBridge.discard(repo.repoId, paths);
          break;
        case 'commit':
          result = await gitBridge.commit(repo.repoId, message);
          break;
        case 'push':
          result = await pushFlow(repo.repoId);
          break;
        case 'pull':
          result = await gitBridge.pull(repo.repoId);
          break;
        case 'fetch':
          result = await gitBridge.fetch(repo.repoId);
          break;
        case 'sync': {
          // Not atomic: report each step's outcome separately.
          try {
            result = await gitBridge.pull(repo.repoId);
          } catch (err) {
            reportError('sync (pull)', errMsg(err));
            result = null;
          }
          if (result) {
            try {
              result = (await pushFlow(repo.repoId)) ?? result;
            } catch (err) {
              reportError('sync (push)', errMsg(err));
            }
          }
          break;
        }
      }
      if (result) {
        applyActionResult(repo.repoId, result);
        if (kind === 'commit' || kind === 'push' || kind === 'pull' || kind === 'fetch' || kind === 'sync') {
          void refreshGraph();
        }
      }
    } catch (err) {
      reportError(kind, errMsg(err));
    } finally {
      set({ busy: false });
    }
  },
}));
