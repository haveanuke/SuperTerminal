import { useCallback, useEffect, useRef, useState } from 'react';
import { useThemeStore } from '../../stores/theme-store';
import { useGitStore } from '../../stores/git-store';
import { GitHeader } from './GitHeader';
import { CommitBox } from './CommitBox';
import { FileSection } from './FileSection';
import { GitGraph } from './GitGraph';

const WIDTH_KEY = 'superTerminal:gitSidebarWidth';
const MIN_W = 220;
const MAX_W = 500;

function loadWidth(): number {
  const stored = Number(localStorage.getItem(WIDTH_KEY));
  return Number.isFinite(stored) && stored >= MIN_W && stored <= MAX_W ? stored : 300;
}

export function GitSidebar() {
  const theme = useThemeStore((s) => s.theme);
  const open = useGitStore((s) => s.open);
  const repo = useGitStore((s) => s.repo);
  const targetPath = useGitStore((s) => s.targetPath);
  const report = useGitStore((s) => s.report);
  const lastSkipped = useGitStore((s) => s.lastSkipped);

  const [width, setWidth] = useState(loadWidth);
  const dragging = useRef(false);

  const onDragStart = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    dragging.current = true;
  }, []);

  useEffect(() => {
    const onMove = (e: MouseEvent) => {
      if (!dragging.current) return;
      const w = Math.min(MAX_W, Math.max(MIN_W, e.clientX));
      setWidth(w);
    };
    const onUp = () => {
      if (!dragging.current) return;
      dragging.current = false;
      setWidth((w) => {
        localStorage.setItem(WIDTH_KEY, String(w));
        return w;
      });
    };
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
    return () => {
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
    };
  }, []);

  if (!open) return null;

  const entries = report?.entries ?? [];
  const staged = entries.filter(
    (e) => e.kind !== 'untracked' && e.kind !== 'unmerged' && e.indexStatus !== '.'
  );
  const changes = entries.filter(
    (e) => e.kind === 'untracked' || (e.kind !== 'unmerged' && e.worktreeStatus !== '.')
  );
  const conflicts = entries.filter((e) => e.kind === 'unmerged');

  return (
    <div
      className="git-sidebar"
      style={{
        width,
        flexShrink: 0,
        display: 'flex',
        flexDirection: 'column',
        backgroundColor: theme.uiSurface,
        borderRight: `1px solid ${theme.uiBorder}`,
        position: 'relative',
        overflow: 'hidden',
      }}
    >
      {repo ? (
        <>
          <GitHeader />
          {report === null ? (
            // Distinct loading state: zero-count sections would falsely read
            // as a clean repo before the first status arrives.
            <div style={{ padding: 12, fontSize: 12, color: theme.uiTextMuted }}>Loading status…</div>
          ) : (
            <>
              <CommitBox />
              {lastSkipped > 0 && (
                <div style={{ padding: '4px 10px', fontSize: 11, color: theme.uiTextMuted, fontStyle: 'italic' }}>
                  {lastSkipped} {lastSkipped === 1 ? 'entry' : 'entries'} skipped (non-actionable paths)
                </div>
              )}
              <div style={{ overflowY: 'auto', maxHeight: '45%', flexShrink: 0 }}>
                {conflicts.length > 0 && <FileSection kind="conflicts" entries={conflicts} />}
                <FileSection kind="staged" entries={staged} />
                <FileSection kind="changes" entries={changes} />
              </div>
            </>
          )}
          <GitGraph />
        </>
      ) : (
        <div style={{ padding: 16, fontSize: 12, color: theme.uiTextMuted, lineHeight: 1.6 }}>
          {targetPath ? (
            <>
              Not a git repository:
              <br />
              <span style={{ wordBreak: 'break-all', color: theme.uiText }}>{targetPath}</span>
            </>
          ) : (
            'Focus a terminal inside a git repository.'
          )}
        </div>
      )}
      {/* Width drag handle */}
      <div
        onMouseDown={onDragStart}
        style={{
          position: 'absolute',
          top: 0,
          right: -3,
          bottom: 0,
          width: 6,
          cursor: 'col-resize',
          zIndex: 10,
        }}
      />
    </div>
  );
}
