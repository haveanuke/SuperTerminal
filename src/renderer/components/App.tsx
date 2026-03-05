import { useTerminalStore } from '../stores/terminal-store';
import { useThemeStore } from '../stores/theme-store';
import { TabBar } from './TabBar';
import { SplitPane } from './SplitPane';
import { GridLayout } from './GridLayout';
import { StatusBar } from './StatusBar';
import { TerminalView } from './TerminalView';
import { PaneToolbar } from './PaneToolbar';

function collectTerminalIds(pane: import('../types').PaneNode): string[] {
  if (pane.type === 'terminal') return [pane.terminalId];
  return [...collectTerminalIds(pane.children[0]), ...collectTerminalIds(pane.children[1])];
}

export function App() {
  const tabs = useTerminalStore((s) => s.tabs);
  const activeTabId = useTerminalStore((s) => s.activeTabId);
  const layoutMode = useTerminalStore((s) => s.layoutMode);
  const theme = useThemeStore((s) => s.theme);
  const closePaneTerminal = useTerminalStore((s) => s.closePaneTerminal);

  const activeTab = tabs.find((t) => t.id === activeTabId) || tabs[0];

  return (
    <div
      className="app"
      style={{
        display: 'flex',
        flexDirection: 'column',
        height: '100vh',
        width: '100vw',
        backgroundColor: theme.uiBackground,
        color: theme.uiText,
        overflow: 'hidden',
      }}
    >
      <TabBar />
      <div style={{ flex: 1, overflow: 'hidden', position: 'relative' }}>
        {layoutMode === 'grid' ? (
          <GridLayout />
        ) : layoutMode === 'tabs' ? (
          // Tabs mode: show only the active tab's first terminal
          activeTab && (() => {
            const termIds = collectTerminalIds(activeTab.pane);
            const termId = termIds[0];
            return termId ? (
              <div style={{ display: 'flex', flexDirection: 'column', width: '100%', height: '100%' }}>
                <PaneToolbar
                  terminalId={termId}
                  onSplitH={() => {}}
                  onSplitV={() => {}}
                  onClose={() => closePaneTerminal(activeTab.id, termId)}
                />
                <div style={{ flex: 1, overflow: 'hidden' }}>
                  <TerminalView terminalId={termId} />
                </div>
              </div>
            ) : null;
          })()
        ) : (
          // Splits mode
          activeTab && <SplitPane pane={activeTab.pane} tabId={activeTab.id} />
        )}
      </div>
      <StatusBar />
    </div>
  );
}
