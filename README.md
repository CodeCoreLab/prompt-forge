# Prompt Forge

> 提示词增强工具 —— **纯 Web 前端 + Rust HTTP 服务**架构
> 单文件 1.59 MB，零非系统 DLL 依赖，Windows 免安装。

---

## 核心能力：四维正交增强

增强效果由四个**互不干扰**的维度共同决定，可任意组合：

| 维度 | 可选值 | 作用 |
|------|--------|------|
| **增强模式** | 目标任务 / 人物设定 / 全局（+ 自定义） | 决定产出**结构** |
| **增强方式** | 通用 / 深度 / 代码 / 创意 / 严谨 | 决定增强**程度** |
| **深度思考强度** | 关闭 / 轻 / 标准 / 深度 / 极致 | 分配模型**推理预算** |
| **生效角色** | 通用助手 / 资深技术专家 / 代码评审官 / 产品经理 / 技术文档写手（+ 自定义） | 决定**以谁的身份**产出 |

- 角色作为**独立 system 消息**注入，不污染被增强的素材
- 思考强度映射到各协议的推理参数；**网关不支持时自动降级重试**，不影响可用性
- 顶栏常驻显示当前生效角色，随时可切

---

## 快速开始

### 发行版（日常使用）

```
dist\启动-发行版.bat
```

双击即用：启动本地 HTTP 服务 → 自动打开浏览器 → 访问 http://127.0.0.1:8787

### 开发版（改前端热更）

```
dist\启动-开发版.bat
```

前端从 `web/` 目录读取。改完 `index.html` / `app.js` / `style.css` 后**刷新浏览器即生效**，无需重编译 Rust。

### 从源码构建

```bash
cargo build --release
# 产物: target/release/prompt-forge.exe
```

需 Rust 1.75+（`https://rustup.rs`）。无需 Node、无需 Python、无需 Visual Studio。

---

## 命令行参数

| 参数 | 说明 | 默认 |
|------|------|------|
| `--port <端口>` | 监听端口。**显式指定时若被占用直接报错退出**（不静默换端口）；未指定而默认端口被占用时自动顺延到下一个可用端口（最多试探 20 个） | 8787 |
| `--no-browser` | 启动后不自动打开浏览器 | 自动打开 |
| `--web-dir <目录>` | 从指定目录读取前端资源（开发热更） | 内嵌资源 |
| `--help` | 显示帮助 | — |

---

## 配置位置

| 内容 | 路径（Windows） |
|------|----------------|
| 配置 | `%APPDATA%\prompt-forge\config.json` |
| 历史 | `%APPDATA%\prompt-forge\history.json` |
| 损坏留档 | `%APPDATA%\prompt-forge\config.json.corrupt` |

API Key 以 **DPAPI 加密**（`enc:` 前缀）落盘，当前用户作用域，跨用户/跨机器不可解。

---

## 架构

```
浏览器（用户已有）          Rust HTTP 服务（1.6MB）
├─ index.html              ├─ api.rs        11 个 JSON 端点
├─ app.js   fetch → API    ├─ llm.rs        四协议出站 + 思考参数 + 降级
└─ style.css               ├─ store.rs      配置/历史持久化
        │ HTTP/1.1 + JSON  ├─ static_files.rs 静态托管
        ▼                   ├─ state.rs      取消令牌 + 配置锁
┌──────────────────────┐   ├─ autostart.rs  注册表自启
│ prompt-forge-core     │   └─ paths.rs      跨平台路径
│ ├─ modes.rs  模式模板 │           │
│ ├─ methods.rs 增强方式 │           ▼
│ ├─ roles.rs  角色系统 │   ┌──────────────────┐
│ ├─ thinking.rs 推理参数│   │ prompt-forge-core│
│ ├─ endpoint.rs 端点解析│   │ （纯逻辑，零 I/O）│
│ ├─ normalize.rs 规整 │           │
│ ├─ config.rs 配置模型 │   └──────────────────┘
│ ├─ crypto.rs DPAPI    │
│ └─ llm.rs  协议纯函数 │
└──────────────────────┘
```

完整设计说明见 [DESIGN.md](DESIGN.md)。

---

## API 契约

所有端点返回 JSON。成功 `{"ok":true,...}`，失败 `{"ok":false,"error":"CODE:message"}`。

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/modes` | 模式列表（内置 + 自定义） |
| GET | `/api/methods` | 增强方式 + 思考强度枚举 |
| GET | `/api/roles` | 角色列表 + 当前生效角色 |
| POST | `/api/parse-url` | 端点解析 `{url, protocol}` |
| GET | `/api/config` | 读取配置（key 解密为明文） |
| POST | `/api/config` | 保存配置 `{config}` |
| POST | `/api/test` | 测试连接 `{profile}` |
| POST | `/api/enhance` | 增强 `{input, mode_id, method_id?, thinking?, role_id?}` |
| POST | `/api/cancel` | 取消当前增强 |
| GET | `/api/history` | 历史记录（最新在前） |
| POST | `/api/clipboard` | 写剪贴板 `{text}` |

错误码：`unauthorized` `forbidden` `rate_limited` `timeout` `server` `network` `http` `config` `no_profile` `empty_result` `cancelled`

---

## 支持的 LLM 协议

| 协议 | 端点 | 认证 | 思考参数 |
|------|------|------|---------|
| OpenAI 兼容 | `{base}/v1/chat/completions` | `Authorization: Bearer` | `reasoning_effort` + `enable_thinking` |
| Anthropic | `{base}/v1/messages` | `x-api-key` + `anthropic-version` | `thinking.budget_tokens` |
| Ollama | `{base}/api/generate` | 可选 | `think` |
| Gemini | `{base}/v1beta/models/{model}:generateContent` | `x-goog-api-key` | `generationConfig.thinkingConfig` |

重试策略：429 与 5xx 指数退避（500ms 起，最多 3 次）；其余 4xx 不重试。
**降级策略**：网关返回 400/422 且拒绝推理参数时，自动去掉该参数重试一次。

---

## 地址智能解析

在设置中粘贴任意地址，自动拆解并回填基础地址：

```
粘贴 https://x.com/v1/chat/completions
  → 协议 https · 主机 x.com · 路径 /v1/chat/completions
  → 「回填基础地址」→ https://x.com/v1
```

缺协议自动补全（本地地址补 http，其余补 https），支持端口、query、IPv6。

---

## 测试

```bash
cargo test          # 40 个核心层单测（含 DPAPI 往返、端点解析、降级判定）
```
