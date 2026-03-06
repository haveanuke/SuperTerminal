import { useState } from 'react';
import { useTerminalStore } from '../stores/terminal-store';
import { useThemeStore } from '../stores/theme-store';

interface PaneToolbarProps {
  terminalId: string;
  onSplitH: () => void;
  onSplitV: () => void;
  onClose: () => void;
}

export function PaneToolbar({ terminalId, onSplitH, onSplitV, onClose }: PaneToolbarProps) {
  const terminal = useTerminalStore((s) => s.terminals.get(terminalId));
  const theme = useThemeStore((s) => s.theme);
  const broadcastMode = useTerminalStore((s) => s.broadcastMode);
  const broadcastTargets = useTerminalStore((s) => s.broadcastTargets);
  const toggleBroadcastTarget = useTerminalStore((s) => s.toggleBroadcastTarget);
  const setAutoRun = useTerminalStore((s) => s.setAutoRun);
  const clearAutoRun = useTerminalStore((s) => s.clearAutoRun);
  const swapSource = useTerminalStore((s) => s.swapSource);
  const startSwap = useTerminalStore((s) => s.startSwap);
  const completeSwap = useTerminalStore((s) => s.completeSwap);
  const cancelSwap = useTerminalStore((s) => s.cancelSwap);

  const [showAutoRun, setShowAutoRun] = useState(false);
  const [command, setCommand] = useState(terminal?.autoRun?.command || '');
  const [interval, setInterval] = useState(terminal?.autoRun?.intervalSeconds?.toString() || '5');
  const [sendEscape, setSendEscape] = useState(terminal?.autoRun?.sendEscape || false);
  const [escapeDelay, setEscapeDelay] = useState(terminal?.autoRun?.escapeDelaySecs?.toString() || '2');

  const isBroadcastTarget = broadcastTargets.has(terminalId);
  const autoRunActive = terminal?.autoRun?.enabled;
  const isSwapSource = swapSource === terminalId;
  const isSwapTarget = swapSource !== null && swapSource !== terminalId;

  const handleStartAutoRun = () => {
    const secs = Math.max(1, Number(interval) || 5);
    const escSecs = Math.max(1, Number(escapeDelay) || 2);
    setAutoRun(terminalId, { command, intervalSeconds: secs, enabled: true, sendEscape, escapeDelaySecs: escSecs });
  };

  const handleStopAutoRun = () => {
    clearAutoRun(terminalId);
  };

  return (
    <div
      className="pane-toolbar"
      style={{
        display: 'flex',
        alignItems: 'center',
        gap: 4,
        padding: '2px 8px',
        backgroundColor: theme.uiSurface,
        borderBottom: `1px solid ${theme.uiBorder}`,
        fontSize: 12,
        color: theme.uiTextMuted,
        userSelect: 'none',
        position: 'relative',
        WebkitAppRegion: 'no-drag' as unknown as string,
      }}
    >
      <span style={{ flex: 1, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
        {terminal?.title || 'Terminal'}
        {autoRunActive && (
          <span style={{ color: theme.green, marginLeft: 6, fontSize: 11 }}>
            [{terminal?.autoRun?.intervalSeconds}s]
          </span>
        )}
      </span>
      {broadcastMode && (
        <button
          className="toolbar-btn"
          onClick={() => toggleBroadcastTarget(terminalId)}
          title={isBroadcastTarget ? 'Remove from broadcast' : 'Add to broadcast'}
          style={{
            background: isBroadcastTarget ? theme.uiAccent : 'transparent',
            color: isBroadcastTarget ? theme.uiBackground : theme.uiTextMuted,
          }}
        >
          BC
        </button>
      )}
      {isSwapTarget ? (
        <button
          className="toolbar-btn"
          onClick={() => completeSwap(terminalId)}
          title="Swap with selected pane"
          style={{ color: theme.yellow, fontWeight: 'bold', fontSize: 11 }}
        >
          Swap here
        </button>
      ) : (
        <button
          className="toolbar-btn"
          onClick={() => isSwapSource ? cancelSwap() : startSwap(terminalId)}
          title={isSwapSource ? 'Cancel swap' : 'Swap this pane'}
          style={{ color: isSwapSource ? theme.yellow : theme.uiTextMuted }}
        >
          &#x21C4;
        </button>
      )}
      <button
        className="toolbar-btn"
        onClick={() => setShowAutoRun(!showAutoRun)}
        title="Auto-run command on timer"
        style={{ color: autoRunActive ? theme.green : theme.uiTextMuted }}
      >
        &#x23F1;
      </button>
      <button className="toolbar-btn" onClick={onSplitH} title="Split horizontal">&#x2502;</button>
      <button className="toolbar-btn" onClick={onSplitV} title="Split vertical">&#x2500;</button>
      <button className="toolbar-btn" onClick={onClose} title="Close">&times;</button>

      {showAutoRun && (
        <div
          style={{
            position: 'absolute',
            top: '100%',
            right: 0,
            zIndex: 200,
            backgroundColor: theme.uiSurface,
            border: `1px solid ${theme.uiBorder}`,
            borderRadius: 6,
            padding: 12,
            width: 260,
            display: 'flex',
            flexDirection: 'column',
            gap: 8,
            boxShadow: '0 4px 12px rgba(0,0,0,0.3)',
          }}
        >
          <div style={{ fontSize: 12, color: theme.uiText, fontWeight: 'bold' }}>Auto-Run</div>
          <input
            type="text"
            placeholder="Command (e.g. kubectl get pods)"
            value={command}
            onChange={(e) => setCommand(e.target.value)}
            style={{
              background: theme.uiBackground,
              border: `1px solid ${theme.uiBorder}`,
              color: theme.uiText,
              padding: '4px 8px',
              fontSize: 12,
              borderRadius: 4,
              outline: 'none',
              fontFamily: 'inherit',
            }}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && command.trim()) handleStartAutoRun();
            }}
          />
          <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
            <label style={{ fontSize: 11, color: theme.uiTextMuted }}>Every</label>
            <input
              type="number"
              min="1"
              max="3600"
              value={interval}
              onChange={(e) => setInterval(e.target.value)}
              style={{
                background: theme.uiBackground,
                border: `1px solid ${theme.uiBorder}`,
                color: theme.uiText,
                padding: '3px 6px',
                fontSize: 12,
                borderRadius: 4,
                outline: 'none',
                width: 50,
              }}
            />
            <label style={{ fontSize: 11, color: theme.uiTextMuted }}>seconds</label>
          </div>
          <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
            <label style={{ fontSize: 11, color: theme.uiTextMuted, display: 'flex', alignItems: 'center', gap: 4, cursor: 'pointer' }}>
              <input
                type="checkbox"
                checked={sendEscape}
                onChange={(e) => setSendEscape(e.target.checked)}
                style={{ margin: 0 }}
              />
              Send Esc after
            </label>
            {sendEscape && (
              <>
                <input
                  type="number"
                  min="1"
                  step="1"
                  value={escapeDelay}
                  onChange={(e) => setEscapeDelay(e.target.value)}
                  style={{
                    background: theme.uiBackground,
                    border: `1px solid ${theme.uiBorder}`,
                    color: theme.uiText,
                    padding: '3px 6px',
                    fontSize: 12,
                    borderRadius: 4,
                    outline: 'none',
                    width: 45,
                  }}
                />
                <label style={{ fontSize: 11, color: theme.uiTextMuted }}>seconds</label>
              </>
            )}
          </div>
          <div style={{ display: 'flex', gap: 6 }}>
            {autoRunActive ? (
              <button
                className="toolbar-btn"
                onClick={handleStopAutoRun}
                style={{
                  flex: 1,
                  padding: '4px 8px',
                  fontSize: 12,
                  backgroundColor: theme.red,
                  color: theme.uiBackground,
                  borderRadius: 4,
                }}
              >
                Stop
              </button>
            ) : (
              <button
                className="toolbar-btn"
                onClick={handleStartAutoRun}
                disabled={!command.trim()}
                style={{
                  flex: 1,
                  padding: '4px 8px',
                  fontSize: 12,
                  backgroundColor: command.trim() ? theme.green : theme.uiBorder,
                  color: theme.uiBackground,
                  borderRadius: 4,
                }}
              >
                Start
              </button>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
