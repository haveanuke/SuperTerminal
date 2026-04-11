import { createStore } from '../lib/create-store';
import type { ThemeConfig } from '../types';

const tokyoNight: ThemeConfig = {
  name: 'Tokyo Night',
  background: '#1a1b26',
  foreground: '#c0caf5',
  cursor: '#c0caf5',
  selection: '#33467c',
  black: '#15161e',
  red: '#f7768e',
  green: '#9ece6a',
  yellow: '#e0af68',
  blue: '#7aa2f7',
  magenta: '#bb9af7',
  cyan: '#7dcfff',
  white: '#a9b1d6',
  brightBlack: '#414868',
  brightRed: '#f7768e',
  brightGreen: '#9ece6a',
  brightYellow: '#e0af68',
  brightBlue: '#7aa2f7',
  brightMagenta: '#bb9af7',
  brightCyan: '#7dcfff',
  brightWhite: '#c0caf5',
  uiBackground: '#1a1b26',
  uiSurface: '#24283b',
  uiBorder: '#414868',
  uiAccent: '#7aa2f7',
  uiText: '#c0caf5',
  uiTextMuted: '#565f89',
};

const dracula: ThemeConfig = {
  name: 'Dracula',
  background: '#282a36',
  foreground: '#f8f8f2',
  cursor: '#f8f8f2',
  selection: '#44475a',
  black: '#21222c',
  red: '#ff5555',
  green: '#50fa7b',
  yellow: '#f1fa8c',
  blue: '#bd93f9',
  magenta: '#ff79c6',
  cyan: '#8be9fd',
  white: '#f8f8f2',
  brightBlack: '#6272a4',
  brightRed: '#ff6e6e',
  brightGreen: '#69ff94',
  brightYellow: '#ffffa5',
  brightBlue: '#d6acff',
  brightMagenta: '#ff92df',
  brightCyan: '#a4ffff',
  brightWhite: '#ffffff',
  uiBackground: '#282a36',
  uiSurface: '#343746',
  uiBorder: '#44475a',
  uiAccent: '#bd93f9',
  uiText: '#f8f8f2',
  uiTextMuted: '#6272a4',
};

const catppuccin: ThemeConfig = {
  name: 'Catppuccin Mocha',
  background: '#1e1e2e',
  foreground: '#cdd6f4',
  cursor: '#f5e0dc',
  selection: '#45475a',
  black: '#45475a',
  red: '#f38ba8',
  green: '#a6e3a1',
  yellow: '#f9e2af',
  blue: '#89b4fa',
  magenta: '#f5c2e7',
  cyan: '#94e2d5',
  white: '#bac2de',
  brightBlack: '#585b70',
  brightRed: '#f38ba8',
  brightGreen: '#a6e3a1',
  brightYellow: '#f9e2af',
  brightBlue: '#89b4fa',
  brightMagenta: '#f5c2e7',
  brightCyan: '#94e2d5',
  brightWhite: '#a6adc8',
  uiBackground: '#1e1e2e',
  uiSurface: '#313244',
  uiBorder: '#45475a',
  uiAccent: '#89b4fa',
  uiText: '#cdd6f4',
  uiTextMuted: '#6c7086',
};

const nord: ThemeConfig = {
  name: 'Nord',
  background: '#2e3440',
  foreground: '#d8dee9',
  cursor: '#d8dee9',
  selection: '#434c5e',
  black: '#3b4252',
  red: '#bf616a',
  green: '#a3be8c',
  yellow: '#ebcb8b',
  blue: '#81a1c1',
  magenta: '#b48ead',
  cyan: '#88c0d0',
  white: '#e5e9f0',
  brightBlack: '#4c566a',
  brightRed: '#bf616a',
  brightGreen: '#a3be8c',
  brightYellow: '#ebcb8b',
  brightBlue: '#81a1c1',
  brightMagenta: '#b48ead',
  brightCyan: '#8fbcbb',
  brightWhite: '#eceff4',
  uiBackground: '#2e3440',
  uiSurface: '#3b4252',
  uiBorder: '#4c566a',
  uiAccent: '#88c0d0',
  uiText: '#d8dee9',
  uiTextMuted: '#4c566a',
};

