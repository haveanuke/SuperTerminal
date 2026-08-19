import { invoke, Channel, convertFileSrc } from '@tauri-apps/api/core';
import { open as openDialog } from '@tauri-apps/plugin-dialog';

type DataCb = (data: string) => void;
type ExitCb = (exitCode: number) => void;
type PtyFrame = ArrayBuffer | { exit: number };

const MAX_BUFFERED_CHARS = 1_000_000;

interface PtyEntry {
  channel: Channel<PtyFrame>;
  decoder: TextDecoder;
  dataSubs: Set<DataCb>;
  exitSubs: Set<ExitCb>;
  pending: string[]; // decoded text awaiting the first data subscriber
  pendingChars: number;
  latchedExit: number | null;
}

const entries = new Map<string, PtyEntry>();

function emitData(entry: PtyEntry, text: string) {
  if (text.length === 0) return;
  if (entry.dataSubs.size === 0) {
    entry.pending.push(text);
    entry.pendingChars += text.length;
    while (entry.pendingChars > MAX_BUFFERED_CHARS && entry.pending.length > 1) {
      const dropped = entry.pending.shift()!;
      entry.pendingChars -= dropped.length;
      console.warn('[tauri-bridge] pty buffer overflow, dropping oldest chunk');
    }
    return;
  }
  for (const cb of entry.dataSubs) cb(text);
}

function emitExit(entry: PtyEntry, code: number) {
  if (entry.exitSubs.size === 0) {
    entry.latchedExit = code;
    return;
  }
  for (const cb of entry.exitSubs) cb(code);
}

function makeEntry(id: string): PtyEntry {
  const entry: PtyEntry = {
    channel: new Channel<PtyFrame>(),
    decoder: new TextDecoder('utf-8'),
    dataSubs: new Set(),
    exitSubs: new Set(),
    pending: [],
    pendingChars: 0,
    latchedExit: null,
  };
  entry.channel.onmessage = (msg) => {
    if (msg instanceof ArrayBuffer) {
      emitData(entry, entry.decoder.decode(msg, { stream: true }));
    } else if (msg && typeof msg === 'object' && 'exit' in msg) {
      emitData(entry, entry.decoder.decode()); // flush the streaming decoder
      emitExit(entry, msg.exit);
    }
  };
  entries.set(id, entry);
  return entry;
}

export function installTauriBridge(): void {
  window.superTerminal = {
    pty: {
      create: async (id, cols, rows, cwd) => {
        if (entries.has(id)) return true;
        const entry = makeEntry(id);
        try {
          return await invoke<boolean>('pty_create', {
            id,
            cols,
            rows,
            cwd: cwd ?? null,
            channel: entry.channel,
          });
        } catch (err) {
          // Identity-guarded: if this id was disposed and recreated while our
          // invoke was in flight, never delete the replacement's entry.
          entry.channel.onmessage = () => {};
          if (entries.get(id) === entry) entries.delete(id);
          throw err instanceof Error ? err : new Error(String(err));
        }
      },
      write: (id, data) => invoke('pty_write', { id, data }),
      writeBroadcast: (ids, data) => invoke('pty_write_broadcast', { ids, data }),
      resize: (id, cols, rows) => invoke('pty_resize', { id, cols, rows }),
      dispose: async (id) => {
        const entry = entries.get(id);
        if (entry) {
          // The Rust side retains the channel until it drops — trailing frames
          // must never reach subscribers of a disposed terminal.
          entry.channel.onmessage = () => {};
          entry.dataSubs.clear();
          entry.exitSubs.clear();
          entry.pending.length = 0;
          entry.pendingChars = 0;
          entry.latchedExit = null;
          entries.delete(id);
        }
        await invoke('pty_dispose', { id });
      },
      onData: (id, callback) => {
        const entry = entries.get(id);
        if (!entry) return () => {};
        const flush = entry.dataSubs.size === 0 && entry.pending.length > 0;
        entry.dataSubs.add(callback);
        if (flush) {
          const chunks = entry.pending.splice(0);
          entry.pendingChars = 0;
          for (const chunk of chunks) callback(chunk);
        }
        return () => entry.dataSubs.delete(callback);
      },
      onExit: (id, callback) => {
        const entry = entries.get(id);
        if (!entry) return () => {};
        entry.exitSubs.add(callback);
        if (entry.latchedExit !== null) {
          const code = entry.latchedExit;
          entry.latchedExit = null;
          callback(code);
        }
        return () => entry.exitSubs.delete(callback);
      },
    },
    session: {
      save: (name, layout) => invoke('session_save', { name, layout }),
      load: async (name) => {
        try {
          return await invoke('session_load', { name });
        } catch (err) {
          throw err instanceof Error ? err : new Error(String(err));
        }
      },
      list: () => invoke('session_list'),
      delete: (name) => invoke('session_delete', { name }),
    },
    dialog: {
      openImage: async () => {
        const picked = await openDialog({
          multiple: false,
          filters: [{ name: 'Images', extensions: ['png', 'jpg', 'jpeg', 'webp', 'gif', 'bmp'] }],
        });
        if (typeof picked !== 'string') return null;
        const stored = await invoke<string>('store_background_image', { src: picked });
        return convertFileSrc(stored);
      },
    },
    buddy: {
      react: (req) => invoke('buddy_react', { req }),
    },
  };
}
