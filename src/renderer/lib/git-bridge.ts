import { invoke } from '@tauri-apps/api/core';

export interface RepoInfo {
  repoId: string;
  displayName: string;
  root: string;
}

export interface StatusEntry {
  kind: 'ordinary' | 'rename_copy' | 'unmerged' | 'untracked';
  indexStatus: string;
  worktreeStatus: string;
  path: string;
  origPath: string | null;
  submodule: boolean;
  actionable: boolean;
}

export interface StatusReport {
  branch: string | null;
  detached: string | null;
  upstream: string | null;
  ahead: number;
  behind: number;
  unborn: boolean;
  entries: StatusEntry[];
}

export interface ActionResult {
  report: StatusReport;
  skipped: number;
}

export interface GraphEdge {
  fromLane: number;
  toLane: number;
}

export interface GraphRow {
  hash: string;
  lane: number;
  edges: GraphEdge[];
  refsDisplay: string;
  author: string;
  time: number;
  subject: string;
}

export interface GraphData {
  rows: GraphRow[];
  laneCount: number;
}

export const gitBridge = {
  resolveRepo: (cwd: string) => invoke<RepoInfo | null>('git_resolve_repo', { cwd }),
  status: (repoId: string) => invoke<StatusReport>('git_status', { repoId }),
  graph: (repoId: string, limit: number) => invoke<GraphData>('git_graph', { repoId, limit }),
  stage: (repoId: string, paths: string[]) => invoke<ActionResult>('git_stage', { repoId, paths }),
  stageAll: (repoId: string) => invoke<ActionResult>('git_stage_all', { repoId }),
  unstage: (repoId: string, paths: string[]) => invoke<ActionResult>('git_unstage', { repoId, paths }),
  unstageAll: (repoId: string) => invoke<ActionResult>('git_unstage_all', { repoId }),
  discard: (repoId: string, paths: string[]) => invoke<ActionResult>('git_discard', { repoId, paths }),
  commit: (repoId: string, message: string) => invoke<ActionResult>('git_commit', { repoId, message }),
  push: (repoId: string, setUpstream: boolean) => invoke<ActionResult>('git_push', { repoId, setUpstream }),
  pull: (repoId: string) => invoke<ActionResult>('git_pull', { repoId }),
  fetch: (repoId: string) => invoke<ActionResult>('git_fetch', { repoId }),
};

export type GitBridge = typeof gitBridge;
