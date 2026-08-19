import { useState } from 'react';
import { useThemeStore } from '../../stores/theme-store';
import { useGitStore } from '../../stores/git-store';

export function CommitBox() {
  const theme = useThemeStore((s) => s.theme);
  const report = useGitStore((s) => s.report);
  const busy = useGitStore((s) => s.busy);
  const runAction = useGitStore((s) => s.runAction);
  const [message, setMessage] = useState('');

  const stagedCount = report
    ? report.entries.filter((e) => e.kind !== 'untracked' && e.kind !== 'unmerged' && e.indexStatus !== '.').length
    : 0;
  const canCommit = stagedCount > 0 && message.trim().length > 0 && !busy;

  const commit = async () => {
    if (!canCommit) return;
    await runAction('commit', message);
    // Only clear when the commit actually landed (report no longer has staged entries)
    const after = useGitStore.getState().report;
    const stillStaged = after
      ? after.entries.some((e) => e.kind !== 'untracked' && e.kind !== 'unmerged' && e.indexStatus !== '.')
      : false;
    if (!stillStaged) setMessage('');
  };

  return (
    <div
      style={{
        display: 'flex',
        flexDirection: 'column',
        gap: 6,
        padding: '8px 10px',
        borderBottom: `1px solid ${theme.uiBorder}`,
      }}
    >
      <textarea
        placeholder="Commit message (Cmd+Enter to commit)"
        value={message}
        onChange={(e) => setMessage(e.target.value)}
        rows={2}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && e.metaKey) {
            e.preventDefault();
            void commit();
          }
        }}
        style={{
          background: theme.uiBackground,
          border: `1px solid ${theme.uiBorder}`,
          color: theme.uiText,
          padding: '6px 8px',
          fontSize: 12,
          borderRadius: 4,
          outline: 'none',
          resize: 'vertical',
          fontFamily: 'inherit',
        }}
      />
      <button
        className="toolbar-btn"
        disabled={!canCommit}
        onClick={() => void commit()}
        title={stagedCount === 0 ? 'Nothing staged' : 'Commit staged changes'}
        style={{
          padding: '5px 8px',
          fontSize: 12,
          borderRadius: 4,
          backgroundColor: canCommit ? theme.uiAccent : theme.uiBorder,
          color: canCommit ? theme.uiBackground : theme.uiTextMuted,
        }}
      >
        Commit{stagedCount > 0 ? ` (${stagedCount})` : ''}
      </button>
    </div>
  );
}
