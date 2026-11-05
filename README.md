# Prompt Forge — Electron 重构版

桌面提示词快速增强工具，从 **Tauri 2** 迁移到 **Electron 33** 架构，彻底消除 **WebView2 Runtime** 依赖，实现开箱即用。

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

## 项目结构

```
prompt-forge-electron/
├── src/
│   ├── main/           # Electron 主进程
│   │   ├── index.js   # 主入口，窗口/托盘/IPC
│   │   ├── config.js  # 配置管理（Node.js）
│   │   └── crypto.js  # DPAPI 加密（占位）
│   ├── preload/       # 预加载脚本（安全 IPC 桥接）
│   │   └── index.js
│   └── renderer/      # 前端（复用原始 UI）
│       ├── index.html
│       ├── style.css
│       └── app.js     # 替换 Tauri API 为 Electron IPC
├── rust-core/         # Rust 核心（保留 prompt-forge-core 测试）
│   ├── Cargo.toml     # 工作空间
│   └── crates/
│       ├── prompt-forge-core/   # 纯逻辑层（零 Tauri 依赖）
│       └── prompt-forge-cli/    # CLI 二进制（stdin/stdout JSON）
└── package.json
```

## 快速开始

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

### 打包（NSIS 安装版）

```bash
npm run build
```

打包产物在 `dist/` 目录。

### 打包（便携版）

```bash
npm run build:portable
```

## 核心特性

- ✅ **零 WebView2 依赖**（Chromium 内嵌）
- ✅ **Rust 核心逻辑保留**（prompt-forge-core 独立测试）
- ✅ **四种 LLM 协议**（OpenAI / Anthropic / Ollama / Gemini）
- ✅ **三种内置增强模式** + 自定义模式（extend/replace）
- ✅ **输出自动规整**（think 块 / 围栏 / 引号）
- ✅ **429/5xx 指数退避重试**
- ✅ **托盘常驻** + 最小化到托盘
- ✅ **开机自启**
- ✅ **历史记录**（最近 20 条）
- ✅ **API Key 加密存储**

## 与 Tauri 版的区别

| 项目 | Tauri 2 | Electron |
|------|---------|----------|
| WebView 依赖 | WebView2 Runtime | Chromium 内嵌 |
| 体积 | ~5MB（不含运行时） | ~100MB |
| 前端 | 原生 HTML/JS | 原生 HTML/JS（不变） |
| 后端 | Rust + Tauri | Rust CLI + Node.js |
| 打包 | NSIS | NSIS / 便携版 |
| 长期维护 | Tauri 2 迭代风险 | Electron 生态成熟 |

## 技术栈

- **前端**: 原生 HTML/CSS/JavaScript（零框架）
- **桌面框架**: Electron 33
- **后端核心**: Rust (tokio / reqwest / serde)
- **通信**: stdin/stdout JSON + Electron IPC
- **打包**: electron-builder

## 故障排除

### Rust 编译失败

如果 `cargo build` 失败，提示 `ring` crate 需要 C 编译器：

1. 安装 [Visual Studio Build Tools](https://visualstudio.microsoft.com/zh-hans/downloads/?q=build+tools)
2. 或安装 [MinGW-w64](https://www.mingw-w64.org/)
3. 确保 `gcc` 或 `cl.exe` 在 PATH 中

### Electron 运行失败

- 确保 Rust CLI 已编译：`cd rust-core && cargo build --release`
- 检查 `prompt-forge-cli.exe` 是否存在

## License

MIT
