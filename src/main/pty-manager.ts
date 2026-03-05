import * as pty from 'node-pty';
import os from 'os';

export class PtyManager {
  private processes = new Map<string, pty.IPty>();

  create(id: string, cols: number, rows: number, cwd?: string) {
    const shell = process.env.SHELL || (os.platform() === 'win32' ? 'powershell.exe' : 'bash');
    const proc = pty.spawn(shell, [], {
      name: 'xterm-256color',
      cols,
      rows,
      cwd: cwd || os.homedir(),
      env: process.env as Record<string, string>,
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
