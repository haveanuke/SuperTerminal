import { useThemeStore } from '../../stores/theme-store';
import { useGitStore } from '../../stores/git-store';
import { ArrowDown, ArrowUp, GitBranch, RefreshCw, SyncArrows } from '../icons';

export function GitHeader() {
  const theme = useThemeStore((s) => s.theme);
  const repo = useGitStore((s) => s.repo);
  const report = useGitStore((s) => s.report);
  const busy = useGitStore((s) => s.busy);
  const runAction = useGitStore((s) => s.runAction);
  const refresh = useGitStore((s) => s.refresh);

  if (!repo) return null;

  const branchLabel = report
    ? report.branch ??
      (report.detached ? `detached @ ${report.detached}` : report.unborn ? 'no commits yet' : '...')
    : '...';

  return (
    <div
      style={{
        display: 'flex',
        alignItems: 'center',
        gap: 6,
        padding: '6px 10px',
        borderBottom: `1px solid ${theme.uiBorder}`,
        fontSize: 12,
        color: theme.uiText,
      }}
    >
      <GitBranch size={13} style={{ color: theme.uiAccent, flexShrink: 0 }} />
      <span
        style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}
        title={`${repo.root} — ${branchLabel}`}
      >
        <span style={{ fontWeight: 'bold' }}>{repo.displayName}</span>
        <span style={{ color: theme.uiTextMuted }}> {branchLabel}</span>
      </span>
      <span style={{ flex: 1 }} />
      {report?.upstream && (report.ahead > 0 || report.behind > 0) && (
        <span style={{ color: theme.uiTextMuted, fontSize: 11, display: 'flex', alignItems: 'center', gap: 2 }}>
          {report.ahead > 0 && (
            <>
              <ArrowUp size={11} />
              {report.ahead}
            </>
          )}
          {report.behind > 0 && (
            <>
              <ArrowDown size={11} />
              {report.behind}
            </>
          )}
        </span>
      )}
      <button
        className="toolbar-btn"
        title="Sync (pull, then push)"
        disabled={busy}
        onClick={() => void runAction('sync')}
      >
        <SyncArrows size={13} />
      </button>
      <button
        className="toolbar-btn"
        title="Fetch"
        disabled={busy}
        onClick={() => void runAction('fetch')}
      >
        <ArrowDown size={13} />
      </button>
      <button className="toolbar-btn" title="Refresh" disabled={busy} onClick={refresh}>
        <RefreshCw size={13} />
      </button>
    </div>
  );
}
