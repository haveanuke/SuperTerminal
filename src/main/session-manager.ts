import fs from 'fs';
import path from 'path';
import { app } from 'electron';

interface SessionData {
  name: string;
  layout: unknown;
  savedAt: string;
}

export class SessionManager {
  private sessionsDir: string;

  constructor() {
    this.sessionsDir = path.join(app.getPath('userData'), 'sessions');
    if (!fs.existsSync(this.sessionsDir)) {
      fs.mkdirSync(this.sessionsDir, { recursive: true });
    }
  }

  save(name: string, layout: unknown): boolean {
    const safeName = name.replace(/[^a-zA-Z0-9_-]/g, '_');
    const data: SessionData = { name, layout, savedAt: new Date().toISOString() };
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
    return JSON.parse(fs.readFileSync(filePath, 'utf-8'));
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
