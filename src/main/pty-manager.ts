import * as pty from 'node-pty';
import os from 'os';
import { execSync } from 'child_process';

function getShellEnv(): Record<string, string> {
  if (os.platform() === 'win32') return process.env as Record<string, string>;
  const shell = process.env.SHELL || 'bash';
  try {
    const output = execSync(`${shell} -ilc 'env'`, {
      encoding: 'utf-8',
      timeout: 5000,
    });
    const env: Record<string, string> = {};
    for (const line of output.split('\n')) {
      const idx = line.indexOf('=');
      if (idx > 0) {
        env[line.substring(0, idx)] = line.substring(idx + 1);
      }
    }
    return env;
  } catch {
    return process.env as Record<string, string>;
  }
}

const shellEnv = getShellEnv();

interface PtyRecord {
  proc: pty.IPty;
  listenerDisposables: pty.IDisposable[];
}

export class PtyManager {
  private records = new Map<string, PtyRecord>();

  has(id: string): boolean {
    return this.records.has(id);
  }

  create(id: string, cols: number, rows: number, cwd?: string) {
    if (this.records.has(id)) return;
    const shell = process.env.SHELL || (os.platform() === 'win32' ? 'powershell.exe' : 'bash');
    const proc = pty.spawn(shell, [], {
      name: 'xterm-256color',
      cols,
      rows,
      cwd: cwd || os.homedir(),
      env: shellEnv,
    });
    this.records.set(id, { proc, listenerDisposables: [] });
  }

  write(id: string, data: string) {
    this.records.get(id)?.proc.write(data);
  }

  resize(id: string, cols: number, rows: number) {
    this.records.get(id)?.proc.resize(cols, rows);
  }

  onData(id: string, callback: (data: string) => void) {
    const rec = this.records.get(id);
    if (!rec) return;
    rec.listenerDisposables.push(rec.proc.onData(callback));
  }

  onExit(id: string, callback: (exitCode: number) => void) {
    const rec = this.records.get(id);
    if (!rec) return;
    rec.listenerDisposables.push(rec.proc.onExit(({ exitCode }) => callback(exitCode)));
  }

  dispose(id: string) {
    const rec = this.records.get(id);
    if (!rec) return;
    for (const d of rec.listenerDisposables) {
      try { d.dispose(); } catch { /* ignore */ }
    }
    rec.listenerDisposables.length = 0;
    rec.proc.kill();
    this.records.delete(id);
  }

  disposeAll() {
    for (const id of [...this.records.keys()]) {
      this.dispose(id);
    }
  }
}
