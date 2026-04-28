import { spawn, spawnSync } from 'child_process';

export interface BuddyAgentRequest {
  command: string;
  args: string[]; // {prompt} placeholder substituted with prompt
  prompt: string;
  timeoutMs?: number;
}

export interface BuddyAgentResult {
  ok: boolean;
  text: string;
  error?: string;
}

const ANSI_RE = /\x1b\[[0-9;?]*[a-zA-Z]/g;

function stripAnsi(s: string): string {
  return s.replace(ANSI_RE, '');
}

// Sanity bounds against malformed IPC payloads — not a security boundary.
// The renderer already has arbitrary local exec via pty:write, so locking down
// the binary here would be theater. Args are spawned with shell:false so
// metacharacters in args can't escape into the shell.
const MAX_ARGS = 16;
const MAX_ARG_LEN = 4096;

/**
 * Electron on macOS/Linux doesn't inherit the user's shell PATH, so CLIs installed
 * via homebrew/pyenv/fnm/etc. aren't findable. Ask the login shell for its $PATH once
 * and cache it.
 */
let cachedShellPath: string | null = null;

function getShellPath(): string {
  if (cachedShellPath !== null) return cachedShellPath;
  if (process.platform === 'win32') {
    cachedShellPath = process.env.PATH ?? '';
    return cachedShellPath;
  }
  const shell = process.env.SHELL || '/bin/zsh';
  try {
    const result = spawnSync(shell, ['-ilc', 'echo -n "$PATH"'], {
      encoding: 'utf8',
      timeout: 3000,
    });
    if (result.status === 0 && result.stdout) {
      cachedShellPath = result.stdout.trim();
      return cachedShellPath;
    }
  } catch {
    // ignore
  }
  const fallback = [
    process.env.PATH,
    '/opt/homebrew/bin',
    '/usr/local/bin',
    '/usr/bin',
    '/bin',
    `${process.env.HOME}/.local/bin`,
  ].filter(Boolean).join(':');
  cachedShellPath = fallback;
  return cachedShellPath;
}

/**
 * Run the configured agent CLI once with the user's prompt substituted into args.
 * Returns first non-empty line/chunk of stdout as the reaction text.
 * Keeps stderr out of the response, falls back gracefully on timeout/exit.
 */
export function runBuddyAgent(req: BuddyAgentRequest): Promise<BuddyAgentResult> {
  const timeoutMs = req.timeoutMs ?? 25_000;

  return new Promise((resolve) => {
    if (typeof req.command !== 'string' || !req.command.trim()) {
      resolve({ ok: false, text: '', error: 'empty command' });
      return;
    }
    if (
      !Array.isArray(req.args)
      || req.args.length > MAX_ARGS
      || req.args.some((a) => typeof a !== 'string' || a.length > MAX_ARG_LEN)
    ) {
      resolve({ ok: false, text: '', error: 'invalid args' });
      return;
    }
    if (typeof req.prompt !== 'string') {
      resolve({ ok: false, text: '', error: 'invalid prompt' });
      return;
    }

    const substitutedArgs = req.args.map((a) => a.replaceAll('{prompt}', req.prompt));

    let child;
    try {
      child = spawn(req.command, substitutedArgs, {
        stdio: ['ignore', 'pipe', 'pipe'],
        shell: false,
        env: { ...process.env, PATH: getShellPath(), NO_COLOR: '1', FORCE_COLOR: '0' },
      });
    } catch (err) {
      resolve({ ok: false, text: '', error: String(err) });
      return;
    }

    let stdout = '';
    let stderr = '';
    let settled = false;

    const finish = (result: BuddyAgentResult) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      try { child.kill(); } catch { /* ignore */ }
      resolve(result);
    };

    const timer = setTimeout(() => {
      finish({ ok: false, text: '', error: 'timeout' });
    }, timeoutMs);

    child.stdout?.on('data', (chunk: Buffer) => {
      stdout += chunk.toString('utf8');
    });
    child.stderr?.on('data', (chunk: Buffer) => {
      stderr += chunk.toString('utf8');
    });

    child.on('error', (err) => {
      finish({ ok: false, text: '', error: err.message });
    });

    child.on('close', (code) => {
      const cleaned = stripAnsi(stdout).trim();
      if (code === 0 && cleaned) {
        // Take the first meaningful line/paragraph, cap length
        const firstBlock = cleaned.split(/\n\s*\n/)[0].replace(/\s+/g, ' ').trim();
        const capped = firstBlock.length > 280 ? firstBlock.slice(0, 277) + '...' : firstBlock;
        finish({ ok: true, text: capped });
      } else {
        finish({
          ok: false,
          text: '',
          error: `exit ${code}: ${stripAnsi(stderr).slice(0, 200)}`,
        });
      }
    });
  });
}
