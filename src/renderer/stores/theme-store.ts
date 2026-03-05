import { create } from 'zustand';
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

export const themes: ThemeConfig[] = [tokyoNight, dracula, catppuccin];

interface ThemeStore {
  theme: ThemeConfig;
  fontSize: number;
  fontFamily: string;
  opacity: number;
  setTheme: (theme: ThemeConfig) => void;
  setFontSize: (size: number) => void;
  setFontFamily: (family: string) => void;
  setOpacity: (opacity: number) => void;
}

export const useThemeStore = create<ThemeStore>((set) => ({
  theme: tokyoNight,
  fontSize: 14,
  fontFamily: 'JetBrains Mono, Menlo, Monaco, Consolas, monospace',
  opacity: 1,
  setTheme: (theme) => set({ theme }),
  setFontSize: (fontSize) => set({ fontSize }),
  setFontFamily: (fontFamily) => set({ fontFamily }),
  setOpacity: (opacity) => set({ opacity }),
}));
