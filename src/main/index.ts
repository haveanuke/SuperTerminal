import { app, BrowserWindow, ipcMain, shell } from 'electron';
import path from 'path';
import { PtyManager } from './pty-manager';
import { SessionManager } from './session-manager';

let mainWindow: BrowserWindow | null = null;
const ptyManager = new PtyManager();
const sessionManager = new SessionManager();

function createWindow() {
  mainWindow = new BrowserWindow({
    width: 1200,
    height: 800,
    minWidth: 600,
    minHeight: 400,
    titleBarStyle: 'hiddenInset',
    trafficLightPosition: { x: 12, y: 14 },
    backgroundColor: '#1a1b26',
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
      nodeIntegration: false,
      contextIsolation: true,
    },
  });

  if (process.env.NODE_ENV === 'development' || process.argv.includes('--dev')) {
    mainWindow.loadURL('http://localhost:5173');
  } else {
    mainWindow.loadFile(path.join(__dirname, '../renderer/index.html'));
  }

  mainWindow.on('closed', () => {
    mainWindow = null;
  });

  mainWindow.webContents.setWindowOpenHandler(({ url }) => {
    shell.openExternal(url);
    return { action: 'deny' };
  });
}

app.whenReady().then(createWindow);

app.on('window-all-closed', () => {
  ptyManager.disposeAll();
  if (process.platform !== 'darwin') app.quit();
});

app.on('activate', () => {
  if (BrowserWindow.getAllWindows().length === 0) createWindow();
});

// PTY IPC handlers
ipcMain.handle('pty:create', (_event, { id, cols, rows, cwd }) => {
  const existed = ptyManager.has(id);
  ptyManager.create(id, cols, rows, cwd);
  if (!existed) {
    ptyManager.onData(id, (data) => {
      mainWindow?.webContents.send(`pty:data:${id}`, data);
    });
    ptyManager.onExit(id, (exitCode) => {
      mainWindow?.webContents.send(`pty:exit:${id}`, exitCode);
    });
  }
  return true;
});

ipcMain.handle('pty:write', (_event, { id, data }) => {
  ptyManager.write(id, data);
});

ipcMain.handle('pty:resize', (_event, { id, cols, rows }) => {
  ptyManager.resize(id, cols, rows);
});

ipcMain.handle('pty:dispose', (_event, { id }) => {
  ptyManager.dispose(id);
});

// Session IPC handlers
ipcMain.handle('session:save', (_event, { name, layout }) => {
  return sessionManager.save(name, layout);
});

ipcMain.handle('session:load', (_event, { name }) => {
  return sessionManager.load(name);
});

ipcMain.handle('session:list', () => {
  return sessionManager.list();
});

ipcMain.handle('session:delete', (_event, { name }) => {
  return sessionManager.delete(name);
});
