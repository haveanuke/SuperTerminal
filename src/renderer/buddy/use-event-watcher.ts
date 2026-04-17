import { useEffect, useRef } from 'react';
import { useTerminalStore } from '../stores/terminal-store';
import { useBuddyStore } from './buddy-store';
import type { Companion } from './buddy-store';
import type { ReactionReason } from './reactions';

const IDLE_MS = 90_000;
const TURN_MIN_MS = 120_000;
const SMART_COOLDOWN_MS = 8_000;
const REVIEW_QUIET_MS = 5_000;          // quiet window after activity before buddy reviews
const REVIEW_MIN_CHARS = 120;           // don't review trivial changes
const REVIEW_MAX_PAUSE_BETWEEN = 60_000; // don't conflate separate sessions of work
const ERROR_RE = /(?:^|\s)(?:error|ERROR|Error):/;
const TRACEBACK_RE = /Traceback \(most recent call last\)|Exception in thread|\bstack trace\b/i;
const CMD_NOT_FOUND_RE = /command not found|is not recognized as|No such file or directory/;
const TEST_FAIL_RE = /\b(\d+)\s+(?:tests?\s+)?failed\b|\bFAIL(?:ED)?\s+[\w./-]+|\u2716\s+\d+\s+failing/i;
const LARGE_CHUNK_LINES = 60;

