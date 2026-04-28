import { createStore } from '../lib/create-store';
import type { ThemeConfig } from '../types';
import { builtinThemes, tokyoNight } from './theme-presets';

export { builtinThemes };

const CUSTOM_THEMES_KEY = 'superTerminal:customThemes';

function loadCustomThemes(): ThemeConfig[] {
  try {
    const stored = localStorage.getItem(CUSTOM_THEMES_KEY);
    return stored ? JSON.parse(stored) : [];
  } catch {
    return [];
  }
}

function saveCustomThemes(themes: ThemeConfig[]) {
  localStorage.setItem(CUSTOM_THEMES_KEY, JSON.stringify(themes));
}

const themeFields: (keyof ThemeConfig)[] = [
  'name', 'background', 'foreground', 'cursor', 'selection',
  'black', 'red', 'green', 'yellow', 'blue', 'magenta', 'cyan', 'white',
  'brightBlack', 'brightRed', 'brightGreen', 'brightYellow', 'brightBlue',
  'brightMagenta', 'brightCyan', 'brightWhite',
  'uiBackground', 'uiSurface', 'uiBorder', 'uiAccent', 'uiText', 'uiTextMuted',
];

export function validateTheme(obj: unknown): obj is ThemeConfig {
  if (!obj || typeof obj !== 'object') return false;
  const record = obj as Record<string, unknown>;
  return themeFields.every((f) => typeof record[f] === 'string' && record[f] !== '');
}

export function exportThemeJSON(theme: ThemeConfig): string {
  return JSON.stringify(theme, null, 2);
}

interface ThemeStore {
  theme: ThemeConfig;
  customThemes: ThemeConfig[];
  setTheme: (theme: ThemeConfig) => void;
  addCustomTheme: (theme: ThemeConfig) => void;
  removeCustomTheme: (name: string) => void;
  getAllThemes: () => ThemeConfig[];
}

export const useThemeStore = createStore<ThemeStore>((set, get) => ({
  theme: tokyoNight,
  customThemes: loadCustomThemes(),
  setTheme: (theme) => set({ theme }),
  addCustomTheme: (theme) => {
    const existing = get().customThemes;
    const filtered = existing.filter((t) => t.name !== theme.name);
    const updated = [...filtered, theme];
    saveCustomThemes(updated);
    set({ customThemes: updated });
  },
  removeCustomTheme: (name) => {
    const updated = get().customThemes.filter((t) => t.name !== name);
    saveCustomThemes(updated);
    set({ customThemes: updated });
    if (get().theme.name === name) set({ theme: tokyoNight });
  },
  getAllThemes: () => [...builtinThemes, ...get().customThemes],
}));
