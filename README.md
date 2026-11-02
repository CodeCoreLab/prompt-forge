# Prompt Forge 提示词快速增强工具

Rust + Tauri 2 桌面端，一键将简短指令增强为高质量提示词。

## 功能

- 紧凑主窗口：输入框 + 输出框，一键增强、一键复制
- 快速增强：单轮 LLM 调用直接输出，不做多步推理
- 增强模式：目标拆解 / 结构化角色 / 全局增强，架构可扩展
- 多配置 LLM 管理：兼容 OpenAI 兼容、Anthropic (Claude)、Ollama、Gemini 协议
- 系统集成：托盘常驻、最小化到托盘、开机自启，均可在设置中开关

## 开发

```bash
npm install
npm run tauri dev
```

## 构建

```bash
npm run tauri build
```

## 配置

- Windows: `%APPDATA%\prompt-forge\config.json`
- macOS: `~/Library/Application Support/prompt-forge/config.json`
- Linux: `~/.config/prompt-forge/config.json`

API Key 存储于本地配置文件，不上传。
