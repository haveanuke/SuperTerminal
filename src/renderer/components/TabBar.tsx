import { useTerminalStore } from '../stores/terminal-store';
import { useThemeStore } from '../stores/theme-store';
import type { LayoutMode } from '../types';

export function TabBar() {
  const tabs = useTerminalStore((s) => s.tabs);
  const activeTabId = useTerminalStore((s) => s.activeTabId);
  const setActiveTab = useTerminalStore((s) => s.setActiveTab);
  const addTab = useTerminalStore((s) => s.addTab);
  const removeTab = useTerminalStore((s) => s.removeTab);
  const layoutMode = useTerminalStore((s) => s.layoutMode);
  const setLayoutMode = useTerminalStore((s) => s.setLayoutMode);
  const broadcastMode = useTerminalStore((s) => s.broadcastMode);
  const toggleBroadcast = useTerminalStore((s) => s.toggleBroadcast);
  const setBroadcastAll = useTerminalStore((s) => s.setBroadcastAll);
  const searchOpen = useTerminalStore((s) => s.searchOpen);
  const setSearchOpen = useTerminalStore((s) => s.setSearchOpen);
  const searchQuery = useTerminalStore((s) => s.searchQuery);
  const setSearchQuery = useTerminalStore((s) => s.setSearchQuery);
  const theme = useThemeStore((s) => s.theme);

  const layoutModes: { mode: LayoutMode; label: string }[] = [
    { mode: 'tabs', label: 'Tabs' },
    { mode: 'splits', label: 'Splits' },
    { mode: 'grid', label: 'Grid' },
  ];

  return (
    <div
      className="tab-bar"
      style={{
        display: 'flex',
        alignItems: 'center',
        backgroundColor: theme.uiSurface,
        borderBottom: `1px solid ${theme.uiBorder}`,
        WebkitAppRegion: 'drag' as unknown as string,
      }}
    >
      {/* Drag region / window controls spacer */}
      <div style={{ width: 78, flexShrink: 0 }} />

      {/* Tabs */}
      <div
        style={{
          display: 'flex',
          flex: 1,
          overflow: 'auto',
          WebkitAppRegion: 'no-drag' as unknown as string,
        }}
      >
        {tabs.map((tab) => (
          <div
            key={tab.id}
            className={`tab ${tab.id === activeTabId ? 'tab-active' : ''}`}
            onClick={() => setActiveTab(tab.id)}
            style={{
              padding: '6px 12px',
              cursor: 'pointer',
              display: 'flex',
              alignItems: 'center',
              gap: 6,
              fontSize: 13,
              color: tab.id === activeTabId ? theme.uiText : theme.uiTextMuted,
              backgroundColor: tab.id === activeTabId ? theme.uiBackground : 'transparent',
              borderRight: `1px solid ${theme.uiBorder}`,
              whiteSpace: 'nowrap',
            }}
          >
            <span>{tab.label}</span>
            {tabs.length > 1 && (
              <span
                className="tab-close"
                onClick={(e) => {
                  e.stopPropagation();
                  removeTab(tab.id);
                }}
                style={{ opacity: 0.5, fontSize: 14 }}
              >
                &times;
              </span>
            )}
          </div>
        ))}
        <button
          className="toolbar-btn"
          onClick={addTab}
          title="New tab"
          style={{
            padding: '6px 10px',
            fontSize: 16,
            WebkitAppRegion: 'no-drag' as unknown as string,
          }}
        >
          +
        </button>
      </div>

      {/* Controls */}
      <div
        style={{
          display: 'flex',
          alignItems: 'center',
          gap: 4,
          padding: '0 8px',
          WebkitAppRegion: 'no-drag' as unknown as string,
        }}
      >
        {/* Search */}
        {searchOpen && (
          <input
            type="text"
            placeholder="Search..."
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            autoFocus
            style={{
              background: theme.uiBackground,
              border: `1px solid ${theme.uiBorder}`,
              color: theme.uiText,
              padding: '3px 8px',
              fontSize: 12,
              borderRadius: 4,
              outline: 'none',
              width: 160,
            }}
            onKeyDown={(e) => {
              if (e.key === 'Escape') {
                setSearchOpen(false);
                setSearchQuery('');
              }
            }}
          />
        )}
        <button
          className="toolbar-btn"
          onClick={() => setSearchOpen(!searchOpen)}
          title="Search"
          style={{ color: searchOpen ? theme.uiAccent : undefined }}
        >
          &#x1F50D;
        </button>

        {/* Broadcast */}
        <button
          className="toolbar-btn"
          onClick={() => {
            toggleBroadcast();
            if (!broadcastMode) setBroadcastAll();
          }}
          title={broadcastMode ? 'Disable broadcast' : 'Enable broadcast'}
          style={{
            color: broadcastMode ? theme.uiAccent : undefined,
            fontWeight: broadcastMode ? 'bold' : 'normal',
          }}
        >
          BC
        </button>

        {/* Layout mode */}
        {layoutModes.map(({ mode, label }) => (
          <button
            key={mode}
            className="toolbar-btn"
            onClick={() => setLayoutMode(mode)}
            style={{
              color: layoutMode === mode ? theme.uiAccent : theme.uiTextMuted,
              fontWeight: layoutMode === mode ? 'bold' : 'normal',
              fontSize: 11,
            }}
          >
            {label}
          </button>
        ))}
      </div>
    </div>
  );
}