const ANSI_RE = /\x1b\[[0-9;?]*[a-zA-Z]/g;
function stripAnsi(s: string): string { return s.replace(ANSI_RE, ''); }

function buildPrompt(companion: Companion, reason: ReactionReason, snippet: string): string {
  const { bones, name, personality } = companion;
  const statLines = (['DEBUGGING', 'PATIENCE', 'CHAOS', 'WISDOM', 'SNARK'] as const)
    .map((s) => `${s}: ${bones.stats[s]}`).join(', ');

  const eventDesc: Record<ReactionReason, string> = {
    hatch: 'just hatched into existence',
    pet: 'just got petted by the developer',
    error: 'saw an error in the terminal output',
    'test-fail': 'saw tests fail in the terminal output',
    'large-output': 'saw a wall of output scroll by in the terminal',
    turn: 'is just watching the developer work',
    idle: 'has been sitting in a quiet terminal for a while',
    command: 'saw the developer run a new command',
    review: "is reviewing what just happened in the terminal (maybe Claude/Codex/the dev ran commands, edited files, or produced output — comment on what you notice, whether it looks right, and anything that might've been missed)",
  };

  return [
    `You are ${name}, a ${bones.rarity} ${bones.species} companion living in a developer's terminal.`,
    `Your stats: ${statLines}. Peak stat: ${bones.peak}. Weakest stat: ${bones.dump}.`,
    `Personality: ${personality}`,
    '',
    `Event: the developer ${eventDesc[reason]}.`,
    snippet ? `Recent terminal output (tail):\n---\n${snippet}\n---` : '',
    '',
    'React to this event, in character, with EXACTLY ONE short sentence (max 18 words).',
    'Use action-asterisks sparingly (e.g. *blinks*). No preamble, no markdown, no explanation.',
    'Let your stats colour the tone: high CHAOS = chaotic, high SNARK = sarcastic, high WISDOM = sage, low PATIENCE = tetchy.',
    'Output ONLY the reaction text, nothing else.',
  ].filter(Boolean).join('\n');
}

export function useBuddyEventWatcher() {
  const activeTerminalId = useTerminalStore((s) => s.activeTerminalId);
  const idleTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const reviewTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const lastTurnAt = useRef(0);
  const lastSmartAt = useRef(0);
  const smartInFlight = useRef(false);
  const recentOutputRef = useRef<string[]>([]);
  const unreviewedCharsRef = useRef(0);
  const lastChunkAtRef = useRef(0);

  useEffect(() => {
    if (!activeTerminalId) return;

    const resetIdle = () => {
      if (idleTimerRef.current) clearTimeout(idleTimerRef.current);
      idleTimerRef.current = setTimeout(() => {
        const s = useBuddyStore.getState();
        // No idle chatter in smart mode — keeps the buddy quiet unless it has something real to say
        if (s.visible && !s.agent.enabled) fireReaction('idle', '');
      }, IDLE_MS);
    };

    const fireReaction = async (reason: ReactionReason, snippet: string, ctx?: { line?: number; count?: number; lines?: number }) => {
      const state = useBuddyStore.getState();
      const { companion, agent } = state;
      if (!companion) return;

      if (agent.enabled) {
        // In smart mode, ONLY the agent speaks. No canned fallback — silence beats noise.
        const smartEligible =
          reason === 'error' ||
          reason === 'test-fail' ||
          reason === 'hatch' ||
          reason === 'large-output' ||
          reason === 'review';
        if (!smartEligible) return;
        if (Date.now() - lastSmartAt.current < SMART_COOLDOWN_MS) return;
        if (smartInFlight.current) return;

        lastSmartAt.current = Date.now();
        smartInFlight.current = true;
        const prompt = buildPrompt(companion, reason, snippet.slice(-1200));
        try {
          const res = await window.superTerminal.buddy.react({
            command: agent.command,
            args: agent.args,
            prompt,
          });
          if (res.ok && res.text) {
            useBuddyStore.getState().setBubbleText(res.text, 7000);
          } else {
            console.warn('[buddy] smart agent failed:', res.error || '(no text)');
          }
        } catch (err) {
          console.warn('[buddy] smart agent threw:', err);
        } finally {
          smartInFlight.current = false;
        }
        return;
      }

      // Smart mode off: use the canned reaction pool as before
      useBuddyStore.getState().react(reason, ctx);
    };

    resetIdle();

    const scheduleReview = () => {
      if (reviewTimerRef.current) clearTimeout(reviewTimerRef.current);
      reviewTimerRef.current = setTimeout(() => {
        const state = useBuddyStore.getState();
        if (!state.agent.enabled) return;
        if (unreviewedCharsRef.current < REVIEW_MIN_CHARS) return;
        unreviewedCharsRef.current = 0;
        const snippet = recentOutputRef.current.join('').slice(-2000);
        fireReaction('review', snippet);
      }, REVIEW_QUIET_MS);
    };

    const unsub = window.superTerminal.pty.onData(activeTerminalId, (data) => {
      resetIdle();
      const now = Date.now();

      const clean = stripAnsi(data);
      // Keep a rolling tail (~4KB) for the smart prompt
      recentOutputRef.current.push(clean);
      while (recentOutputRef.current.join('').length > 4000) {
        recentOutputRef.current.shift();
      }
      const snippet = recentOutputRef.current.join('').slice(-2000);
      const lineCount = (clean.match(/\n/g) || []).length;

      // Track activity for the post-quiet review. If there's been a very long pause,
      // reset the counter so we only review the *current* burst of work.
      if (now - lastChunkAtRef.current > REVIEW_MAX_PAUSE_BETWEEN) {
        unreviewedCharsRef.current = 0;
      }
      unreviewedCharsRef.current += clean.length;
      lastChunkAtRef.current = now;
      scheduleReview();

      if (TEST_FAIL_RE.test(clean)) {
        const m = clean.match(/(\d+)\s+(?:tests?\s+)?failed/i);
        unreviewedCharsRef.current = 0;
        fireReaction('test-fail', snippet, m ? { count: Number(m[1]) } : undefined);
        return;
      }
      if (ERROR_RE.test(clean) || TRACEBACK_RE.test(clean) || CMD_NOT_FOUND_RE.test(clean)) {
        unreviewedCharsRef.current = 0;
        fireReaction('error', snippet);
        return;
      }
      if (lineCount >= LARGE_CHUNK_LINES) {
        unreviewedCharsRef.current = 0;
        fireReaction('large-output', snippet, { lines: lineCount });
        return;
      }
      if (!useBuddyStore.getState().agent.enabled
          && now - lastTurnAt.current > TURN_MIN_MS
          && Math.random() < 0.06) {
        lastTurnAt.current = now;
        useBuddyStore.getState().react('turn');
      }
    });

    return () => {
      unsub();
      if (idleTimerRef.current) clearTimeout(idleTimerRef.current);
      if (reviewTimerRef.current) clearTimeout(reviewTimerRef.current);
    };
  }, [activeTerminalId]);
}
