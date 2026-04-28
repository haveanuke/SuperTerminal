import { useState } from 'react';
import { useTerminalStore } from '../stores/terminal-store';
import { useThemeStore } from '../stores/theme-store';
import { useBuddyStore } from '../buddy/buddy-store';
import { SettingsPanel } from './SettingsPanel';
import { Settings } from './icons';

export function StatusBar() {
  const terminals = useTerminalStore((s) => s.terminals);
  const tabs = useTerminalStore((s) => s.tabs);
  const broadcastMode = useTerminalStore((s) => s.broadcastMode);
  const theme = useThemeStore((s) => s.theme);

  const buddyVisible = useBuddyStore((s) => s.visible);
  const setBuddyVisible = useBuddyStore((s) => s.setVisible);
  const buddyHatched = useBuddyStore((s) => s.hasHatched);
  const buddyCompanion = useBuddyStore((s) => s.companion);
  const setBuddyCardOpen = useBuddyStore((s) => s.setCardOpen);

  const [showSettings, setShowSettings] = useState(false);

  return (
    <>
      <div
        className="status-bar"
        style={{
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'space-between',
          padding: '2px 12px',
          backgroundColor: theme.uiSurface,
          borderTop: `1px solid ${theme.uiBorder}`,
          fontSize: 11,
          color: theme.uiTextMuted,
          userSelect: 'none',
        }}
      >
        <div style={{ display: 'flex', gap: 12 }}>
          <span>{tabs.length} tab{tabs.length !== 1 ? 's' : ''}</span>
          <span>{terminals.size} terminal{terminals.size !== 1 ? 's' : ''}</span>
          {broadcastMode && <span style={{ color: theme.uiAccent }}>Broadcast ON</span>}
        </div>
        <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
          {buddyHatched && buddyCompanion && (
            <button
              className="toolbar-btn"
              onClick={() => {
                if (buddyVisible) setBuddyCardOpen(true);
                else setBuddyVisible(true);
              }}
              title={buddyVisible ? `${buddyCompanion.name} — click for details` : `Show ${buddyCompanion.name}`}
              style={{ fontSize: 11, padding: '1px 6px', opacity: buddyVisible ? 1 : 0.55 }}
            >
              {buddyVisible ? '◉' : '○'} {buddyCompanion.name}
            </button>
          )}
          <button
            className="toolbar-btn"
            onClick={() => setShowSettings(!showSettings)}
            title="Settings"
          >
            <Settings size={13} />
          </button>
        </div>
      </div>

      {showSettings && <SettingsPanel />}
    </>
  );
}
