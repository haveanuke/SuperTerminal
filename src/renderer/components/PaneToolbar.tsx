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

  const isBroadcastTarget = broadcastTargets.has(terminalId);

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
        WebkitAppRegion: 'no-drag' as unknown as string,
      }}
    >
      <span style={{ flex: 1, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
        {terminal?.title || 'Terminal'}
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
      <button className="toolbar-btn" onClick={onSplitH} title="Split horizontal">&#x2502;</button>
      <button className="toolbar-btn" onClick={onSplitV} title="Split vertical">&#x2500;</button>
      <button className="toolbar-btn" onClick={onClose} title="Close">&times;</button>
    </div>
  );
}