const solarizedDark: ThemeConfig = {
  name: 'Solarized Dark',
  background: '#002b36',
  foreground: '#839496',
  cursor: '#839496',
  selection: '#073642',
  black: '#073642',
  red: '#dc322f',
  green: '#859900',
  yellow: '#b58900',
  blue: '#268bd2',
  magenta: '#d33682',
  cyan: '#2aa198',
  white: '#eee8d5',
  brightBlack: '#586e75',
  brightRed: '#cb4b16',
  brightGreen: '#586e75',
  brightYellow: '#657b83',
  brightBlue: '#839496',
  brightMagenta: '#6c71c4',
  brightCyan: '#93a1a1',
  brightWhite: '#fdf6e3',
  uiBackground: '#002b36',
  uiSurface: '#073642',
  uiBorder: '#586e75',
  uiAccent: '#268bd2',
  uiText: '#839496',
  uiTextMuted: '#586e75',
};

const solarizedLight: ThemeConfig = {
  name: 'Solarized Light',
  background: '#fdf6e3',
  foreground: '#657b83',
  cursor: '#657b83',
  selection: '#eee8d5',
  black: '#073642',
  red: '#dc322f',
  green: '#859900',
  yellow: '#b58900',
  blue: '#268bd2',
  magenta: '#d33682',
  cyan: '#2aa198',
  white: '#eee8d5',
  brightBlack: '#002b36',
  brightRed: '#cb4b16',
  brightGreen: '#586e75',
  brightYellow: '#657b83',
  brightBlue: '#839496',
  brightMagenta: '#6c71c4',
  brightCyan: '#93a1a1',
  brightWhite: '#fdf6e3',
  uiBackground: '#fdf6e3',
  uiSurface: '#eee8d5',
  uiBorder: '#93a1a1',
  uiAccent: '#268bd2',
  uiText: '#657b83',
  uiTextMuted: '#93a1a1',
};

const gruvboxDark: ThemeConfig = {
  name: 'Gruvbox Dark',
  background: '#282828',
  foreground: '#ebdbb2',
  cursor: '#ebdbb2',
  selection: '#504945',
  black: '#282828',
  red: '#cc241d',
  green: '#98971a',
  yellow: '#d79921',
  blue: '#458588',
  magenta: '#b16286',
  cyan: '#689d6a',
  white: '#a89984',
  brightBlack: '#928374',
  brightRed: '#fb4934',
  brightGreen: '#b8bb26',
  brightYellow: '#fabd2f',
  brightBlue: '#83a598',
  brightMagenta: '#d3869b',
  brightCyan: '#8ec07c',
  brightWhite: '#ebdbb2',
  uiBackground: '#282828',
  uiSurface: '#3c3836',
  uiBorder: '#504945',
  uiAccent: '#fabd2f',
  uiText: '#ebdbb2',
  uiTextMuted: '#928374',
};

const oneDark: ThemeConfig = {
  name: 'One Dark',
  background: '#282c34',
  foreground: '#abb2bf',
  cursor: '#528bff',
  selection: '#3e4451',
  black: '#282c34',
  red: '#e06c75',
  green: '#98c379',
  yellow: '#e5c07b',
  blue: '#61afef',
  magenta: '#c678dd',
  cyan: '#56b6c2',
  white: '#abb2bf',
  brightBlack: '#5c6370',
  brightRed: '#e06c75',
  brightGreen: '#98c379',
  brightYellow: '#e5c07b',
  brightBlue: '#61afef',
  brightMagenta: '#c678dd',
  brightCyan: '#56b6c2',
  brightWhite: '#ffffff',
  uiBackground: '#282c34',
  uiSurface: '#21252b',
  uiBorder: '#3e4451',
  uiAccent: '#61afef',
  uiText: '#abb2bf',
  uiTextMuted: '#5c6370',
};

const monokai: ThemeConfig = {
  name: 'Monokai',
  background: '#272822',
  foreground: '#f8f8f2',
  cursor: '#f8f8f0',
  selection: '#49483e',
  black: '#272822',
  red: '#f92672',
  green: '#a6e22e',
  yellow: '#f4bf75',
  blue: '#66d9ef',
  magenta: '#ae81ff',
  cyan: '#a1efe4',
  white: '#f8f8f2',
  brightBlack: '#75715e',
  brightRed: '#f92672',
  brightGreen: '#a6e22e',
  brightYellow: '#f4bf75',
  brightBlue: '#66d9ef',
  brightMagenta: '#ae81ff',
  brightCyan: '#a1efe4',
  brightWhite: '#f9f8f5',
  uiBackground: '#272822',
  uiSurface: '#3e3d32',
  uiBorder: '#49483e',
  uiAccent: '#a6e22e',
  uiText: '#f8f8f2',
  uiTextMuted: '#75715e',
};

