import { createStore } from '../lib/create-store';
import {
  generateBones, hashString, randomUserId,
  type BuddyBones, type StatName,
} from './engine';
import {
  getReaction, generateFallbackName, generatePersonality,
  type ReactionReason,
} from './reactions';
import { speak as ttsSpeak, stopSpeaking } from './tts';

export interface AgentConfig {
  enabled: boolean;
  preset: 'claude' | 'codex' | 'ollama' | 'custom';
  command: string;
  args: string[]; // {prompt} placeholder is substituted at runtime
}

export interface TtsConfig {
  enabled: boolean;
  voice: string | null;
  rate: number;
  pitch: number;
}

const DEFAULT_TTS: TtsConfig = { enabled: false, voice: null, rate: 1, pitch: 1 };

export const AGENT_PRESETS: Record<AgentConfig['preset'], Omit<AgentConfig, 'enabled' | 'preset'>> = {
  claude: { command: 'claude', args: ['-p', '{prompt}'] },
  codex: { command: 'codex', args: ['exec', '{prompt}'] },
  ollama: { command: 'ollama', args: ['run', 'llama3', '{prompt}'] },
  custom: { command: 'claude', args: ['-p', '{prompt}'] },
};

export interface Companion {
  bones: BuddyBones;
  name: string;
  personality: string;
  userId: string;
  hatchedAt: number;
  petCount: number;
}

interface SpeechBubble {
  text: string;
  at: number;
  ttl: number;
}

export interface ChatEntry {
  role: 'user' | 'buddy';
  text: string;
  at: number;
}

const CHAT_LOG_LIMIT = 50;

interface BuddyStoreState {
  companion: Companion | null;
  visible: boolean;
  hasHatched: boolean;
  position: { x: number; y: number };
  frame: number;
  blinking: boolean;
  bubble: SpeechBubble | null;
  cardOpen: boolean;
  chatOpen: boolean;
  chatBusy: boolean;
  logOpen: boolean;
  chatLog: ChatEntry[];
  lastReactionAt: number;
  agent: AgentConfig;
  tts: TtsConfig;

  hatch: () => void;
  reroll: () => void;
  rename: (name: string) => void;
  setPersonality: (personality: string) => void;
  pet: () => void;
  react: (reason: ReactionReason, context?: { line?: number; count?: number; lines?: number; text?: string }) => void;
  setBubbleText: (text: string, ttl?: number) => void;
  dismissBubble: () => void;
  setVisible: (visible: boolean) => void;
  setPosition: (pos: { x: number; y: number }) => void;
  setFrame: (frame: number) => void;
  setBlinking: (blinking: boolean) => void;
  setCardOpen: (open: boolean) => void;
  setChatOpen: (open: boolean) => void;
  setChatBusy: (busy: boolean) => void;
  setLogOpen: (open: boolean) => void;
  addChatEntry: (entry: Omit<ChatEntry, 'at'>) => void;
  clearChatLog: () => void;
  setAgent: (patch: Partial<AgentConfig>) => void;
  setTts: (patch: Partial<TtsConfig>) => void;
  speak: (text: string) => void;
}

const STORAGE_KEY = 'superTerminal:buddy';
const POSITION_KEY = 'superTerminal:buddyPosition';
const VISIBLE_KEY = 'superTerminal:buddyVisible';
const AGENT_KEY = 'superTerminal:buddyAgent';
const TTS_KEY = 'superTerminal:buddyTts';

function loadTts(): TtsConfig {
  try {
    const raw = localStorage.getItem(TTS_KEY);
    if (!raw) return { ...DEFAULT_TTS };
    const parsed = JSON.parse(raw);
    return {
      enabled: !!parsed.enabled,
      voice: typeof parsed.voice === 'string' ? parsed.voice : null,
      rate: typeof parsed.rate === 'number' ? parsed.rate : 1,
      pitch: typeof parsed.pitch === 'number' ? parsed.pitch : 1,
    };
  } catch {
    return { ...DEFAULT_TTS };
  }
}

function saveTts(tts: TtsConfig) {
  try {
    localStorage.setItem(TTS_KEY, JSON.stringify(tts));
  } catch {
    // ignore
  }
}

function loadAgent(): AgentConfig {
  try {
    const raw = localStorage.getItem(AGENT_KEY);
    if (!raw) return { enabled: false, preset: 'claude', ...AGENT_PRESETS.claude };
    const parsed = JSON.parse(raw) as AgentConfig;
    if (!parsed.command || !Array.isArray(parsed.args)) {
      return { enabled: false, preset: 'claude', ...AGENT_PRESETS.claude };
    }
    return parsed;
  } catch {
    return { enabled: false, preset: 'claude', ...AGENT_PRESETS.claude };
  }
}

function saveAgent(agent: AgentConfig) {
  try {
    localStorage.setItem(AGENT_KEY, JSON.stringify(agent));
  } catch {
    // ignore
  }
}

interface Persisted {
  companion: Companion | null;
  hasHatched: boolean;
}

function load(): Persisted {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return { companion: null, hasHatched: false };
    const parsed = JSON.parse(raw) as Persisted;
    let companion = parsed.companion ?? null;
    // Heal buddies saved before the hash sign-bug was fixed
    if (companion) {
      if (!companion.name || typeof companion.name !== 'string') {
        companion = { ...companion, name: generateFallbackName() };
      }
      if (!companion.personality || typeof companion.personality !== 'string') {
        companion = { ...companion, personality: generatePersonality() };
      }
    }
    return { companion, hasHatched: !!parsed.hasHatched };
  } catch {
    return { companion: null, hasHatched: false };
  }
}

