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

export class PtyManager {
  private processes = new Map<string, pty.IPty>();

  has(id: string): boolean {
    return this.processes.has(id);
  }

  create(id: string, cols: number, rows: number, cwd?: string) {
    if (this.processes.has(id)) return;
    const shell = process.env.SHELL || (os.platform() === 'win32' ? 'powershell.exe' : 'bash');
    const proc = pty.spawn(shell, [], {
      name: 'xterm-256color',
      cols,
      rows,
      cwd: cwd || os.homedir(),
      env: shellEnv,
    });
    this.processes.set(id, proc);
  }

  write(id: string, data: string) {
    this.processes.get(id)?.write(data);
  }

  resize(id: string, cols: number, rows: number) {
    this.processes.get(id)?.resize(cols, rows);
  }

  onData(id: string, callback: (data: string) => void) {
    this.processes.get(id)?.onData(callback);
  }

  onExit(id: string, callback: (exitCode: number) => void) {
    this.processes.get(id)?.onExit(({ exitCode }) => callback(exitCode));
  }

  dispose(id: string) {
    const proc = this.processes.get(id);
    if (proc) {
      proc.kill();
      this.processes.delete(id);
    }
  }

  disposeAll() {
    for (const [id] of this.processes) {
      this.dispose(id);
    }
  }
}
