const { app, BrowserWindow, Tray, Menu, ipcMain, nativeImage, clipboard } = require('electron');
const path = require('path');
const { spawn } = require('child_process');
const { ConfigManager } = require('./config');

// ── 全局状态 ──
let mainWindow = null;
let tray = null;
let rustProcess = null;
let configManager = null;

// ── Rust CLI 路径 ──
function getCliPath() {
  const devPath = path.join(__dirname, '../../rust-core/target/release/prompt-forge-cli.exe');
  const prodPath = path.join(process.resourcesPath, 'rust-core/prompt-forge-cli.exe');
  if (require('fs').existsSync(devPath)) {
    return devPath;
  }
  return prodPath;
}

// ── 启动 Rust CLI ──
function startRustCli() {
  const cliPath = getCliPath();
  rustProcess = spawn(cliPath, [], {
    stdio: ['pipe', 'pipe', 'inherit'],
  });

  rustProcess.stdout.on('data', (data) => {
    const lines = data.toString().split('\n').filter(l => l.trim());
    for (const line of lines) {
      try {
        const msg = JSON.parse(line);
        handleCliMessage(msg);
      } catch (e) {
        console.error('Failed to parse CLI message:', line);
      }
    }
  });

  rustProcess.stderr.on('data', (data) => {
    console.error('CLI stderr:', data.toString());
  });

  rustProcess.on('exit', (code) => {
    console.log(`CLI exited with code ${code}`);
    rustProcess = null;
  });
}

// ── 处理 CLI 消息 ──
const pendingRequests = new Map();
let requestId = 0;

function handleCliMessage(msg) {
  if (pendingRequests.has(msg.id)) {
    const { resolve, reject } = pendingRequests.get(msg.id);
    pendingRequests.delete(msg.id);
    if (msg.success) {
      resolve(msg.data);
    } else {
      reject(new Error(msg.error || 'CLI error'));
    }
  }
}

function sendToCli(action, payload = {}) {
  return new Promise((resolve, reject) => {
    const id = (++requestId).toString();
    pendingRequests.set(id, { resolve, reject });
    const req = { id, action, ...payload };
    rustProcess.stdin.write(JSON.stringify(req) + '\n');
  });
}

// ── 创建窗口 ──
function createWindow() {
  mainWindow = new BrowserWindow({
    width: 560,
    height: 620,
    minWidth: 460,
    minHeight: 480,
    center: true,
    resizable: true,
    fullscreen: false,
    icon: path.join(__dirname, '../assets/icon.png'),
    webPreferences: {
      preload: path.join(__dirname, '../preload/index.js'),
      contextIsolation: true,
      nodeIntegration: false,
    },
  });

  mainWindow.loadFile(path.join(__dirname, '../renderer/index.html'));

  mainWindow.on('minimize', (event) => {
    const settings = configManager?.getConfig()?.settings;
    if (settings?.minimizeToTray) {
      event.preventDefault();
      mainWindow.hide();
    }
  });

  mainWindow.on('close', (event) => {
    const settings = configManager?.getConfig()?.settings;
    if (settings?.minimizeToTray && !app.isQuitting) {
      event.preventDefault();
      mainWindow.hide();
    }
  });
}

// ── 创建托盘 ──
function createTray() {
  const iconPath = path.join(__dirname, '../assets/icon.png');
  const trayIcon = nativeImage.createFromPath(iconPath);
  tray = new Tray(trayIcon);

  const contextMenu = Menu.buildFromTemplate([
    {
      label: '显示主窗口',
      click: () => {
        mainWindow.show();
        mainWindow.focus();
      },
    },
    {
      label: '退出',
      click: () => {
        app.isQuitting = true;
        app.quit();
      },
    },
  ]);

  tray.setToolTip('Prompt Forge 提示词增强');
  tray.setContextMenu(contextMenu);
  tray.on('click', () => {
    mainWindow.show();
    mainWindow.focus();
  });
}

// ── IPC 处理 ──
function setupIpc() {
  // 增强
  ipcMain.handle('enhance', async (event, text, mode) => {
    try {
      const result = await sendToCli('enhance', { text, mode });
      return result;
    } catch (e) {
      throw e;
    }
  });

  // 模式列表
  ipcMain.handle('getModes', async () => {
    const result = await sendToCli('get_modes');
    return result;
  });

  // 配置读取
  ipcMain.handle('getConfig', async () => {
    return configManager.getConfig();
  });

  // 配置保存
  ipcMain.handle('saveConfig', async (event, config) => {
    configManager.save(config);
    return configManager.getConfig();
  });

  // 模型列表
  ipcMain.handle('listModels', async () => {
    const config = configManager.getConfig();
    return config.profiles || [];
  });

  // 添加模型
  ipcMain.handle('addModel', async (event, model) => {
    configManager.addModel(model);
    return configManager.getConfig().profiles;
  });

  // 删除模型
  ipcMain.handle('removeModel', async (event, id) => {
    configManager.removeModel(id);
    return configManager.getConfig().profiles;
  });

  // 历史记录
  ipcMain.handle('getHistory', async () => {
    const result = await sendToCli('get_history');
    return result;
  });

  // 复制到剪贴板
  ipcMain.handle('copyToClipboard', async (event, text) => {
    clipboard.writeText(text);
    return undefined;
  });

  // 取消增强
  ipcMain.handle('cancelEnhance', async () => {
    // TODO: 实现取消逻辑
    return undefined;
  });

  // 测试连接
  ipcMain.handle('testConnection', async (event, profile) => {
    try {
      const result = await sendToCli('test_llm', { profile });
      return result;
    } catch (e) {
      throw e;
    }
  });

  // 开机自启
  ipcMain.handle('setAutostart', async (event, enabled) => {
    app.setLoginItemSettings({
      openAtLogin: enabled,
      openAsHidden: true,
    });
    const config = configManager.getConfig();
    config.autostart = enabled;
    configManager.save(config);
    return enabled;
  });
}

// ── 应用生命周期 ──
app.whenReady().then(() => {
  configManager = new ConfigManager();
  startRustCli();
  createWindow();
  createTray();
  setupIpc();

  // 恢复自启设置
  const config = configManager.getConfig();
  app.setLoginItemSettings({
    openAtLogin: config.autostart,
    openAsHidden: true,
  });

  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) {
      createWindow();
    } else {
      mainWindow.show();
    }
  });
});

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') {
    app.quit();
  }
});

app.on('before-quit', () => {
  app.isQuitting = true;
  if (rustProcess) {
    rustProcess.kill();
  }
});
