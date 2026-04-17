import { contextBridge, ipcRenderer } from 'electron';

contextBridge.exposeInMainWorld('superTerminal', {
  pty: {
    create: (id: string, cols: number, rows: number, cwd?: string) =>
      ipcRenderer.invoke('pty:create', { id, cols, rows, cwd }),
    write: (id: string, data: string) =>
      ipcRenderer.invoke('pty:write', { id, data }),
    resize: (id: string, cols: number, rows: number) =>
      ipcRenderer.invoke('pty:resize', { id, cols, rows }),
    dispose: (id: string) =>
      ipcRenderer.invoke('pty:dispose', { id }),
    onData: (id: string, callback: (data: string) => void) => {
      const handler = (_event: Electron.IpcRendererEvent, data: string) => callback(data);
      ipcRenderer.on(`pty:data:${id}`, handler);
      return () => ipcRenderer.removeListener(`pty:data:${id}`, handler);
    },
    onExit: (id: string, callback: (exitCode: number) => void) => {
      const handler = (_event: Electron.IpcRendererEvent, exitCode: number) => callback(exitCode);
      ipcRenderer.on(`pty:exit:${id}`, handler);
      return () => ipcRenderer.removeListener(`pty:exit:${id}`, handler);
    },
  },
  session: {
    save: (name: string, layout: unknown) =>
      ipcRenderer.invoke('session:save', { name, layout }),
    load: (name: string) =>
      ipcRenderer.invoke('session:load', { name }),
    list: () => ipcRenderer.invoke('session:list'),
    delete: (name: string) =>
      ipcRenderer.invoke('session:delete', { name }),
  },
  dialog: {
    openImage: () => ipcRenderer.invoke('dialog:openImage'),
  },
  buddy: {
    react: (req: { command: string; args: string[]; prompt: string; timeoutMs?: number }) =>
      ipcRenderer.invoke('buddy:react', req),
  },
});
