import { useEffect, useRef, useState } from 'react';
import { useThemeStore } from '../../stores/theme-store';
import { GRAPH_MAX, useGitStore } from '../../stores/git-store';
import { relTime } from '../../lib/rel-time';
import type { ThemeConfig } from '../../types';
import type { GraphRow } from '../../lib/git-bridge';

const ROW_H = 26;
const LANE_W = 12;
const OVERSCAN = 10;
const MAX_GUTTER_LANES = 12;

function laneColors(theme: ThemeConfig): string[] {
  return [theme.uiAccent, theme.green, theme.yellow, theme.magenta, theme.cyan, theme.blue, theme.red];
}

// True x-coordinate per lane — never clamped onto a shared column; lanes past
// the gutter width clip out of view instead of drawing a false topology.
function laneX(lane: number): number {
  return lane * LANE_W + LANE_W / 2 + 2;
}

function RowSvg({ row, colors, gutterW }: { row: GraphRow; colors: string[]; gutterW: number }) {
  const cy = ROW_H / 2;
  // overflow:visible lets edges continue into the next row vertically; the
  // clipPath (wide vertically, bounded horizontally) stops lanes beyond the
  // gutter from drawing over the commit text.
  const clipId = `lane-clip-${row.hash.slice(0, 12)}`;
  return (
    <svg
      width={gutterW}
      height={ROW_H}
      style={{ overflow: 'visible', flexShrink: 0 }}
    >
      <defs>
        <clipPath id={clipId}>
          <rect x={0} y={-ROW_H} width={gutterW} height={ROW_H * 3} />
        </clipPath>
      </defs>
      <g clipPath={`url(#${clipId})`}>
        {row.edges.map((e, i) => {
          const fx = laneX(e.fromLane);
          const tx = laneX(e.toLane);
          const color = colors[e.toLane % colors.length];
          const d =
            fx === tx
              ? `M ${fx} ${cy} L ${fx} ${cy + ROW_H}`
              : `M ${fx} ${cy} C ${fx} ${cy + ROW_H * 0.8}, ${tx} ${cy + ROW_H * 0.2}, ${tx} ${cy + ROW_H}`;
          return <path key={i} d={d} stroke={color} strokeWidth={1.5} fill="none" />;
        })}
        <circle cx={laneX(row.lane)} cy={cy} r={3.5} fill={colors[row.lane % colors.length]} />
      </g>
    </svg>
  );
}

export function GitGraph() {
  const theme = useThemeStore((s) => s.theme);
  const graph = useGitStore((s) => s.graph);
  const graphLimit = useGitStore((s) => s.graphLimit);
  const loadMoreGraph = useGitStore((s) => s.loadMoreGraph);

  const containerRef = useRef<HTMLDivElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewH, setViewH] = useState(400);

  // The container div renders in EVERY state (loading/empty/rows) so the
  // ResizeObserver attaches on first mount and survives data arriving later.
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setViewH(el.clientHeight));
    ro.observe(el);
    setViewH(el.clientHeight);
    return () => ro.disconnect();
  }, []);

  const rows = graph?.rows ?? [];
  const colors = laneColors(theme);
  const gutterW = Math.max(2, Math.min(graph?.laneCount ?? 1, MAX_GUTTER_LANES)) * LANE_W + 4;
  const showLoadMore = rows.length >= graphLimit && graphLimit < GRAPH_MAX;
  const totalH = rows.length * ROW_H + (showLoadMore ? ROW_H + 8 : 0);
  const first = Math.max(0, Math.floor(scrollTop / ROW_H) - OVERSCAN);
  const last = Math.min(rows.length, Math.ceil((scrollTop + viewH) / ROW_H) + OVERSCAN);

  return (
    <div
      ref={containerRef}
      onScroll={(e) => setScrollTop((e.target as HTMLDivElement).scrollTop)}
      style={{ flex: 1, overflowY: 'auto', overflowX: 'hidden', position: 'relative', minHeight: 80 }}
    >
      {graph === null && (
        <div style={{ padding: 12, fontSize: 12, color: theme.uiTextMuted }}>Loading history…</div>
      )}
      {graph !== null && rows.length === 0 && (
        <div style={{ padding: 12, fontSize: 12, color: theme.uiTextMuted }}>No commits</div>
      )}
      <div style={{ height: totalH, position: 'relative' }}>
        {rows.slice(first, last).map((row, i) => {
          const index = first + i;
          const isLastRow = index === rows.length - 1;
          return (
            <div
              key={row.hash}
              title={`${row.subject}\n${row.author} · ${row.hash.slice(0, 8)}`}
              style={{
                position: 'absolute',
                top: index * ROW_H,
                left: 0,
                right: 0,
                height: ROW_H,
                display: 'flex',
                alignItems: 'center',
                gap: 4,
                paddingRight: 8,
                fontSize: 11,
                color: theme.uiText,
                // Suppress the below-row edge stubs on the final row
                overflow: isLastRow ? 'hidden' : undefined,
              }}
            >
              <RowSvg row={row} colors={colors} gutterW={gutterW} />
              {row.refsDisplay && (
                <span
                  style={{
                    flexShrink: 1,
                    maxWidth: '38%',
                    overflow: 'hidden',
                    textOverflow: 'ellipsis',
                    whiteSpace: 'nowrap',
                    backgroundColor: `${theme.uiAccent}22`,
                    color: theme.uiAccent,
                    border: `1px solid ${theme.uiAccent}55`,
                    borderRadius: 8,
                    padding: '0 6px',
                    fontSize: 10,
                  }}
                  title={row.refsDisplay}
                >
                  {row.refsDisplay}
                </span>
              )}
              <span
                style={{ flex: 1, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}
              >
                {row.subject}
              </span>
              <span style={{ flexShrink: 0, color: theme.uiTextMuted, fontSize: 10 }}>
                {row.author.split(' ')[0]} · {relTime(row.time)}
              </span>
            </div>
          );
        })}
        {showLoadMore && (
          <button
            className="toolbar-btn"
            onClick={loadMoreGraph}
            style={{
              position: 'absolute',
              top: rows.length * ROW_H + 4,
              left: 10,
              right: 10,
              padding: '4px 0',
              fontSize: 11,
              borderRadius: 4,
              border: `1px solid ${theme.uiBorder}`,
              color: theme.uiTextMuted,
            }}
          >
            Load more commits
          </button>
        )}
      </div>
    </div>
  );
}
