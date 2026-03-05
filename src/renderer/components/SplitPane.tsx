import { Allotment } from 'allotment';
import 'allotment/dist/style.css';
import type { PaneNode } from '../types';
import { TerminalView } from './TerminalView';
import { PaneToolbar } from './PaneToolbar';
import { useTerminalStore } from '../stores/terminal-store';

interface SplitPaneProps {
  pane: PaneNode;
  tabId: string;
}

export function SplitPane({ pane, tabId }: SplitPaneProps) {
  const closePaneTerminal = useTerminalStore((s) => s.closePaneTerminal);
  const splitPane = useTerminalStore((s) => s.splitPane);

  if (pane.type === 'terminal') {
    return (
      <div style={{ display: 'flex', flexDirection: 'column', width: '100%', height: '100%' }}>
        <PaneToolbar
          terminalId={pane.terminalId}
          onSplitH={() => splitPane(tabId, pane.terminalId, 'horizontal')}
          onSplitV={() => splitPane(tabId, pane.terminalId, 'vertical')}
          onClose={() => closePaneTerminal(tabId, pane.terminalId)}
        />
        <div style={{ flex: 1, overflow: 'hidden' }}>
          <TerminalView terminalId={pane.terminalId} />
        </div>
      </div>
    );
  }

  return (
    <Allotment vertical={pane.direction === 'vertical'}>
      {pane.children.map((child, i) => (
        <Allotment.Pane key={i}>
          <SplitPane pane={child} tabId={tabId} />
        </Allotment.Pane>
      ))}
    </Allotment>
  );
}
