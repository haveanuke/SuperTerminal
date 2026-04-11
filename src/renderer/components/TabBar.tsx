import { useState } from 'react';
import { useTerminalStore } from '../stores/terminal-store';
import { useThemeStore } from '../stores/theme-store';
import { Plus, X, Search, Radio } from './icons';

export function TabBar() {
  const [editingTabId, setEditingTabId] = useState<string | null>(null);
  const tabs = useTerminalStore((s) => s.tabs);
  const activeTabId = useTerminalStore((s) => s.activeTabId);
  const setActiveTab = useTerminalStore((s) => s.setActiveTab);
  const addTab = useTerminalStore((s) => s.addTab);
  const removeTab = useTerminalStore((s) => s.removeTab);
  const broadcastMode = useTerminalStore((s) => s.broadcastMode);
  const toggleBroadcast = useTerminalStore((s) => s.toggleBroadcast);
  const setBroadcastAll = useTerminalStore((s) => s.setBroadcastAll);
  const searchOpen = useTerminalStore((s) => s.searchOpen);
  const setSearchOpen = useTerminalStore((s) => s.setSearchOpen);
  const searchQuery = useTerminalStore((s) => s.searchQuery);
  const setSearchQuery = useTerminalStore((s) => s.setSearchQuery);
  const setTabLabel = useTerminalStore((s) => s.setTabLabel);
  const theme = useThemeStore((s) => s.theme);

  return (
    <div
      className="tab-bar"
      style={{
        display: 'flex',
        alignItems: 'stretch',
        minHeight: 40,
        backgroundColor: theme.uiSurface,
        borderBottom: `1px solid ${theme.uiBorder}`,
        WebkitAppRegion: 'drag' as unknown as string,
      }}
    >
      {/* Drag region / window controls spacer */}
      <div style={{ width: 78, flexShrink: 0, display: 'flex', alignItems: 'center' }} />

      {/* Tabs */}
      <div
        style={{
          display: 'flex',
          overflow: 'auto',
          flexShrink: 1,
          WebkitAppRegion: 'no-drag' as unknown as string,
        }}
      >
        {tabs.map((tab) => {
          const isActive = tab.id === activeTabId;
          return (
            <div
              key={tab.id}
              className="tab"
              onClick={() => setActiveTab(tab.id)}
              style={{
                padding: '0 12px',
                cursor: 'pointer',
                display: 'flex',
                alignItems: 'center',
                alignSelf: 'stretch',
                gap: 6,
                fontSize: 13,
                color: isActive ? theme.uiText : theme.uiTextMuted,
                backgroundColor: isActive ? theme.uiBackground : 'transparent',
                borderRight: `1px solid ${theme.uiBorder}`,
                borderBottom: isActive ? `2px solid ${theme.uiAccent}` : '2px solid transparent',
                whiteSpace: 'nowrap',
                position: 'relative',
              }}
            >
              {editingTabId === tab.id ? (
                <input
                  type="text"
                  defaultValue={tab.label}
                  autoFocus
                  onBlur={(e) => {
                    const val = e.target.value.trim();
                    if (val) setTabLabel(tab.id, val);
                    setEditingTabId(null);
                  }}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') {
                      const val = (e.target as HTMLInputElement).value.trim();
                      if (val) setTabLabel(tab.id, val);
                      setEditingTabId(null);
                    }
                    if (e.key === 'Escape') setEditingTabId(null);
                  }}
                  onClick={(e) => e.stopPropagation()}
                  style={{
                    background: theme.uiBackground,
                    border: `1px solid ${theme.uiAccent}`,
                    color: theme.uiText,
                    fontSize: 13,
                    padding: '0 4px',
                    width: 80,
                    outline: 'none',
                    borderRadius: 3,
                  }}
                />
              ) : (
                <span onDoubleClick={(e) => { e.stopPropagation(); setEditingTabId(tab.id); }}>
                  {tab.label}
                </span>
              )}
              {tabs.length > 1 && (
                <span
                  className="tab-close"
                  onClick={(e) => {
                    e.stopPropagation();
                    removeTab(tab.id);
                  }}
                >
                  <X size={12} />
                </span>
              )}
            </div>
          );
        })}
        <button
          className="toolbar-btn"
          onClick={addTab}
          title="New tab"
          style={{
            padding: '6px 10px',
            WebkitAppRegion: 'no-drag' as unknown as string,
          }}
        >
          <Plus size={14} />
        </button>
      </div>

      {/* Draggable spacer */}
      <div style={{ flex: 1, minWidth: 40, display: 'flex', alignItems: 'center' }} />

      {/* Controls */}
      <div
        style={{
          display: 'flex',
          alignItems: 'center',
          gap: 2,
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
          <Search />
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
          }}
        >
          <Radio />
        </button>
      </div>
    </div>
  );
}
