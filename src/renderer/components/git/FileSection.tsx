import { useThemeStore } from '../../stores/theme-store';
import { useGitStore } from '../../stores/git-store';
import type { StatusEntry } from '../../lib/git-bridge';
import type { ThemeConfig } from '../../types';
import { Plus, Minus, Undo } from '../icons';

export type SectionKind = 'staged' | 'changes' | 'conflicts';

interface FileSectionProps {
  kind: SectionKind;
  entries: StatusEntry[];
}

function statusColor(letter: string, theme: ThemeConfig): string {
  switch (letter) {
    case 'M':
      return theme.yellow;
    case 'A':
      return theme.green;
    case 'D':
      return theme.red;
    case '?':
    case 'U':
      return theme.magenta;
    case 'R':
    case 'C':
      return theme.blue;
    default:
      return theme.uiTextMuted;
  }
}

const TITLES: Record<SectionKind, string> = {
  staged: 'Staged',
  changes: 'Changes',
  conflicts: 'Merge conflicts',
};

export function FileSection({ kind, entries }: FileSectionProps) {
  const theme = useThemeStore((s) => s.theme);
  const busy = useGitStore((s) => s.busy);
  const runAction = useGitStore((s) => s.runAction);

  if (entries.length === 0 && kind !== 'changes' && kind !== 'staged') return null;

  const letter = (e: StatusEntry) => (kind === 'staged' ? e.indexStatus : e.worktreeStatus);

  const confirmDiscard = (e: StatusEntry): boolean => {
    const untracked = e.kind === 'untracked';
    const label = e.origPath ? `${e.origPath} → ${e.path}` : e.path;
    if (!window.confirm(`Discard changes to ${label}?`)) return false;
    if (untracked) {
      return window.confirm(`${label} is untracked — deleting it cannot be undone. Delete?`);
    }
    return true;
  };

  const bulk = () => {
    if (kind === 'staged') void runAction('unstageAll');
    else if (kind === 'changes') void runAction('stageAll');
  };

  const rowActions = (e: StatusEntry) => {
    if (!e.actionable) return null;
    if (kind === 'staged') {
      return (
        <button
          className="toolbar-btn"
          title="Unstage"
          disabled={busy}
          onClick={() => void runAction('unstage', [e.path])}
        >
          <Minus size={12} />
        </button>
      );
    }
    if (kind === 'conflicts') {
      return (
        <button
          className="toolbar-btn"
          title="Stage as resolved"
          disabled={busy}
          onClick={() => void runAction('stage', [e.path])}
        >
          <Plus size={12} />
        </button>
      );
    }
    return (
      <>
        <button
          className="toolbar-btn"
          title="Discard changes"
          disabled={busy}
          onClick={() => {
            if (confirmDiscard(e)) void runAction('discard', [e.path]);
          }}
        >
          <Undo size={12} />
        </button>
        <button
          className="toolbar-btn"
          title="Stage"
          disabled={busy}
          onClick={() => void runAction('stage', [e.path])}
        >
          <Plus size={12} />
        </button>
      </>
    );
  };

  return (
    <div style={{ borderBottom: `1px solid ${theme.uiBorder}` }}>
      <div
        style={{
          display: 'flex',
          alignItems: 'center',
          padding: '4px 10px',
          fontSize: 11,
          fontWeight: 'bold',
          textTransform: 'uppercase',
          letterSpacing: 0.5,
          color: kind === 'conflicts' ? theme.magenta : theme.uiTextMuted,
        }}
      >
        <span style={{ flex: 1 }}>
          {TITLES[kind]} ({entries.length})
        </span>
        {kind !== 'conflicts' && entries.length > 0 && (
          <button
            className="toolbar-btn"
            style={{ fontSize: 10 }}
            disabled={busy}
            onClick={bulk}
            title={kind === 'staged' ? 'Unstage all' : 'Stage all'}
          >
            {kind === 'staged' ? <Minus size={11} /> : <Plus size={11} />} all
          </button>
        )}
      </div>
      {entries.map((e) => (
        <div
          key={`${e.path}|${letter(e)}`}
          className="git-file-row"
          style={{
            display: 'flex',
            alignItems: 'center',
            gap: 6,
            padding: '2px 10px',
            fontSize: 12,
            color: e.actionable ? theme.uiText : theme.uiTextMuted,
            opacity: e.actionable ? 1 : 0.6,
          }}
        >
          <span
            style={{
              width: 12,
              flexShrink: 0,
              fontWeight: 'bold',
              color: statusColor(letter(e), theme),
            }}
          >
            {letter(e) === '?' ? 'U' : letter(e)}
          </span>
          <span
            title={e.origPath ? `${e.origPath} → ${e.path}` : e.path}
            style={{
              flex: 1,
              overflow: 'hidden',
              textOverflow: 'ellipsis',
              whiteSpace: 'nowrap',
              direction: 'rtl',
              textAlign: 'left',
            }}
          >
            {e.origPath ? `${e.origPath} → ${e.path}` : e.path}
          </span>
          <span className="git-row-actions" style={{ display: 'flex', flexShrink: 0 }}>
            {rowActions(e)}
          </span>
        </div>
      ))}
    </div>
  );
}
