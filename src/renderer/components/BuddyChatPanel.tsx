import { useState } from 'react';
import { useBuddyStore } from '../buddy/buddy-store';
import { useThemeStore } from '../stores/theme-store';
import { talkToBuddy } from '../buddy/talk';
import { getChatPanelPlacement } from '../lib/buddy-layout';

interface Props {
  position: { x: number; y: number };
}

export function BuddyChatPanel({ position }: Props) {
  const companion = useBuddyStore((s) => s.companion);
  const chatBusy = useBuddyStore((s) => s.chatBusy);
  const agent = useBuddyStore((s) => s.agent);
  const setChatOpen = useBuddyStore((s) => s.setChatOpen);
  const setChatBusy = useBuddyStore((s) => s.setChatBusy);
  const setBubbleText = useBuddyStore((s) => s.setBubbleText);
  const theme = useThemeStore((s) => s.theme);

  const [draft, setDraft] = useState('');

  const send = async () => {
    const msg = draft.trim();
    if (!msg || !companion || chatBusy) return;
    setChatBusy(true);
    setBubbleText('...', 60_000);
    const res = await talkToBuddy(companion, agent, msg);
    setChatBusy(false);
    if (res.ok && res.text) {
      setBubbleText(res.text, 9_000);
    } else {
      setBubbleText(res.error || '*silence*', 4_000);
    }
    setDraft('');
    setChatOpen(false);
  };

  if (!companion) return null;
  const placement = getChatPanelPlacement(position, { width: window.innerWidth, height: window.innerHeight });

  return (
    <div
      onMouseDown={(e) => e.stopPropagation()}
      style={{
        position: 'fixed',
        top: placement.top,
        left: placement.left,
        width: placement.width,
        display: 'flex',
        gap: 4,
        background: theme.uiSurface,
        border: `1px solid ${theme.uiBorder}`,
        borderRadius: 8,
        padding: 6,
        boxShadow: '0 4px 16px rgba(0,0,0,0.35)',
        zIndex: 201,
        boxSizing: 'border-box',
      }}
    >
      <input
        autoFocus
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter') send();
          else if (e.key === 'Escape') {
            setChatOpen(false);
            setDraft('');
          }
        }}
        placeholder={agent.enabled ? `talk to ${companion.name}...` : 'enable smart mode in settings'}
        disabled={!agent.enabled || chatBusy}
        style={{
          flex: '1 1 auto',
          minWidth: 0,
          background: theme.uiBackground,
          color: theme.uiText,
          border: `1px solid ${theme.uiBorder}`,
          padding: '3px 8px',
          fontSize: 12,
          borderRadius: 4,
          outline: 'none',
        }}
      />
      <button
        className="toolbar-btn"
        onClick={send}
        disabled={!agent.enabled || chatBusy || !draft.trim()}
        style={{ fontSize: 11, flex: '0 0 auto', whiteSpace: 'nowrap' }}
      >
        {chatBusy ? '...' : 'send'}
      </button>
    </div>
  );
}
