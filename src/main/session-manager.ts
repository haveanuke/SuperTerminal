import fs from 'fs';
import path from 'path';
import { app } from 'electron';

interface SessionPaneTerminal {
  type: 'terminal';
  terminalId: string;
}

interface SessionPaneSplit {
  type: 'split';
  direction: 'horizontal' | 'vertical';
  children: [SessionPane, SessionPane];
  sizes?: [number, number];
}

type SessionPane = SessionPaneTerminal | SessionPaneSplit;

interface SessionTab {
  id: string;
  label: string;
  pane: SessionPane;
}

interface SessionLayout {
  tabs: SessionTab[];
  activeTabId: string;
}

interface SessionData {
  name: string;
  layout: SessionLayout;
  savedAt: string;
}

const DANGEROUS_KEYS = new Set(['__proto__', 'constructor', 'prototype']);

function isPlainObject(v: unknown): v is Record<string, unknown> {
  if (typeof v !== 'object' || v === null) return false;
  const proto = Object.getPrototypeOf(v);
  return proto === Object.prototype || proto === null;
}

function hasNoDangerousKeys(v: unknown): boolean {
  if (Array.isArray(v)) return v.every(hasNoDangerousKeys);
  if (!isPlainObject(v)) return true;
  for (const key of Object.keys(v)) {
    if (DANGEROUS_KEYS.has(key)) return false;
    if (!hasNoDangerousKeys(v[key])) return false;
  }
  return true;
}

function validatePane(v: unknown): v is SessionPane {
  if (!isPlainObject(v)) return false;
  if (v.type === 'terminal') {
    return typeof v.terminalId === 'string';
  }
  if (v.type === 'split') {
    if (v.direction !== 'horizontal' && v.direction !== 'vertical') return false;
    if (!Array.isArray(v.children) || v.children.length !== 2) return false;
    if (!validatePane(v.children[0]) || !validatePane(v.children[1])) return false;
    if (v.sizes !== undefined) {
      if (!Array.isArray(v.sizes) || v.sizes.length !== 2) return false;
      if (typeof v.sizes[0] !== 'number' || typeof v.sizes[1] !== 'number') return false;
    }
    return true;
  }
  return false;
}

function validateTab(v: unknown): v is SessionTab {
  return isPlainObject(v)
    && typeof v.id === 'string'
    && typeof v.label === 'string'
    && validatePane(v.pane);
}

function validateLayout(v: unknown): v is SessionLayout {
  if (!isPlainObject(v)) return false;
  if (typeof v.activeTabId !== 'string') return false;
  if (!Array.isArray(v.tabs)) return false;
  return v.tabs.every(validateTab);
}

function validateSessionData(v: unknown): v is SessionData {
  return isPlainObject(v)
    && typeof v.name === 'string'
    && typeof v.savedAt === 'string'
    && validateLayout(v.layout)
    && hasNoDangerousKeys(v);
}

export class SessionLoadError extends Error {
  constructor(public reason: 'invalid_json' | 'schema_mismatch', message: string) {
    super(message);
    this.name = 'SessionLoadError';
  }
}

export class SessionManager {
  private sessionsDir: string;

  constructor(sessionsDir?: string) {
    this.sessionsDir = sessionsDir ?? path.join(app.getPath('userData'), 'sessions');
    if (!fs.existsSync(this.sessionsDir)) {
      fs.mkdirSync(this.sessionsDir, { recursive: true });
    }
  }

  save(name: string, layout: unknown): boolean {
    const safeName = name.replace(/[^a-zA-Z0-9_-]/g, '_');
    const data = { name, layout, savedAt: new Date().toISOString() };
    fs.writeFileSync(
      path.join(this.sessionsDir, `${safeName}.json`),
      JSON.stringify(data, null, 2)
    );
    return true;
  }

  load(name: string): SessionData | null {
    const safeName = name.replace(/[^a-zA-Z0-9_-]/g, '_');
    const filePath = path.join(this.sessionsDir, `${safeName}.json`);
    if (!fs.existsSync(filePath)) return null;
    const raw = fs.readFileSync(filePath, 'utf-8');
    let parsed: unknown;
    try {
      parsed = JSON.parse(raw);
    } catch {
      throw new SessionLoadError('invalid_json', `session "${name}" is corrupted (invalid JSON)`);
    }
    if (!validateSessionData(parsed)) {
      throw new SessionLoadError('schema_mismatch', `session "${name}" is corrupted (schema mismatch)`);
    }
    return parsed;
  }

  list(): string[] {
    return fs.readdirSync(this.sessionsDir)
      .filter((f) => f.endsWith('.json'))
      .map((f) => f.replace('.json', ''));
  }

  delete(name: string): boolean {
    const safeName = name.replace(/[^a-zA-Z0-9_-]/g, '_');
    const filePath = path.join(this.sessionsDir, `${safeName}.json`);
    if (fs.existsSync(filePath)) {
      fs.unlinkSync(filePath);
      return true;
    }
    return false;
  }
}
