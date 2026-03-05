import { useTerminalStore } from '../stores/terminal-store';
import { useThemeStore } from '../stores/theme-store';
import { themes } from '../stores/theme-store';
import { useState } from 'react';

export function StatusBar() {
  const terminals = useTerminalStore((s) => s.terminals);
  const layoutMode = useTerminalStore((s) => s.layoutMode);
  const broadcastMode = useTerminalStore((s) => s.broadcastMode);
  const gridCols = useTerminalStore((s) => s.gridCols);
  const gridRows = useTerminalStore((s) => s.gridRows);
  const setGridSize = useTerminalStore((s) => s.setGridSize);
  const theme = useThemeStore((s) => s.theme);
  const setTheme = useThemeStore((s) => s.setTheme);
  const fontSize = useThemeStore((s) => s.fontSize);
  const setFontSize = useThemeStore((s) => s.setFontSize);

  const [showSettings, setShowSettings] = useState(false);

  // Session management
  const getSerializableLayout = useTerminalStore((s) => s.getSerializableLayout);
  const [sessionName, setSessionName] = useState('');
  const [savedSessions, setSavedSessions] = useState<string[]>([]);

  const loadSessions = async () => {
    const list = await window.superTerminal.session.list();
    setSavedSessions(list);
  };

  const saveSession = async () => {
    if (!sessionName.trim()) return;
    const layout = getSerializableLayout();
    await window.superTerminal.session.save(sessionName, layout);
    setSessionName('');
    loadSessions();
  };

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
          <span>{terminals.size} terminal{terminals.size !== 1 ? 's' : ''}</span>
          <span>Layout: {layoutMode}</span>
          {broadcastMode && <span style={{ color: theme.uiAccent }}>Broadcast ON</span>}
          {layoutMode === 'grid' && (
            <span>
              Grid:
              <button className="toolbar-btn" onClick={() => setGridSize(Math.max(1, gridCols - 1), gridRows)} style={{ fontSize: 11 }}>-</button>
              {gridCols}x{gridRows}
              <button className="toolbar-btn" onClick={() => setGridSize(gridCols + 1, gridRows)} style={{ fontSize: 11 }}>+</button>
              <button className="toolbar-btn" onClick={() => setGridSize(gridCols, Math.max(1, gridRows - 1))} style={{ fontSize: 11 }}>-R</button>
              <button className="toolbar-btn" onClick={() => setGridSize(gridCols, gridRows + 1)} style={{ fontSize: 11 }}>+R</button>
            </span>
          )}
        </div>
        <div style={{ display: 'flex', gap: 8 }}>
          <button
            className="toolbar-btn"
            onClick={() => {
              setShowSettings(!showSettings);
              if (!showSettings) loadSessions();
            }}
            style={{ fontSize: 11 }}
          >
            Settings
          </button>
        </div>
      </div>

      {showSettings && (
        <div
          className="settings-panel"
          style={{
            position: 'absolute',
            bottom: 24,
            right: 8,
            width: 300,
            backgroundColor: theme.uiSurface,
            border: `1px solid ${theme.uiBorder}`,
            borderRadius: 8,
            padding: 16,
            zIndex: 100,
            color: theme.uiText,
            fontSize: 13,
            display: 'flex',
            flexDirection: 'column',
            gap: 12,
          }}
        >
          <div>
            <label style={{ display: 'block', marginBottom: 4, color: theme.uiTextMuted }}>Theme</label>
            <div style={{ display: 'flex', gap: 4 }}>
              {themes.map((t) => (
                <button
                  key={t.name}
                  className="toolbar-btn"
                  onClick={() => setTheme(t)}
                  style={{
                    padding: '4px 8px',
                    fontSize: 11,
                    backgroundColor: t.background,
                    color: t.foreground,
                    border: theme.name === t.name ? `2px solid ${theme.uiAccent}` : `1px solid ${theme.uiBorder}`,
                    borderRadius: 4,
                  }}
                >
                  {t.name}
                </button>
              ))}
            </div>
          </div>

          <div>
            <label style={{ display: 'block', marginBottom: 4, color: theme.uiTextMuted }}>
              Font Size: {fontSize}px
            </label>
            <input
              type="range"
              min="10"
              max="24"
              value={fontSize}
              onChange={(e) => setFontSize(Number(e.target.value))}
              style={{ width: '100%' }}
            />
          </div>

          <div>
            <label style={{ display: 'block', marginBottom: 4, color: theme.uiTextMuted }}>Sessions</label>
            <div style={{ display: 'flex', gap: 4, marginBottom: 8 }}>
              <input
                type="text"
                placeholder="Session name"
                value={sessionName}
                onChange={(e) => setSessionName(e.target.value)}
                style={{
                  flex: 1,
                  background: theme.uiBackground,
                  border: `1px solid ${theme.uiBorder}`,
                  color: theme.uiText,
                  padding: '3px 8px',
                  fontSize: 12,
                  borderRadius: 4,
                  outline: 'none',
                }}
              />
              <button className="toolbar-btn" onClick={saveSession} style={{ fontSize: 11, padding: '3px 8px' }}>
                Save
              </button>
            </div>
            {savedSessions.length > 0 && (
              <div style={{ display: 'flex', flexDirection: 'column', gap: 2 }}>
                {savedSessions.map((name) => (
                  <div key={name} style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
                    <span style={{ fontSize: 12 }}>{name}</span>
                    <button
                      className="toolbar-btn"
                      onClick={() => window.superTerminal.session.delete(name).then(loadSessions)}
                      style={{ fontSize: 10, color: theme.red }}
                    >
                      Delete
                    </button>
                  </div>
                ))}
              </div>
            )}
          </div>
        </div>
      )}
    </>
  );
}
