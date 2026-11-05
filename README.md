# Prompt Forge — Electron 重构版

桌面端提示词快速增强工具，从 **Tauri 2** 迁移到 **Electron 33** 架构，彻底消除 **WebView2 Runtime** 依赖，实现开箱即用。

## 功能

- 紧凑主窗口：输入框 + 输出框，一键增强、一键复制
- 快速增强：单轮 LLM 调用直接输出，不做多步推理
- 增强模式：目标拆解 / 结构化角色 / 全局增强，架构可扩展
- 多配置 LLM 管理：兼容 OpenAI 兼容、Anthropic (Claude)、Ollama、Gemini 协议
- 系统集成：托盘常驻、最小化到托盘、开机自启，均可在设置中开关
- 历史记录：最近 20 条增强一键回填
- 取消增强：增强过程中可随时取消，不必干等超时
- API Key 加密：Windows DPAPI（当前用户作用域）加密落盘

## 架构

```
┌─────────────────┐     stdin/stdout JSON      ┌──────────────────┐
│   Electron      │ ────────────────────────▶ │  prompt-forge-   │
│   主进程         │ ◀──────────────────────── │  cli (Rust)      │
│   (Node.js)     │      JSON Response        │                  │
└─────────────────┘                            └──────────────────┘
        │                                                │
        │ IPC                                            │ HTTP
        ▼                                                ▼
┌─────────────────┐                            ┌──────────────────┐
│   渲染进程       │                            │   LLM 服务商      │
│   (HTML/CSS/JS) │                            │   (OpenAI/等)     │
└─────────────────┘                            └──────────────────┘
```

## 开发

### 环境要求

- **Node.js** >= 18
- **Rust** >= 1.75
- **Windows 10/11 x64**

### 安装依赖

```bash
npm install
```

### 构建 Rust CLI

```bash
cd rust-core
cargo build --release
```

编译产物：`rust-core/target/release/prompt-forge-cli.exe`

### 开发模式运行

```bash
npm run dev
```

## 构建

### NSIS 安装版

```bash
npm run build
```

打包产物在 `dist/` 目录：`Prompt Forge Setup 1.0.0.exe`（安装包）、`Prompt Forge 1.0.0.exe`（便携版）。

### 便携版

```bash
npm run build:portable
```

### 直接下载已编译产物

推荐走 GitHub Actions（CI 已配置，无需本地 Rust / NSIS 工具链）：

| 产物 | 说明 |
|------|------|
| `Prompt.Forge.Setup.1.0.0.exe` | NSIS 安装包（82 MB） |
| `Prompt.Forge.1.0.0.exe` | 便携版（75 MB） |
| `Prompt.Forge.Setup.1.0.0.exe.blockmap` | 差分更新元数据 |

下载入口：<https://github.com/CodeCoreLab/prompt-forge/releases>

Rust CLI 单独产物（`prompt-forge-cli-windows` artifact）可从
<https://github.com/CodeCoreLab/prompt-forge/actions> 获取。

## 配置

- Windows: `%APPDATA%\prompt-forge\config.json`

API Key 存储于本地配置文件，不上传。

## 故障排除

### Rust 编译失败

如果 `cargo build` 失败，提示 `ring` crate 需要 C 编译器：

1. 安装 [Visual Studio Build Tools](https://visualstudio.microsoft.com/zh-hans/downloads/?q=build+tools)
2. 或安装 [MinGW-w64](https://www.mingw-w64.org/)
3. 确保 `gcc` 或 `cl.exe` 在 PATH 中

**推荐方案**：直接使用 GitHub Actions 编译，下载 artifact 中的 `prompt-forge-cli.exe`。

### Electron 运行失败

- 确保 Rust CLI 已编译：`cd rust-core && cargo build --release`
- 检查 `prompt-forge-cli.exe` 是否存在
- 开发模式下，确保 `node_modules` 已安装：`npm install`

### 在受限环境启动 Electron

若 Electron 以纯 Node 模式启动（报 `Cannot read properties of undefined (reading 'whenReady')`），
通常是环境变量 `ELECTRON_RUN_AS_NODE=1` 或 `NODE_OPTIONS` 指向了其他 Node 注入脚本：

```bash
env -u ELECTRON_RUN_AS_NODE -u NODE_OPTIONS ./node_modules/electron/dist/electron.exe .
```

### DPAPI 加密

API Key 的加密完全由 Rust 侧 `prompt-forge-core::crypto` 承担（`libloading` 直调 `crypt32.dll`），
Node 侧无原生模块依赖，因此**不需要** `node-gyp` / Visual Studio Build Tools。

- 落盘格式：`enc:<base64>`（DPAPI 当前用户作用域加密）
- 读取时自动解密为明文交给前端，旧版明文配置自动兼容

### GitHub Actions 编译

推荐路径（无需本地 Rust / NSIS 工具链）：

1. 推送代码到 `electron-migration` 分支
2. 访问 https://github.com/CodeCoreLab/prompt-forge/actions
3. CI 依次执行：`Rust CLI (Windows)` → `Electron App (Windows)` → `Publish Release`
4. 从 https://github.com/CodeCoreLab/prompt-forge/releases 下载安装包/便携版

单独只需 CLI 二进制时，可下载 `prompt-forge-cli-windows` artifact，
解压后放到 `rust-core/target/release/` 目录。

CI 已在 `.github/workflows/build.yml` 中配置 `--publish never` + 独立 Release job，
避免在无 `GH_TOKEN` 时因自动发布而中断构建。

### 本地 NSIS 打包报符号链接错误

electron-builder 打包时会解压 `winCodeSign` 工具包，其中包含 macOS 符号链接。
Windows 未开启「开发者模式」时无法创建符号链接，会导致解压报错（Darwin 相关文件与 Windows 打包无关）。

解决方案（二选一）：

1. 开启 Windows 开发者模式（设置 → 系统 → 开发者选项 → 开发人员模式）
2. 优先使用 GitHub Actions 编译（CI runner 无此限制）

### Node.js 版本告警

CI 中 `actions/checkout@v4` / `actions/upload-artifact@v4` 提示 Node.js 20 已弃用，
属于 GitHub 平台侧提示，不影响构建产物。

## License

MIT
