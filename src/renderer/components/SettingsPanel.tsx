import { useEffect, useRef, useState } from 'react';
import { useTerminalStore } from '../stores/terminal-store';
import { useThemeStore, validateTheme, exportThemeJSON } from '../stores/theme-store';
import { useUIStore } from '../stores/ui-store';
import { useBuddyStore, AGENT_PRESETS } from '../buddy/buddy-store';
import type { AgentConfig } from '../buddy/buddy-store';
import { toastError } from '../stores/toast-store';
import { Import, Export, Trash, ImageIcon, Save, X } from './icons';

export function SettingsPanel() {
  const theme = useThemeStore((s) => s.theme);
  const setTheme = useThemeStore((s) => s.setTheme);
  const customThemes = useThemeStore((s) => s.customThemes);
  const addCustomTheme = useThemeStore((s) => s.addCustomTheme);
  const removeCustomTheme = useThemeStore((s) => s.removeCustomTheme);
  const getAllThemes = useThemeStore((s) => s.getAllThemes);

  const fontSize = useUIStore((s) => s.fontSize);
  const setFontSize = useUIStore((s) => s.setFontSize);
  const backgroundImage = useUIStore((s) => s.backgroundImage);
  const backgroundOpacity = useUIStore((s) => s.backgroundOpacity);
  const setBackgroundImage = useUIStore((s) => s.setBackgroundImage);
  const setBackgroundOpacity = useUIStore((s) => s.setBackgroundOpacity);

  const buddyAgent = useBuddyStore((s) => s.agent);
  const setBuddyAgent = useBuddyStore((s) => s.setAgent);

  const getSerializableLayout = useTerminalStore((s) => s.getSerializableLayout);

  const [importError, setImportError] = useState('');
  const fileInputRef = useRef<HTMLInputElement>(null);

  const [sessionName, setSessionName] = useState('');
  const [savedSessions, setSavedSessions] = useState<string[]>([]);

  const loadSessions = async () => {
    const list = await window.superTerminal.session.list();
    setSavedSessions(list);
  };

  useEffect(() => {
    loadSessions();
  }, []);

  const saveSession = async () => {
    if (!sessionName.trim()) return;
    const layout = getSerializableLayout();
    try {
      await window.superTerminal.session.save(sessionName, layout);
      setSessionName('');
      loadSessions();
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      toastError(`Failed to save session: ${msg}`);
    }
  };

  const handleImportTheme = (e: React.ChangeEvent<HTMLInputElement>) => {
    setImportError('');
    const file = e.target.files?.[0];
    if (!file) return;
    const reader = new FileReader();
    reader.onload = () => {
      try {
        const parsed = JSON.parse(reader.result as string);
        if (!validateTheme(parsed)) {
          setImportError('Invalid theme: missing required fields');
          return;
        }
        addCustomTheme(parsed);
        setTheme(parsed);
        setImportError('');
      } catch {
        setImportError('Invalid JSON file');
      }
    };
    reader.readAsText(file);
    e.target.value = '';
  };

  const handleExportTheme = () => {
    const json = exportThemeJSON(theme);
    const blob = new Blob([json], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `${theme.name.toLowerCase().replace(/\s+/g, '-')}.json`;
    a.click();
    URL.revokeObjectURL(url);
  };

  const handlePickImage = async () => {
    try {
      const path = await window.superTerminal.dialog.openImage();
      if (path) setBackgroundImage(path);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      toastError(`Failed to open image: ${msg}`);
    }
  };

  const allThemes = getAllThemes();
  const isCustom = (name: string) => customThemes.some((t) => t.name === name);

  return (
    <div
      className="settings-panel"
      style={{
        position: 'absolute',
        bottom: 24,
        right: 8,
        width: 340,
        maxHeight: 'calc(100vh - 80px)',
        overflowY: 'auto',
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
      {/* Theme Selection */}
      <div>
        <label style={{ display: 'block', marginBottom: 6, color: theme.uiTextMuted, fontSize: 11, textTransform: 'uppercase', letterSpacing: '0.5px' }}>Theme</label>
        <div
          style={{
            display: 'grid',
            gridTemplateColumns: 'repeat(3, 1fr)',
            gap: 4,
            maxHeight: 180,
            overflowY: 'auto',
            paddingRight: 4,
          }}
        >
          {allThemes.map((t) => (
            <button
              key={t.name}
              className="toolbar-btn"
              onClick={() => setTheme(t)}
              title={t.name}
              style={{
                padding: '5px 6px',
                fontSize: 10,
                backgroundColor: t.background,
                color: t.foreground,
                border: theme.name === t.name ? `2px solid ${theme.uiAccent}` : `1px solid ${theme.uiBorder}`,
                borderRadius: 4,
                textOverflow: 'ellipsis',
                overflow: 'hidden',
                whiteSpace: 'nowrap',
              }}
            >
              {t.name}
            </button>
          ))}
        </div>
      </div>

      {/* Custom Theme Actions */}
      <div style={{ display: 'flex', gap: 4, flexWrap: 'wrap' }}>
        <button
          className="toolbar-btn"
          onClick={() => fileInputRef.current?.click()}
          style={{ fontSize: 11, padding: '3px 8px' }}
        >
          <Import size={12} /> Import
        </button>
        <button
          className="toolbar-btn"
          onClick={handleExportTheme}
          style={{ fontSize: 11, padding: '3px 8px' }}
        >
          <Export size={12} /> Export
        </button>
        {isCustom(theme.name) && (
          <button
            className="toolbar-btn toolbar-btn-danger"
            onClick={() => removeCustomTheme(theme.name)}
            style={{ fontSize: 11, padding: '3px 8px', color: theme.red }}
          >
            <Trash size={12} /> Delete
          </button>
        )}
        <input
          ref={fileInputRef}
          type="file"
          accept=".json"
          onChange={handleImportTheme}
          style={{ display: 'none' }}
        />
      </div>
      {importError && (
        <span style={{ color: theme.red, fontSize: 11 }}>{importError}</span>
      )}

      {/* Font Size */}
      <div>
        <label style={{ display: 'block', marginBottom: 4, color: theme.uiTextMuted, fontSize: 11, textTransform: 'uppercase', letterSpacing: '0.5px' }}>
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

      {/* Background Image */}
      <div>
        <label style={{ display: 'block', marginBottom: 4, color: theme.uiTextMuted, fontSize: 11, textTransform: 'uppercase', letterSpacing: '0.5px' }}>
          Background
        </label>
        <div style={{ display: 'flex', gap: 4, alignItems: 'center' }}>
          <button
            className="toolbar-btn"
            onClick={handlePickImage}
            style={{ fontSize: 11, padding: '3px 8px' }}
          >
            <ImageIcon size={12} /> {backgroundImage ? 'Change' : 'Set Image'}
          </button>
          {backgroundImage && (
            <button
              className="toolbar-btn toolbar-btn-danger"
              onClick={() => setBackgroundImage(null)}
              style={{ fontSize: 11, padding: '3px 8px', color: theme.red }}
            >
              <X size={12} /> Clear
            </button>
          )}
        </div>
        {backgroundImage && (
          <div style={{ marginTop: 6 }}>
            <label style={{ display: 'block', marginBottom: 4, color: theme.uiTextMuted, fontSize: 11 }}>
              Opacity: {Math.round(backgroundOpacity * 100)}%
            </label>
            <input
              type="range"
              min="5"
              max="100"
              value={Math.round(backgroundOpacity * 100)}
              onChange={(e) => setBackgroundOpacity(Number(e.target.value) / 100)}
              style={{ width: '100%' }}
            />
          </div>
        )}
      </div>

      {/* Claude Buddy */}
      <div>
        <label style={{ display: 'block', marginBottom: 6, color: theme.uiTextMuted, fontSize: 11, textTransform: 'uppercase', letterSpacing: '0.5px' }}>
          Buddy
        </label>
        <label style={{ display: 'flex', alignItems: 'center', gap: 6, fontSize: 12, marginBottom: 6 }}>
          <input
            type="checkbox"
            checked={buddyAgent.enabled}
            onChange={(e) => setBuddyAgent({ enabled: e.target.checked })}
          />
          Smart reactions (run a CLI agent)
        </label>
        {buddyAgent.enabled && (
          <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
            <select
              value={buddyAgent.preset}
              onChange={(e) => {
                const preset = e.target.value as AgentConfig['preset'];
                if (preset === 'custom') {
                  setBuddyAgent({ preset });
                } else {
                  setBuddyAgent({ preset, ...AGENT_PRESETS[preset] });
                }
              }}
              style={{
                background: theme.uiBackground,
                border: `1px solid ${theme.uiBorder}`,
                color: theme.uiText,
                padding: '3px 6px',
                fontSize: 12,
                borderRadius: 4,
              }}
            >
              <option value="claude">Claude Code (claude -p)</option>
              <option value="codex">Codex (codex exec)</option>
              <option value="ollama">Ollama (llama3)</option>
              <option value="custom">Custom</option>
            </select>
            <input
              type="text"
              value={buddyAgent.command}
              onChange={(e) => setBuddyAgent({ command: e.target.value, preset: 'custom' })}
              placeholder="command (e.g. claude)"
              style={{
                background: theme.uiBackground,
                border: `1px solid ${theme.uiBorder}`,
                color: theme.uiText,
                padding: '3px 8px',
                fontSize: 12,
                borderRadius: 4,
                outline: 'none',
                fontFamily: 'monospace',
              }}
            />
            <input
              type="text"
              value={buddyAgent.args.join(' ')}
              onChange={(e) => {
                const args = e.target.value.match(/(?:"[^"]*"|\S)+/g)?.map((a) => a.replace(/^"|"$/g, '')) ?? [];
                setBuddyAgent({ args, preset: 'custom' });
              }}
              placeholder={'args (use {prompt} placeholder)'}
              style={{
                background: theme.uiBackground,
                border: `1px solid ${theme.uiBorder}`,
                color: theme.uiText,
                padding: '3px 8px',
                fontSize: 12,
                borderRadius: 4,
                outline: 'none',
                fontFamily: 'monospace',
              }}
            />
            <div style={{ fontSize: 10, color: theme.uiTextMuted }}>
              Runs on error / test-fail / large output, max once per 30s. Falls back to canned reactions.
            </div>
          </div>
        )}
      </div>

      {/* Sessions */}
      <div>
        <label style={{ display: 'block', marginBottom: 4, color: theme.uiTextMuted, fontSize: 11, textTransform: 'uppercase', letterSpacing: '0.5px' }}>Sessions</label>
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
            <Save size={12} /> Save
          </button>
        </div>
        {savedSessions.length > 0 && (
          <div style={{ display: 'flex', flexDirection: 'column', gap: 2 }}>
            {savedSessions.map((name) => (
              <div key={name} style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
                <span style={{ fontSize: 12 }}>{name}</span>
                <button
                  className="toolbar-btn toolbar-btn-danger"
                  onClick={() => window.superTerminal.session.delete(name).then(loadSessions)}
                  style={{ fontSize: 10, color: theme.red }}
                >
                  <Trash size={11} />
                </button>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
