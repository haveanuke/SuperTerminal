export interface AutoRunConfig {
  command: string;
  intervalSeconds: number;
  enabled: boolean;
  sendEscape?: boolean;
  escapeDelaySecs?: number;
}

export interface TerminalInstance {
  id: string;
  title: string;
  cwd?: string;
  autoRun?: AutoRunConfig;
}

export type SplitDirection = 'horizontal' | 'vertical';

export type PaneNode =
  | { type: 'terminal'; terminalId: string }
  | { type: 'split'; direction: SplitDirection; children: [PaneNode, PaneNode]; sizes?: [number, number] };


export interface Tab {
  id: string;
  label: string;
  pane: PaneNode;
}

export interface ThemeConfig {
  name: string;
  background: string;
  foreground: string;
  cursor: string;
  selection: string;
  black: string;
  red: string;
  green: string;
  yellow: string;
  blue: string;
  magenta: string;
  cyan: string;
  white: string;
  brightBlack: string;
  brightRed: string;
  brightGreen: string;
  brightYellow: string;
  brightBlue: string;
  brightMagenta: string;
  brightCyan: string;
  brightWhite: string;
  uiBackground: string;
  uiSurface: string;
  uiBorder: string;
  uiAccent: string;
  uiText: string;
  uiTextMuted: string;
}

declare global {
  interface Window {
    superTerminal: {
      pty: {
        create: (id: string, cols: number, rows: number, cwd?: string) => Promise<boolean>;
        write: (id: string, data: string) => Promise<void>;
        resize: (id: string, cols: number, rows: number) => Promise<void>;
        dispose: (id: string) => Promise<void>;
        onData: (id: string, callback: (data: string) => void) => () => void;
        onExit: (id: string, callback: (exitCode: number) => void) => () => void;
      };
      session: {
        save: (name: string, layout: unknown) => Promise<boolean>;
        load: (name: string) => Promise<{ name: string; layout: unknown; savedAt: string } | null>;
        list: () => Promise<string[]>;
        delete: (name: string) => Promise<boolean>;
      };
      dialog: {
        openImage: () => Promise<string | null>;
      };
      buddy: {
        react: (req: { command: string; args: string[]; prompt: string; timeoutMs?: number }) =>
          Promise<{ ok: boolean; text: string; error?: string }>;
      };
    };
  }
}
