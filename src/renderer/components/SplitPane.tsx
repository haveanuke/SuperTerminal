import { useState, useRef, useCallback } from 'react';
import type { PaneNode } from '../types';
import { TerminalView } from './TerminalView';
import { PaneToolbar } from './PaneToolbar';
import { useTerminalStore } from '../stores/terminal-store';
import { useThemeStore } from '../stores/theme-store';

interface SplitPaneProps {
  pane: PaneNode;
  tabId: string;
}

const MIN_SIZE_PX = 50;
const SNAP_THRESHOLD_PX = 20;
const DIVIDER_SIZE_PX = 4;

interface SplitContainerProps {
  direction: 'horizontal' | 'vertical';
  children: [PaneNode, PaneNode];
  tabId: string;
}

function SplitContainer({ direction, children, tabId }: SplitContainerProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [ratio, setRatio] = useState(0.5);
  const [collapsed, setCollapsed] = useState<null | 0 | 1>(null);
  const dragging = useRef(false);
  const theme = useThemeStore((s) => s.theme);
  const [hovered, setHovered] = useState(false);

  const isVertical = direction === 'vertical';

  const handleMouseDown = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    dragging.current = true;

    const container = containerRef.current;
    if (!container) return;

    const onMouseMove = (e: MouseEvent) => {
      if (!dragging.current || !container) return;
      const rect = container.getBoundingClientRect();
      const total = isVertical ? rect.height : rect.width;
      const offset = isVertical ? e.clientY - rect.top : e.clientX - rect.left;
      const adjustedTotal = total - DIVIDER_SIZE_PX;

      let newRatio = offset / total;
      const minRatio = MIN_SIZE_PX / adjustedTotal;
      const maxRatio = 1 - minRatio;

      // Snap-to-close
      if (offset < SNAP_THRESHOLD_PX) {
        setCollapsed(0);
        setRatio(0);
        return;
      } else if (total - offset < SNAP_THRESHOLD_PX) {
        setCollapsed(1);
        setRatio(1);
        return;
      } else {
        setCollapsed(null);
      }

      newRatio = Math.max(minRatio, Math.min(maxRatio, newRatio));
      setRatio(newRatio);
    };

    const onMouseUp = () => {
      dragging.current = false;
      document.removeEventListener('mousemove', onMouseMove);
      document.removeEventListener('mouseup', onMouseUp);
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
    };

    document.addEventListener('mousemove', onMouseMove);
    document.addEventListener('mouseup', onMouseUp);
    document.body.style.cursor = isVertical ? 'row-resize' : 'col-resize';
    document.body.style.userSelect = 'none';
  }, [isVertical]);

  const handleDoubleClick = useCallback(() => {
    setRatio(0.5);
    setCollapsed(null);
  }, []);

  const cursor = isVertical ? 'row-resize' : 'col-resize';

  const size0 = collapsed === 0 ? '0px' : collapsed === 1 ? '1fr' : `${ratio}fr`;
  const size1 = collapsed === 1 ? '0px' : collapsed === 0 ? '1fr' : `${1 - ratio}fr`;

  const gridTemplate = isVertical
    ? { gridTemplateRows: `${size0} ${DIVIDER_SIZE_PX}px ${size1}`, gridTemplateColumns: '1fr' }
    : { gridTemplateColumns: `${size0} ${DIVIDER_SIZE_PX}px ${size1}`, gridTemplateRows: '1fr' };

  return (
    <div
      ref={containerRef}
      style={{
        display: 'grid',
        ...gridTemplate,
        width: '100%',
        height: '100%',
        overflow: 'hidden',
      }}
    >
      <div style={{ overflow: 'hidden', minWidth: 0, minHeight: 0 }}>
        {collapsed !== 0 && <SplitPane pane={children[0]} tabId={tabId} />}
      </div>
      <div
        onMouseDown={handleMouseDown}
        onDoubleClick={handleDoubleClick}
        onMouseEnter={() => setHovered(true)}
        onMouseLeave={() => setHovered(false)}
        style={{
          cursor,
          backgroundColor: hovered || dragging.current ? theme.uiAccent : theme.uiBorder,
          transition: 'background-color 0.15s',
          zIndex: 1,
        }}
      />
      <div style={{ overflow: 'hidden', minWidth: 0, minHeight: 0 }}>
        {collapsed !== 1 && <SplitPane pane={children[1]} tabId={tabId} />}
      </div>
    </div>
  );
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
    <SplitContainer direction={pane.direction} tabId={tabId}>
      {pane.children}
    </SplitContainer>
  );
}