function save(companion: Companion | null, hasHatched: boolean) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify({ companion, hasHatched }));
  } catch {
    // ignore
  }
}

function loadPosition(): { x: number; y: number } {
  try {
    const raw = localStorage.getItem(POSITION_KEY);
    if (!raw) return { x: window.innerWidth - 200, y: window.innerHeight - 200 };
    return JSON.parse(raw);
  } catch {
    return { x: window.innerWidth - 200, y: window.innerHeight - 200 };
  }
}

function loadVisible(): boolean {
  const raw = localStorage.getItem(VISIBLE_KEY);
  return raw === null ? true : raw === 'true';
}

function makeCompanion(): Companion {
  const userId = randomUserId();
  const bones = generateBones(userId);
  const nameSeed = (hashString(userId + ':name') % 100000) / 100000;
  const persSeed = (hashString(userId + ':pers') % 100000) / 100000;
  return {
    bones,
    name: generateFallbackName(nameSeed),
    personality: generatePersonality(persSeed),
    userId,
    hatchedAt: Date.now(),
    petCount: 0,
  };
}

const initial = load();
if (initial.companion) save(initial.companion, initial.hasHatched);

export const useBuddyStore = createStore<BuddyStoreState>((set, get) => ({
  companion: initial.companion,
  hasHatched: initial.hasHatched,
  visible: loadVisible(),
  position: loadPosition(),
  frame: 0,
  blinking: false,
  bubble: null,
  cardOpen: false,
  chatOpen: false,
  chatBusy: false,
  logOpen: false,
  chatLog: [],
  lastReactionAt: 0,
  agent: loadAgent(),
  tts: loadTts(),

  hatch: () => {
    const companion = makeCompanion();
    save(companion, true);
    set({ companion, hasHatched: true });
    // Fire hatch reaction after tiny delay so UI can mount first
    setTimeout(() => get().react('hatch'), 400);
  },

  reroll: () => {
    const companion = makeCompanion();
    save(companion, true);
    set({ companion, bubble: null });
    setTimeout(() => get().react('hatch'), 200);
  },

  rename: (name) => {
    const { companion } = get();
    if (!companion) return;
    const next = { ...companion, name: name.slice(0, 14) || companion.name };
    save(next, true);
    set({ companion: next });
  },

  setPersonality: (personality) => {
    const { companion } = get();
    if (!companion) return;
    const next = { ...companion, personality };
    save(next, true);
    set({ companion: next });
  },

  pet: () => {
    const { companion } = get();
    if (!companion) return;
    const next = { ...companion, petCount: companion.petCount + 1 };
    save(next, true);
    set({ companion: next });
    get().react('pet');
  },

  react: (reason, context) => {
    const { companion, lastReactionAt } = get();
    if (!companion) return;
    const now = Date.now();
    // Rate limit: min 2 seconds between reactions (except pet, which is user-driven)
    if (reason !== 'pet' && reason !== 'hatch' && now - lastReactionAt < 2000) return;
    const text = getReaction(reason, companion.bones.species, companion.bones.rarity, context);
    const ttl = reason === 'hatch' ? 12_000 : reason === 'pet' ? 7_000 : 10_000;
    set({ bubble: { text, at: now, ttl }, lastReactionAt: now });
    get().addChatEntry({ role: 'buddy', text });
    get().speak(text);
  },

  setBubbleText: (text, ttl = 6000) => {
    set({ bubble: { text, at: Date.now(), ttl }, lastReactionAt: Date.now() });
  },

  dismissBubble: () => set({ bubble: null }),

  setAgent: (patch) => {
    const next = { ...get().agent, ...patch };
    saveAgent(next);
    set({ agent: next });
  },

  setVisible: (visible) => {
    localStorage.setItem(VISIBLE_KEY, String(visible));
    if (!visible) stopSpeaking();
    set({ visible });
  },

  setPosition: (pos) => {
    localStorage.setItem(POSITION_KEY, JSON.stringify(pos));
    set({ position: pos });
  },

  setFrame: (frame) => set({ frame }),
  setBlinking: (blinking) => set({ blinking }),
  setCardOpen: (cardOpen) => set({ cardOpen }),
  setChatOpen: (chatOpen) => set({ chatOpen }),
  setChatBusy: (chatBusy) => set({ chatBusy }),
  setLogOpen: (logOpen) => set({ logOpen }),

  addChatEntry: ({ role, text }) => {
    const trimmed = text.trim();
    if (!trimmed) return;
    const entry: ChatEntry = { role, text: trimmed, at: Date.now() };
    const next = [...get().chatLog, entry];
    if (next.length > CHAT_LOG_LIMIT) next.splice(0, next.length - CHAT_LOG_LIMIT);
    set({ chatLog: next });
  },

  clearChatLog: () => set({ chatLog: [] }),

  setTts: (patch) => {
    const next = { ...get().tts, ...patch };
    saveTts(next);
    if (!next.enabled) stopSpeaking();
    set({ tts: next });
  },

  speak: (text) => {
    const { tts, visible } = get();
    if (!tts.enabled || !visible) return;
    ttsSpeak(text, { voice: tts.voice, rate: tts.rate, pitch: tts.pitch });
  },
}));

export type { ReactionReason, StatName };
