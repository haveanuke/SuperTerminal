import { useEffect, useRef, useState } from 'react';
import { useBuddyStore } from '../buddy/buddy-store';
import { useThemeStore } from '../stores/theme-store';
import { getArtFrame, applyHat } from '../buddy/species-art';
import { RARITY_COLOR, RARITY_STARS, STAT_NAMES } from '../buddy/engine';
import { talkToBuddy } from '../buddy/talk';
import { X } from './icons';

const FRAME_INTERVAL = 900;
const BLINK_CHANCE = 0.2;

export function ClaudeBuddy() {
  const companion = useBuddyStore((s) => s.companion);
  const hasHatched = useBuddyStore((s) => s.hasHatched);
  const visible = useBuddyStore((s) => s.visible);
  const position = useBuddyStore((s) => s.position);
  const bubble = useBuddyStore((s) => s.bubble);
  const cardOpen = useBuddyStore((s) => s.cardOpen);
  const chatOpen = useBuddyStore((s) => s.chatOpen);
  const chatBusy = useBuddyStore((s) => s.chatBusy);
  const agent = useBuddyStore((s) => s.agent);
  const hatch = useBuddyStore((s) => s.hatch);
  const pet = useBuddyStore((s) => s.pet);
  const setPosition = useBuddyStore((s) => s.setPosition);
  const dismissBubble = useBuddyStore((s) => s.dismissBubble);
  const setCardOpen = useBuddyStore((s) => s.setCardOpen);
  const setChatOpen = useBuddyStore((s) => s.setChatOpen);
  const setChatBusy = useBuddyStore((s) => s.setChatBusy);
  const setBubbleText = useBuddyStore((s) => s.setBubbleText);

  const theme = useThemeStore((s) => s.theme);

  const [frame, setFrame] = useState(0);
  const [blink, setBlink] = useState(false);
  const [chatDraft, setChatDraft] = useState('');
  const dragRef = useRef<{ offsetX: number; offsetY: number } | null>(null);

  const sendChat = async () => {
    const msg = chatDraft.trim();
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
    setChatDraft('');
    setChatOpen(false);
  };

  // Hatch on first run
  useEffect(() => {
    if (!hasHatched) hatch();
  }, [hasHatched, hatch]);

  // Animate frames + blink
  useEffect(() => {
    if (!companion || !visible) return;
    const id = setInterval(() => {
      setFrame((f) => (f + 1) % 3);
      if (Math.random() < BLINK_CHANCE) {
        setBlink(true);
        setTimeout(() => setBlink(false), 140);
      }
    }, FRAME_INTERVAL);
    return () => clearInterval(id);
  }, [companion, visible]);

  // Auto-dismiss speech bubble after ttl
  useEffect(() => {
    if (!bubble) return;
    const id = setTimeout(() => dismissBubble(), bubble.ttl);
    return () => clearTimeout(id);
  }, [bubble, dismissBubble]);

  if (!companion || !visible) return null;

  const art = applyHat(getArtFrame(companion.bones.species, companion.bones.eye, frame, blink), companion.bones.hat);
  const color = RARITY_COLOR[companion.bones.rarity];
  const stars = RARITY_STARS[companion.bones.rarity];

  const startDrag = (e: React.MouseEvent) => {
    if (e.button !== 0) return;
    const target = e.currentTarget as HTMLDivElement;
    const rect = target.getBoundingClientRect();
    dragRef.current = { offsetX: e.clientX - rect.left, offsetY: e.clientY - rect.top };
    const onMove = (ev: MouseEvent) => {
      if (!dragRef.current) return;
      const x = Math.max(0, Math.min(window.innerWidth - 160, ev.clientX - dragRef.current.offsetX));
      const y = Math.max(0, Math.min(window.innerHeight - 100, ev.clientY - dragRef.current.offsetY));
      setPosition({ x, y });
    };
    const onUp = () => {
      dragRef.current = null;
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
    };
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
  };

  return (
    <>
      <div
        className="claude-buddy"
        style={{
          position: 'fixed',
          left: position.x,
          top: position.y,
          zIndex: 200,
          userSelect: 'none',
          cursor: 'grab',
          pointerEvents: 'auto',
        }}
        onMouseDown={startDrag}
        onDoubleClick={(e) => {
          e.preventDefault();
          e.stopPropagation();
          setChatOpen(true);
        }}
        onContextMenu={(e) => {
          e.preventDefault();
          setCardOpen(true);
        }}
      >
        {/* Speech bubble */}
        {bubble && (
          <div
            style={{
              position: 'absolute',
              bottom: 'calc(100% + 6px)',
              left: '50%',
              transform: 'translateX(-50%)',
              maxWidth: 240,
              minWidth: 120,
              padding: '6px 10px',
              background: theme.uiSurface,
              color: theme.uiText,
              border: `1px solid ${theme.uiBorder}`,
              borderRadius: 8,
              fontSize: 12,
              fontStyle: 'italic',
              whiteSpace: 'normal',
              textAlign: 'center',
              boxShadow: '0 4px 16px rgba(0,0,0,0.35)',
            }}
          >
            {bubble.text}
            <div
              style={{
                position: 'absolute',
                top: '100%',
                left: '50%',
                transform: 'translateX(-50%)',
                width: 0,
                height: 0,
                borderLeft: '6px solid transparent',
                borderRight: '6px solid transparent',
                borderTop: `6px solid ${theme.uiBorder}`,
              }}
            />
          </div>
        )}

        {chatOpen && (() => {
          // Chat panel is rendered at a fixed viewport position so it can't
          // get clipped by the buddy's own offsetParent, and flips sides
          // depending on which half of the screen the buddy sits in.
          const panelW = 280;
          const panelH = 40;
          const margin = 8;
          const buddyW = 160;
          const buddyH = 100;
          const anchorBelow = position.y + buddyH + panelH + margin < window.innerHeight;
          const left = Math.max(margin, Math.min(window.innerWidth - panelW - margin, position.x + buddyW / 2 - panelW / 2));
          const top = anchorBelow ? position.y + buddyH + margin : Math.max(margin, position.y - panelH - margin);
          return (
            <div
              onMouseDown={(e) => e.stopPropagation()}
              style={{
                position: 'fixed',
                top,
                left,
                width: panelW,
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
                value={chatDraft}
                onChange={(e) => setChatDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') sendChat();
                  else if (e.key === 'Escape') {
                    setChatOpen(false);
                    setChatDraft('');
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
                onClick={sendChat}
                disabled={!agent.enabled || chatBusy || !chatDraft.trim()}
                style={{ fontSize: 11, flex: '0 0 auto', whiteSpace: 'nowrap' }}
              >
                {chatBusy ? '...' : 'send'}
              </button>
            </div>
          );
        })()}

        <div
          onClick={(e) => {
            e.stopPropagation();
            pet();
          }}
          title={`${companion.name} — click to pet, double-click to talk, right-click for details`}
          style={{ textAlign: 'center' }}
        >
          <pre
            style={{
              fontFamily: 'JetBrains Mono, Menlo, Monaco, Consolas, monospace',
              fontSize: 13,
              lineHeight: 1.1,
              color,
              textShadow: companion.bones.shiny
                ? '0 0 6px #ffc107, 0 1px 2px rgba(0,0,0,0.8)'
                : '0 1px 2px rgba(0,0,0,0.8), 0 0 3px rgba(0,0,0,0.6)',
              margin: 0,
              whiteSpace: 'pre',
            }}
          >
            {art.join('\n')}
          </pre>
          <div
            style={{
              fontSize: 12,
              fontWeight: 600,
              marginTop: 3,
              color: theme.uiText,
              textShadow: '0 1px 2px rgba(0,0,0,0.8)',
              fontFamily: 'JetBrains Mono, Menlo, Monaco, Consolas, monospace',
            }}
          >
            {companion.bones.shiny && '\u2728 '}
            <span style={{ color }}>{stars}</span> {companion.name}
          </div>
        </div>
      </div>

      {cardOpen && <BuddyCard onClose={() => setCardOpen(false)} />}
    </>
  );
}

function BuddyCard({ onClose }: { onClose: () => void }) {
  const companion = useBuddyStore((s) => s.companion);
  const rename = useBuddyStore((s) => s.rename);
  const reroll = useBuddyStore((s) => s.reroll);
  const setVisible = useBuddyStore((s) => s.setVisible);
  const theme = useThemeStore((s) => s.theme);
  const [editName, setEditName] = useState(false);
  const [nameDraft, setNameDraft] = useState(companion?.name ?? '');

  if (!companion) return null;

  const color = RARITY_COLOR[companion.bones.rarity];
  const stars = RARITY_STARS[companion.bones.rarity];
  const art = applyHat(getArtFrame(companion.bones.species, companion.bones.eye, 0), companion.bones.hat);

  return (
    <div
      onClick={onClose}
      style={{
        position: 'fixed',
        inset: 0,
        background: 'rgba(0,0,0,0.5)',
        zIndex: 300,
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
      }}
    >
      <div
        onClick={(e) => e.stopPropagation()}
        style={{
          background: theme.uiSurface,
          border: `2px solid ${color}`,
          borderRadius: 10,
          padding: 20,
          width: 360,
          color: theme.uiText,
          fontSize: 13,
          boxShadow: '0 12px 40px rgba(0,0,0,0.5)',
        }}
      >
        <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'flex-start', marginBottom: 8 }}>
          <div style={{ fontSize: 11, color: theme.uiTextMuted, textTransform: 'uppercase', letterSpacing: 1 }}>
            Claude Buddy
          </div>
          <button className="toolbar-btn" onClick={onClose} style={{ padding: 2 }}>
            <X size={12} />
          </button>
        </div>

        <pre
          style={{
            fontFamily: 'JetBrains Mono, Menlo, Monaco, Consolas, monospace',
            color,
            textAlign: 'center',
            margin: '8px 0',
            fontSize: 14,
            lineHeight: 1.1,
            textShadow: companion.bones.shiny ? '0 0 8px #ffc107' : 'none',
          }}
        >
          {art.join('\n')}
        </pre>

        <div style={{ textAlign: 'center', marginBottom: 12 }}>
          {editName ? (
            <input
              autoFocus
              value={nameDraft}
              onChange={(e) => setNameDraft(e.target.value)}
              onBlur={() => {
                if (nameDraft.trim()) rename(nameDraft.trim());
                setEditName(false);
              }}
              onKeyDown={(e) => {
                if (e.key === 'Enter') {
                  if (nameDraft.trim()) rename(nameDraft.trim());
                  setEditName(false);
                } else if (e.key === 'Escape') {
                  setNameDraft(companion.name);
                  setEditName(false);
                }
              }}
              maxLength={14}
              style={{
                background: theme.uiBackground,
                color: theme.uiText,
                border: `1px solid ${theme.uiBorder}`,
                padding: '3px 8px',
                fontSize: 14,
                fontWeight: 'bold',
                borderRadius: 4,
                outline: 'none',
                textAlign: 'center',
              }}
            />
          ) : (
            <div
              onClick={() => {
                setNameDraft(companion.name);
                setEditName(true);
              }}
              style={{ fontSize: 16, fontWeight: 'bold', cursor: 'text' }}
              title="Click to rename"
            >
              {companion.name}
            </div>
          )}
          <div style={{ fontSize: 11, marginTop: 4 }}>
            <span style={{ color }}>
              {companion.bones.shiny && '\u2728 '}
              {companion.bones.rarity.toUpperCase()}
            </span>{' '}
            {companion.bones.species} <span style={{ color }}>{stars}</span>
          </div>
          <div style={{ fontSize: 10, color: theme.uiTextMuted, marginTop: 2 }}>
            eye: {companion.bones.eye} · hat: {companion.bones.hat} · pets: {companion.petCount}
          </div>
        </div>

        <div style={{ display: 'flex', flexDirection: 'column', gap: 3, marginBottom: 12 }}>
          {STAT_NAMES.map((stat) => {
            const val = companion.bones.stats[stat];
            const filled = Math.round(val / 10);
            const bar = '\u2588'.repeat(filled) + '\u2591'.repeat(10 - filled);
            const marker =
              stat === companion.bones.peak ? ' \u25b2' : stat === companion.bones.dump ? ' \u25bc' : '';
            return (
              <div key={stat} style={{ display: 'flex', alignItems: 'center', gap: 8, fontFamily: 'monospace', fontSize: 11 }}>
                <span style={{ width: 70, color: theme.uiTextMuted }}>{stat}</span>
                <span style={{ color, letterSpacing: 1 }}>{bar}</span>
                <span style={{ width: 28, textAlign: 'right' }}>{val}{marker}</span>
              </div>
            );
          })}
        </div>

        <div style={{ fontSize: 12, fontStyle: 'italic', color: theme.uiTextMuted, textAlign: 'center', marginBottom: 12 }}>
          "{companion.personality}"
        </div>

        <div style={{ display: 'flex', gap: 6, justifyContent: 'center', flexWrap: 'wrap' }}>
          <button
            className="toolbar-btn"
            onClick={() => {
              if (confirm('Re-roll buddy? Your current companion will be replaced.')) {
                reroll();
              }
            }}
            style={{ fontSize: 11 }}
          >
            Re-roll
          </button>
          <button
            className="toolbar-btn"
            onClick={() => {
              setVisible(false);
              onClose();
            }}
            style={{ fontSize: 11 }}
          >
            Hide
          </button>
        </div>
      </div>
    </div>
  );
}
