import { useTerminalStore } from '../stores/terminal-store';
import { useThemeStore } from '../stores/theme-store';
import { TabBar } from './TabBar';
import { SplitPane } from './SplitPane';
import { StatusBar } from './StatusBar';

export function App() {
  const tabs = useTerminalStore((s) => s.tabs);
  const activeTabId = useTerminalStore((s) => s.activeTabId);
  const theme = useThemeStore((s) => s.theme);

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
        {tabs.map((tab) => (
          <div
            key={tab.id}
            style={{
              position: 'absolute',
              inset: 0,
              visibility: tab.id === activeTabId ? 'visible' : 'hidden',
              pointerEvents: tab.id === activeTabId ? 'auto' : 'none',
            }}
          >
            <SplitPane pane={tab.pane} tabId={tab.id} />
          </div>
        ))}
      </div>
      <StatusBar />
    </div>
  );
}
