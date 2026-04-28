import { createStore } from '../lib/create-store';

const FONT_SIZE_KEY = 'superTerminal:fontSize';
const FONT_FAMILY_KEY = 'superTerminal:fontFamily';
const BG_IMAGE_KEY = 'superTerminal:backgroundImage';
const BG_OPACITY_KEY = 'superTerminal:backgroundOpacity';

const DEFAULT_FONT_SIZE = 14;
const DEFAULT_FONT_FAMILY = 'JetBrains Mono, Menlo, Monaco, Consolas, monospace';
const DEFAULT_BG_OPACITY = 0.3;

function loadFontSize(): number {
  const stored = localStorage.getItem(FONT_SIZE_KEY);
  const parsed = stored ? Number(stored) : DEFAULT_FONT_SIZE;
  return Number.isFinite(parsed) && parsed > 0 ? parsed : DEFAULT_FONT_SIZE;
}

function loadFontFamily(): string {
  return localStorage.getItem(FONT_FAMILY_KEY) || DEFAULT_FONT_FAMILY;
}

function loadBackgroundImage(): string | null {
  return localStorage.getItem(BG_IMAGE_KEY);
}

function loadBackgroundOpacity(): number {
  const stored = localStorage.getItem(BG_OPACITY_KEY);
  const parsed = stored ? Number(stored) : DEFAULT_BG_OPACITY;
  return Number.isFinite(parsed) ? parsed : DEFAULT_BG_OPACITY;
}

interface UIStore {
  fontSize: number;
  fontFamily: string;
  backgroundImage: string | null;
  backgroundOpacity: number;
  setFontSize: (size: number) => void;
  setFontFamily: (family: string) => void;
  setBackgroundImage: (path: string | null) => void;
  setBackgroundOpacity: (opacity: number) => void;
}

export const useUIStore = createStore<UIStore>((set) => ({
  fontSize: loadFontSize(),
  fontFamily: loadFontFamily(),
  backgroundImage: loadBackgroundImage(),
  backgroundOpacity: loadBackgroundOpacity(),
  setFontSize: (fontSize) => {
    localStorage.setItem(FONT_SIZE_KEY, String(fontSize));
    set({ fontSize });
  },
  setFontFamily: (fontFamily) => {
    localStorage.setItem(FONT_FAMILY_KEY, fontFamily);
    set({ fontFamily });
  },
  setBackgroundImage: (path) => {
    if (path) localStorage.setItem(BG_IMAGE_KEY, path);
    else localStorage.removeItem(BG_IMAGE_KEY);
    set({ backgroundImage: path });
  },
  setBackgroundOpacity: (opacity) => {
    localStorage.setItem(BG_OPACITY_KEY, String(opacity));
    set({ backgroundOpacity: opacity });
  },
}));
