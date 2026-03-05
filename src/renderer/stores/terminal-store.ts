import { create } from 'zustand';
import { nanoid } from 'nanoid';
import type { Tab, PaneNode, LayoutMode, TerminalInstance } from '../types';

function createTerminal(cwd?: string): TerminalInstance {
  return { id: nanoid(), title: 'Terminal', cwd };
}

function createTab(terminal?: TerminalInstance): Tab {
  const term = terminal || createTerminal();
  return {
    id: nanoid(),
    label: 'Terminal',
    pane: { type: 'terminal', terminalId: term.id },
  };
}

interface TerminalStore {
  terminals: Map<string, TerminalInstance>;
  tabs: Tab[];
  activeTabId: string;
  layoutMode: LayoutMode;
  gridCols: number;
  gridRows: number;
  broadcastMode: boolean;
  broadcastTargets: Set<string>;
  searchOpen: boolean;
  searchQuery: string;
  activeTerminalId: string | null;

  // Actions
  addTerminal: (cwd?: string) => TerminalInstance;
  removeTerminal: (id: string) => void;
  setTerminalTitle: (id: string, title: string) => void;

  addTab: () => void;
  removeTab: (tabId: string) => void;
  setActiveTab: (tabId: string) => void;
  setTabLabel: (tabId: string, label: string) => void;

  splitPane: (tabId: string, terminalId: string, direction: 'horizontal' | 'vertical') => void;
  closePaneTerminal: (tabId: string, terminalId: string) => void;

  setLayoutMode: (mode: LayoutMode) => void;
  setGridSize: (cols: number, rows: number) => void;

  toggleBroadcast: () => void;
  toggleBroadcastTarget: (id: string) => void;
  setBroadcastAll: () => void;

  setSearchOpen: (open: boolean) => void;
  setSearchQuery: (query: string) => void;

  setActiveTerminalId: (id: string | null) => void;

  getSerializableLayout: () => unknown;
}

function insertSplit(
  pane: PaneNode,
  targetTerminalId: string,
  direction: 'horizontal' | 'vertical',
  newTerminalId: string
): PaneNode {
  if (pane.type === 'terminal' && pane.terminalId === targetTerminalId) {
    return {
      type: 'split',
      direction,
      children: [pane, { type: 'terminal', terminalId: newTerminalId }],
    };
  }
  if (pane.type === 'split') {
    return {
      ...pane,
      children: [
        insertSplit(pane.children[0], targetTerminalId, direction, newTerminalId),
        insertSplit(pane.children[1], targetTerminalId, direction, newTerminalId),
      ],
    };
  }
  return pane;
}

function removeFromPane(pane: PaneNode, terminalId: string): PaneNode | null {
  if (pane.type === 'terminal') {
    return pane.terminalId === terminalId ? null : pane;
  }
  const left = removeFromPane(pane.children[0], terminalId);
  const right = removeFromPane(pane.children[1], terminalId);
  if (!left && !right) return null;
  if (!left) return right;
  if (!right) return left;
  return { ...pane, children: [left, right] };
}

function collectTerminalIds(pane: PaneNode): string[] {
  if (pane.type === 'terminal') return [pane.terminalId];
  return [...collectTerminalIds(pane.children[0]), ...collectTerminalIds(pane.children[1])];
}

export const useTerminalStore = create<TerminalStore>((set, get) => {
  const firstTerminal = createTerminal();
  const firstTab = createTab(firstTerminal);
  const initialTerminals = new Map<string, TerminalInstance>();
  initialTerminals.set(firstTerminal.id, firstTerminal);

  return {
    terminals: initialTerminals,
    tabs: [firstTab],
    activeTabId: firstTab.id,
    layoutMode: 'splits',
    gridCols: 2,
    gridRows: 2,
    broadcastMode: false,
    broadcastTargets: new Set<string>(),
    searchOpen: false,
    searchQuery: '',
    activeTerminalId: firstTerminal.id,

    addTerminal: (cwd?: string) => {
      const term = createTerminal(cwd);
      set((s) => {
        const terminals = new Map(s.terminals);
        terminals.set(term.id, term);
        return { terminals };
      });
      return term;
    },

    removeTerminal: (id: string) => {
      set((s) => {
        const terminals = new Map(s.terminals);
        terminals.delete(id);
        return { terminals };
      });
      window.superTerminal.pty.dispose(id);
    },

    setTerminalTitle: (id: string, title: string) => {
      set((s) => {
        const terminals = new Map(s.terminals);
        const existing = terminals.get(id);
        if (existing) terminals.set(id, { ...existing, title });
        return { terminals };
      });
    },

    addTab: () => {
      const term = get().addTerminal();
      const tab = createTab(term);
      set((s) => ({ tabs: [...s.tabs, tab], activeTabId: tab.id }));
    },

    removeTab: (tabId: string) => {
      const state = get();
      const tab = state.tabs.find((t) => t.id === tabId);
      if (!tab || state.tabs.length <= 1) return;
      const termIds = collectTerminalIds(tab.pane);
      termIds.forEach((id) => state.removeTerminal(id));
      set((s) => {
        const tabs = s.tabs.filter((t) => t.id !== tabId);
        const activeTabId = s.activeTabId === tabId ? tabs[0].id : s.activeTabId;
        return { tabs, activeTabId };
      });
    },

    setActiveTab: (tabId: string) => set({ activeTabId: tabId }),

    setTabLabel: (tabId: string, label: string) => {
      set((s) => ({
        tabs: s.tabs.map((t) => (t.id === tabId ? { ...t, label } : t)),
      }));
    },

    splitPane: (tabId: string, terminalId: string, direction) => {
      const newTerm = get().addTerminal();
      set((s) => ({
        tabs: s.tabs.map((t) =>
          t.id === tabId
            ? { ...t, pane: insertSplit(t.pane, terminalId, direction, newTerm.id) }
            : t
        ),
      }));
    },

    closePaneTerminal: (tabId: string, terminalId: string) => {
      const state = get();
      const tab = state.tabs.find((t) => t.id === tabId);
      if (!tab) return;
      const remaining = collectTerminalIds(tab.pane).filter((id) => id !== terminalId);
      if (remaining.length === 0) {
        state.removeTab(tabId);
        return;
      }
      state.removeTerminal(terminalId);
      set((s) => ({
        tabs: s.tabs.map((t) =>
          t.id === tabId ? { ...t, pane: removeFromPane(t.pane, terminalId)! } : t
        ),
      }));
    },

    setLayoutMode: (mode) => set({ layoutMode: mode }),
    setGridSize: (gridCols, gridRows) => set({ gridCols, gridRows }),

    toggleBroadcast: () => set((s) => ({ broadcastMode: !s.broadcastMode })),
    toggleBroadcastTarget: (id) =>
      set((s) => {
        const targets = new Set(s.broadcastTargets);
        if (targets.has(id)) targets.delete(id);
        else targets.add(id);
        return { broadcastTargets: targets };
      }),
    setBroadcastAll: () =>
      set((s) => ({
        broadcastTargets: new Set(s.terminals.keys()),
      })),

    setSearchOpen: (open) => set({ searchOpen: open }),
    setSearchQuery: (query) => set({ searchQuery: query }),

    setActiveTerminalId: (id) => set({ activeTerminalId: id }),

    getSerializableLayout: () => {
      const s = get();
      return {
        tabs: s.tabs,
        activeTabId: s.activeTabId,
        layoutMode: s.layoutMode,
        gridCols: s.gridCols,
        gridRows: s.gridRows,
      };
    },
  };
});
