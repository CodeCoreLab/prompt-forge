const { app, BrowserWindow, Tray, Menu, ipcMain, nativeImage, clipboard } = require('electron');
const path = require('path');
const fs = require('fs');
const { spawn } = require('child_process');
const { ConfigManager } = require('./config');

// ── 全局状态 ──
let mainWindow = null;
let tray = null;
let rustProcess = null;
let configManager = null;
const pendingRequests = new Map();
let requestId = 0;
let isQuitting = false;

// ── Rust CLI 路径 ──
// 开发态：仓库内 rust-core/target/release；打包态：resources/rust-core（extraResources）
function getCliPath() {
  const devPath = path.join(__dirname, '../../rust-core/target/release/prompt-forge-cli.exe');
  const prodPath = path.join(process.resourcesPath, 'rust-core', 'prompt-forge-cli.exe');
  if (fs.existsSync(devPath)) return devPath;
  return prodPath;
}

// ── 启动 Rust CLI ──
function startRustCli() {
  const cliPath = getCliPath();
  if (!fs.existsSync(cliPath)) {
    console.error(`Rust CLI 不存在: ${cliPath}`);
    return;
  }
  console.log(`Starting Rust CLI: ${cliPath}`);

  rustProcess = spawn(cliPath, [], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });

  rustProcess.stdout.on('data', (data) => {
    const lines = data.toString().split('\n').filter((l) => l.trim());
    for (const line of lines) {
      try {
        handleCliMessage(JSON.parse(line));
      } catch (e) {
        console.error('Failed to parse CLI message:', line);
      }
    }
  });

  rustProcess.stderr.on('data', (data) => console.error('CLI stderr:', data.toString()));

  rustProcess.on('exit', (code, signal) => {
    console.log(`CLI exited with code ${code}, signal ${signal}`);
    rustProcess = null;
    // 意外退出自动重启（主动退出时不重启）
    if (!isQuitting) {
      setTimeout(startRustCli, 500);
    }
  });

  rustProcess.on('error', (err) => console.error('CLI spawn error:', err));
}

// ── 处理 CLI 消息 ──
function handleCliMessage(msg) {
  const pending = pendingRequests.get(msg.id);
  if (!pending) return;
  pendingRequests.delete(msg.id);
  if (msg.success) {
    pending.resolve(msg.data);
  } else {
    pending.reject(new Error(msg.error || 'CLI error'));
  }
}

// ── 发送请求到 CLI ──
function sendToCli(action, payload = {}) {
  return new Promise((resolve, reject) => {
    if (!rustProcess || rustProcess.killed) {
      reject(new Error('CLI not running'));
      return;
    }
    const id = (++requestId).toString();
    pendingRequests.set(id, { resolve, reject });
    try {
      rustProcess.stdin.write(JSON.stringify({ id, action, ...payload }) + '\n');
    } catch (e) {
      pendingRequests.delete(id);
      reject(new Error('Failed to write to CLI: ' + e.message));
    }
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
    if (configManager?.current()?.close_to_tray) {
      event.preventDefault();
      mainWindow.hide();
    }
  });

  mainWindow.on('close', (event) => {
    if (configManager?.current()?.close_to_tray && !isQuitting) {
      event.preventDefault();
      mainWindow.hide();
    }
  });
}

// ── 创建托盘 ──
function createTray() {
  const trayIcon = nativeImage.createFromPath(path.join(__dirname, '../assets/icon.png'));
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
        isQuitting = true;
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
  // 增强（Rust 侧返回增强后的纯文本）
  ipcMain.handle('enhance', async (event, text, mode) => {
    return sendToCli('enhance', { text, mode });
  });

  // 取消增强（Rust 侧 CancellationToken，进程不重启）
  ipcMain.handle('cancelEnhance', async () => {
    await sendToCli('cancel');
    return undefined;
  });

  // 模式列表
  ipcMain.handle('getModes', async () => sendToCli('get_modes'));

  // 配置读取（key 由 Rust 侧解密为明文供前端编辑）
  ipcMain.handle('getConfig', async () => configManager.getConfig());

  // 配置保存（Rust 侧加密 key + 校验 + 原子写）
  ipcMain.handle('saveConfig', async (event, config) => configManager.save(config));

  // 历史记录
  ipcMain.handle('getHistory', async () => sendToCli('get_history'));

  // 复制到剪贴板（Electron 原生，跨平台）
  ipcMain.handle('copyToClipboard', async (event, text) => {
    clipboard.writeText(text);
    return undefined;
  });

  // 测试连接
  ipcMain.handle('testConnection', async (event, profile) => {
    return sendToCli('test_llm', profile);
  });

  // 开机自启
  ipcMain.handle('setAutostart', async (event, enabled) => {
    app.setLoginItemSettings({ openAtLogin: enabled, openAsHidden: true });
    const config = await configManager.getConfig();
    config.autostart = enabled;
    await configManager.save(config);
    return enabled;
  });
}

// ── 应用生命周期 ──
app.whenReady().then(async () => {
  configManager = new ConfigManager(sendToCli);
  startRustCli();
  createWindow();
  createTray();
  setupIpc();

  // 恢复自启设置（配置读取失败时不阻断启动）
  try {
    const config = await configManager.getConfig();
    app.setLoginItemSettings({ openAtLogin: !!config.autostart, openAsHidden: true });
  } catch (e) {
    console.error('读取配置失败:', e);
  }

  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) {
      createWindow();
    } else {
      mainWindow.show();
    }
  });
});

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') app.quit();
});

app.on('before-quit', () => {
  isQuitting = true;
  if (rustProcess) {
    rustProcess.kill();
  }
});
