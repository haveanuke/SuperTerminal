// Dev orchestrator: starts tsc --watch + vite, waits for vite to be reachable,
// then starts electron. Kills all children on electron exit or Ctrl-C.
import { spawn } from 'node:child_process';
import http from 'node:http';

const VITE_URL = 'http://localhost:5173';
const VITE_TIMEOUT_MS = 30_000;
const POLL_INTERVAL_MS = 200;

const children = [];

function start(label, cmd, args) {
  const proc = spawn(cmd, args, { stdio: 'inherit', shell: false });
  proc.on('exit', (code, signal) => {
    if (signal !== 'SIGTERM' && signal !== 'SIGINT') {
      console.error(`[dev] ${label} exited (code=${code}, signal=${signal})`);
    }
  });
  children.push(proc);
  return proc;
}

function killAll() {
  for (const c of children) {
    if (c.exitCode === null && c.signalCode === null) {
      try { c.kill('SIGTERM'); } catch { /* ignore */ }
    }
  }
}

function waitForVite() {
  const deadline = Date.now() + VITE_TIMEOUT_MS;
  return new Promise((resolve, reject) => {
    const tick = () => {
      const req = http.get(VITE_URL, (res) => {
        res.resume();
        if (res.statusCode && res.statusCode < 500) resolve();
        else retry();
      });
      req.on('error', retry);
      req.setTimeout(1000, () => { req.destroy(); retry(); });
    };
    const retry = () => {
      if (Date.now() > deadline) reject(new Error(`vite not reachable at ${VITE_URL} within ${VITE_TIMEOUT_MS}ms`));
      else setTimeout(tick, POLL_INTERVAL_MS);
    };
    tick();
  });
}

process.on('SIGINT', () => { killAll(); process.exit(130); });
process.on('SIGTERM', () => { killAll(); process.exit(143); });

start('tsc:watch', 'npx', ['tsc', '-p', 'tsconfig.main.json', '--watch', '--preserveWatchOutput']);
start('vite', 'npx', ['vite']);

try {
  await waitForVite();
} catch (err) {
  console.error(`[dev] ${err.message}`);
  killAll();
  process.exit(1);
}

const electron = start('electron', 'npx', ['electron', 'dist/main/index.js', '--dev']);
electron.on('exit', (code) => {
  killAll();
  process.exit(code ?? 0);
});
