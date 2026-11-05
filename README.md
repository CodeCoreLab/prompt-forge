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

打包产物在 `dist/` 目录。

### 便携版

```bash
npm run build:portable
```

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

### win-dpapi 安装失败

`win-dpapi` 需要 Python 和 C++ 编译工具链：

```bash
# 安装 windows-build-tools
npm install --global --production windows-build-tools

# 或使用 Visual Studio Build Tools
# 然后重装
npm install
```

如果仍失败，应用会自动回退到 AES-256-GCM 加密。

### GitHub Actions 编译

1. 推送代码到 `electron-migration` 分支
2. 访问 https://github.com/CodeCoreLab/prompt-forge/actions
3. 下载 `prompt-forge-cli-windows` artifact
4. 放到 `rust-core/target/release/` 目录

## License

MIT
