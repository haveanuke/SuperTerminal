import { useEffect } from 'react';
import { useTerminalStore } from '../stores/terminal-store';
import { useThemeStore } from '../stores/theme-store';
import { useUIStore } from '../stores/ui-store';
import { useGitStore } from '../stores/git-store';
import { TabBar } from './TabBar';
import { SplitPane } from './SplitPane';
import { StatusBar } from './StatusBar';
import { ClaudeBuddy } from './ClaudeBuddy';
import { Toasts } from './Toasts';
import { GitSidebar } from './git/GitSidebar';
import { useBuddyEventWatcher } from '../buddy/use-event-watcher';

export function App() {
  useBuddyEventWatcher();

  const tabs = useTerminalStore((s) => s.tabs);
  const activeTabId = useTerminalStore((s) => s.activeTabId);
  const theme = useThemeStore((s) => s.theme);
  const backgroundImage = useUIStore((s) => s.backgroundImage);
  const backgroundOpacity = useUIStore((s) => s.backgroundOpacity);

  // Cmd+Shift+G toggles the source-control sidebar
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.metaKey && e.shiftKey && e.key.toLowerCase() === 'g') {
        e.preventDefault();
        useGitStore.getState().toggle();
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, []);

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
        position: 'relative',
      }}
    >
      {/* Background image layer */}
      {backgroundImage && (
        <div
          style={{
            position: 'absolute',
            inset: 0,
            zIndex: 0,
            pointerEvents: 'none',
          }}
        >
          <img
            src={backgroundImage}
            alt=""
            style={{
              width: '100%',
              height: '100%',
              objectFit: 'cover',
              opacity: backgroundOpacity,
            }}
          />
        </div>
      )}

      <div style={{ position: 'relative', zIndex: 1 }}>
        <TabBar />
      </div>
      <div style={{ flex: 1, overflow: 'hidden', position: 'relative', zIndex: 1, display: 'flex' }}>
        <GitSidebar />
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
      </div>
      <div style={{ position: 'relative', zIndex: 1 }}>
        <StatusBar />
      </div>
      <ClaudeBuddy />
      <Toasts />
    </div>
  );
}
