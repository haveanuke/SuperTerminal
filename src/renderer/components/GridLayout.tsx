import { useTerminalStore } from '../stores/terminal-store';
import { TerminalView } from './TerminalView';
import { PaneToolbar } from './PaneToolbar';
import { useThemeStore } from '../stores/theme-store';

export function GridLayout() {
  const terminals = useTerminalStore((s) => s.terminals);
  const gridCols = useTerminalStore((s) => s.gridCols);
  const gridRows = useTerminalStore((s) => s.gridRows);
  const addTerminal = useTerminalStore((s) => s.addTerminal);
  const removeTerminal = useTerminalStore((s) => s.removeTerminal);
  const theme = useThemeStore((s) => s.theme);

  const terminalIds = Array.from(terminals.keys());
  const totalSlots = gridCols * gridRows;

  // Fill grid slots with existing terminals, add empty slots for remaining
  const slots: (string | null)[] = [];
  for (let i = 0; i < totalSlots; i++) {
    slots.push(terminalIds[i] || null);
  }

  return (
    <div
      style={{
        display: 'grid',
        gridTemplateColumns: `repeat(${gridCols}, 1fr)`,
        gridTemplateRows: `repeat(${gridRows}, 1fr)`,
        width: '100%',
        height: '100%',
        gap: 1,
        backgroundColor: theme.uiBorder,
      }}
    >
      {slots.map((termId, i) =>
        termId ? (
          <div key={termId} style={{ display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
            <PaneToolbar
              terminalId={termId}
              onSplitH={() => {}}
              onSplitV={() => {}}
              onClose={() => removeTerminal(termId)}
            />
            <div style={{ flex: 1, overflow: 'hidden' }}>
              <TerminalView terminalId={termId} />
            </div>
          </div>
        ) : (
          <div
            key={`empty-${i}`}
            style={{
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'center',
              backgroundColor: theme.uiBackground,
            }}
          >
            <button
              className="toolbar-btn"
              onClick={() => addTerminal()}
              style={{ fontSize: 24, padding: '12px 24px', color: theme.uiTextMuted }}
            >
              +
            </button>
          </div>
        )
      )}
    </div>
  );
}