const rosePine: ThemeConfig = {
  name: 'Rose Pine',
  background: '#191724',
  foreground: '#e0def4',
  cursor: '#524f67',
  selection: '#2a283e',
  black: '#26233a',
  red: '#eb6f92',
  green: '#31748f',
  yellow: '#f6c177',
  blue: '#9ccfd8',
  magenta: '#c4a7e7',
  cyan: '#ebbcba',
  white: '#e0def4',
  brightBlack: '#6e6a86',
  brightRed: '#eb6f92',
  brightGreen: '#31748f',
  brightYellow: '#f6c177',
  brightBlue: '#9ccfd8',
  brightMagenta: '#c4a7e7',
  brightCyan: '#ebbcba',
  brightWhite: '#e0def4',
  uiBackground: '#191724',
  uiSurface: '#1f1d2e',
  uiBorder: '#26233a',
  uiAccent: '#c4a7e7',
  uiText: '#e0def4',
  uiTextMuted: '#6e6a86',
};

const kanagawa: ThemeConfig = {
  name: 'Kanagawa',
  background: '#1f1f28',
  foreground: '#dcd7ba',
  cursor: '#c8c093',
  selection: '#2d4f67',
  black: '#16161d',
  red: '#c34043',
  green: '#76946a',
  yellow: '#c0a36e',
  blue: '#7e9cd8',
  magenta: '#957fb8',
  cyan: '#6a9589',
  white: '#c8c093',
  brightBlack: '#727169',
  brightRed: '#e82424',
  brightGreen: '#98bb6c',
  brightYellow: '#e6c384',
  brightBlue: '#7fb4ca',
  brightMagenta: '#938aa9',
  brightCyan: '#7aa89f',
  brightWhite: '#dcd7ba',
  uiBackground: '#1f1f28',
  uiSurface: '#2a2a37',
  uiBorder: '#54546d',
  uiAccent: '#7e9cd8',
  uiText: '#dcd7ba',
  uiTextMuted: '#727169',
};

const everforest: ThemeConfig = {
  name: 'Everforest',
  background: '#2d353b',
  foreground: '#d3c6aa',
  cursor: '#d3c6aa',
  selection: '#475258',
  black: '#343f44',
  red: '#e67e80',
  green: '#a7c080',
  yellow: '#dbbc7f',
  blue: '#7fbbb3',
  magenta: '#d699b6',
  cyan: '#83c092',
  white: '#d3c6aa',
  brightBlack: '#859289',
  brightRed: '#e67e80',
  brightGreen: '#a7c080',
  brightYellow: '#dbbc7f',
  brightBlue: '#7fbbb3',
  brightMagenta: '#d699b6',
  brightCyan: '#83c092',
  brightWhite: '#d3c6aa',
  uiBackground: '#2d353b',
  uiSurface: '#343f44',
  uiBorder: '#475258',
  uiAccent: '#a7c080',
  uiText: '#d3c6aa',
  uiTextMuted: '#859289',
};

export const builtinThemes: ThemeConfig[] = [
  tokyoNight, dracula, catppuccin, nord,
  solarizedDark, solarizedLight, gruvboxDark, oneDark,
  monokai, rosePine, kanagawa, everforest,
];

// Custom theme persistence via localStorage
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

// Required fields for a valid theme
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
  fontSize: number;
  fontFamily: string;
  opacity: number;
  setTheme: (theme: ThemeConfig) => void;
  setFontSize: (size: number) => void;
  setFontFamily: (family: string) => void;
  setOpacity: (opacity: number) => void;
  addCustomTheme: (theme: ThemeConfig) => void;
  removeCustomTheme: (name: string) => void;
  getAllThemes: () => ThemeConfig[];
}

export const useThemeStore = createStore<ThemeStore>((set, get) => ({
  theme: tokyoNight,
  customThemes: loadCustomThemes(),
  fontSize: 14,
  fontFamily: 'JetBrains Mono, Menlo, Monaco, Consolas, monospace',
  opacity: 1,
  setTheme: (theme) => set({ theme }),
  setFontSize: (fontSize) => set({ fontSize }),
  setFontFamily: (fontFamily) => set({ fontFamily }),
  setOpacity: (opacity) => set({ opacity }),
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
    // If current theme was removed, fall back to default
    if (get().theme.name === name) set({ theme: tokyoNight });
  },
  getAllThemes: () => [...builtinThemes, ...get().customThemes],
}));

// Keep backward compat export
export const themes = builtinThemes;
