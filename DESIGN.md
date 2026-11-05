# Prompt Forge — 轻量架构设计文档

> 版本：1.0.0 · 架构：纯 Web 前端 + Rust HTTP 服务
> 本文档回答五个问题：旧方案问题在哪、新架构长什么样、要动哪些文件、依赖怎么选、怎么落地与避险。

---

## 一、当前方案的问题评估（Electron 版）

已被用户指令删除，此处保留评估结论作为选型依据。

| 维度 | 问题 | 实测证据 |
|------|------|----------|
| **编译难度** | 4 套工具链叠加：Rust(msvc/gnu) + Node + electron-builder + NSIS/winCodeSign | 本机 Windows 无开发者模式 → NSIS 打包因 `Cannot create symbolic link` 直接失败；需自写 7za shim 绕过（已随版本删除） |
| **依赖体积** | Electron 运行时 188MB + Chromium + `node_modules` 1.6GB | Chromium 与用户已有浏览器重复；安装包 75MB（便携版）→ 本方案 **1.6MB** |
| **升级成本** | Electron 大版本需同步 Chromium/Node ABI；原生模块需 node-gyp 重建 | `WebView2Loader.dll` 缺失导致 exe 无法启动的根因即来自打包链产物缺失 |
| **功能缺陷** | 模式下拉框为空（`get_modes` 返回值未正确注入 UI） | Tauri CLI 直连 `get_modes` 正常返回 3 个模式，问题在 IPC 桥接层，未定位即随版本废弃 |

**根因归纳**：Electron 的复杂度不来自业务逻辑，而来自"自带浏览器内核 + 打包链 + IPC 桥接"三件与业务无关的事。业务只需求：一个本地服务 + 一个网页。

---

## 二、改后的整体架构

### 2.1 架构总览

```
┌────────────────────────────────────────────────────────────┐
│  浏览器（用户已有的 Chrome/Edge/360）                        │
│  ├─ index.html   结构（原生 DOM，零框架）                    │
│  ├─ app.js       逻辑（fetch → JSON API，零依赖）            │
│  └─ style.css    样式（CSS 变量，零预处理器）                │
└───────────────────────────┬────────────────────────────────┘
                            │ HTTP/1.1 + JSON
                            │ fetch('/api/xxx')
                            ▼
┌────────────────────────────────────────────────────────────┐
│  prompt-forge.exe  （Rust，单文件 1.6MB）                   │
│                                                            │
│  main.rs        入口：参数解析 / 端口绑定 / 浏览器拉起        │
│  api.rs         API 层：8 个端点 + 统一错误封装               │
│  static_files.rs 静态托管：内嵌资源表 / 磁盘热更            │
│  llm.rs         出站层：四协议适配 + 重试 + 错误分类          │
│  store.rs       持久化：配置读写 + 历史（原子写/损坏留档）      │
│  paths.rs       路径解析：按平台定位配置目录                   │
│  autostart.rs   开机自启：注册表 Run 键                       │
│  state.rs       共享状态：取消令牌槽 + 配置互斥锁            │
└───────────────────────────┬────────────────────────────────┘
                            │ 依赖
                            ▼
┌────────────────────────────────────────────────────────────┐
│  prompt-forge-core  （纯逻辑层，零框架依赖）                  │
│  modes.rs     增强模式模板（COMMON 保真纪律 + 策略段）        │
│  normalize.rs 输出规整管道（think块/围栏/引号/空判定）        │
│  config.rs    AppConfig / LlmConfig 模型与校验               │
│  crypto.rs    DPAPI 加解密（crypt32.dll C ABI 直调）         │
│  llm.rs       协议纯函数（endpoint/错误分类/体截断）          │
└────────────────────────────────────────────────────────────┘
```

### 2.2 前后端职责划分

| 层 | 职责 | 不做 |
|----|------|------|
| **前端** | 渲染、输入采集、调用 API、错误码→动作按钮映射、剪贴板写入 | 不做任何业务判断、不接触凭据、不持久化 |
| **API 层** | 参数解析（入口一次校验）、编排、错误码归一、HTTP 状态映射 | 不含业务算法、不做 IO |
| **出站层** | 四协议请求构造、重试策略、响应提取、错误分类 | 不关心调用来源 |
| **持久化层** | 配置/历史读写、原子写、损坏留档、DPAPI 加解密 | 不含业务规则 |
| **核心层** | 纯函数：模板组装、输出规整、模型校验、加解密 | 零 I/O、零框架依赖（可独立 `cargo test`） |

**依赖方向单向向下**：前端 → API → 出站/持久化 → 核心。核心层不依赖任何上层，可单独测试。

### 2.3 通信方式

- **协议**：HTTP/1.1 + JSON（`Content-Type: application/json`）
- **编码**：UTF-8
- **响应包络**：`{"ok":true, ...}` 或 `{"ok":false,"error":"CODE:message","code":"...","message":"..."}`
- **错误码沿用旧版**：`unauthorized` / `forbidden` / `rate_limited` / `timeout` / `server` / `network` / `http` / `config` / `no_profile` / `empty_result` / `cancelled`
  → 前端 `renderError` 的 code→动作映射逻辑**原样复用**，行为等价
- **绑定地址**：`127.0.0.1`（仅本机可访问，不暴露到网络）

**端点契约**（与旧版 Tauri command 一一对应）：

| 方法 | 路径 | 旧命令 | 请求体 | 响应字段 |
|------|------|--------|--------|----------|
| GET | `/api/modes` | `get_modes` | — | `modes[]` |
| GET | `/api/config` | `get_config` | — | `config`（key 解密为明文） |
| POST | `/api/config` | `save_config` | `{config}` | — |
| POST | `/api/test` | `test_llm` | `{profile}` | `output` |
| POST | `/api/enhance` | `enhance_prompt` | `{input,mode_id}` | `output` |
| POST | `/api/cancel` | `cancel_enhance` | — | — |
| GET | `/api/history` | `get_history` | — | `history[]` |
| POST | `/api/clipboard` | `copy_to_clipboard` | `{text}` | — |

### 2.4 数据流

**增强主链路**：
```
输入框 → app.js 组装 {input,mode_id}
      → POST /api/enhance
      → API 层校验入参（一次）
      → store 读配置快照（持 config_lock）
      → core.modes 组装完整提示词（COMMON + 策略段 + <raw_prompt>包裹）
      → core.crypto 解密 API Key（enc: → 明文）
      → state.begin_enhance() 换新取消令牌（旧轮次作废）
      → llm 层出站调用（429/5xx 指数退避重试 3 次）
      → core.normalize 规整输出（剥 think块/围栏/引号 + 空判定）
      → 成功则写 history.json
      → 200 {output}
```

**取消链路**：
```
前端点「取消」→ POST /api/cancel → state.trigger_cancel() → CancellationToken.cancel()
→ 出站 await 被 select! 打断 → 返回 cancelled:已取消（前端静默处理，不弹错误）
```

**配置保存链路**：
```
前端收集表单 → POST /api/config {config}
  → API 层反序列化（入口一次校验）
  → 持 config_lock（互斥，防并发写覆盖）
  → autostart.apply() 同步注册表 Run 键
  → store.save_config()：校验 → 明文 key 加密为 enc: → 原子写（tmp + rename）
```

---

## 三、文件清单

### 新增（`prompt-forge-web/`）

| 文件 | 行数 | 职责 |
|------|------|------|
| `Cargo.toml` | 24 | workspace 定义，release profile（opt-level=z / lto / strip） |
| `crates/prompt-forge-core/Cargo.toml` | 17 | 核心层依赖（serde / serde_json / base64 / libloading） |
| `crates/prompt-forge-core/src/{config,crypto,llm,modes,normalize}.rs` | 800 | 核心纯逻辑（自 Tauri 版原样迁移） |
| `crates/prompt-forge-core/src/ports.rs` | 99 | 端口候选序列 + 显式端口占用处置文案（纯逻辑，可独立测试） |
| `crates/prompt-forge-server/Cargo.toml` | 40 | 服务端依赖（axum / tokio / reqwest / winreg） |
| `crates/prompt-forge-server/build.rs` | 96 | 遍历 `web/` 生成内嵌资源表（零额外依赖） |
| `crates/prompt-forge-server/src/main.rs` | 310 | 入口：参数、端口绑定（显式精确 / 默认顺延）、路由装配、浏览器拉起、优雅退出 |
| `crates/prompt-forge-server/src/api.rs` | 340 | 8 个端点 + 统一错误封装 |
| `crates/prompt-forge-server/src/llm.rs` | 330 | 四协议出站 + 重试 + 错误分类 |
| `crates/prompt-forge-server/src/store.rs` | 175 | 配置/历史持久化（原子写、损坏留档、DPAPI） |
| `crates/prompt-forge-server/src/paths.rs` | 62 | 跨平台配置目录解析 |
| `crates/prompt-forge-server/src/autostart.rs` | 80 | 注册表 Run 键读写 |
| `crates/prompt-forge-server/src/state.rs` | 57 | 共享状态（取消令牌 + 配置锁） |
| `crates/prompt-forge-server/src/static_files.rs` | 116 | 静态托管（内嵌表 + 磁盘热更 + SPA 兜底） |
| `web/{index.html,app.js,style.css}` | ~640 | 前端三件套（原生，零框架） |
| `dist/PromptForge.exe` | — | 发行产物 1.6MB |
| `dist/启动-发行版.bat` / `启动-开发版.bat` | — | 两个试用版本的启动脚本 |

### 修改

| 文件 | 修改内容 |
|------|----------|
| `web/app.js` | `invoke()` → `fetch()` API 层；新增 `api()` 统一封装（错误仍抛 `code:message`）；剪贴板改原生 API + 后端兜底 |

### 复用未改

`crates/prompt-forge-core/src/*` 五个文件与 Tauri 版**逐字相同**——业务逻辑零改动，仅换宿主。

---

## 四、依赖选型对比

### 4.1 HTTP 服务框架

| 方案 | 依赖数 | 编译期 | 二进制增量 | 异步支持 | 结论 |
|------|--------|--------|-----------|---------|------|
| **axum 0.8** | +7（含 tokio） | +35s | +0.9MB | ✅ 原生 async | **选中** |
| tiny_http | +3 | +8s | +0.2MB | ❌ 同步（需 thread pool 桥接） | 取消机制难以精确复刻 |
| warp | +12 | +50s | +1.4MB | ✅ | 依赖更重，无收益 |
| actix-web | +40 | +90s | +3.5MB | ✅ | 明显过重 |

> axum 的异步能力是**功能性必需**：优雅取消依赖 `tokio::select!` + `CancellationToken`，同步框架需用线程中断模拟，取消时机不精确。

### 4.2 出站 HTTP

| 方案 | TLS | 依赖 | 结论 |
|------|-----|------|------|
| **reqwest 0.12 + native-tls** | SChannel（系统自带） | +6 | **选中**：Windows 无需 OpenSSL/cert 工具链 |
| reqwest + rustls | 纯 Rust | +12 | 编译更重，aws-lc-sys 需 cmake+nasm |
| ureq | rustls | +8 | 阻塞式，无法与 axum async 混用 |

### 4.3 其余依赖

| 用途 | 方案 | 对比说明 |
|------|------|----------|
| 静态资源内嵌 | **自写 build.rs** | 对比 `rust-embed`(+5 crate)/`include_dir`(+3 crate)：生成式 include_bytes 零依赖 |
| 开机自启 | **winreg 0.55** | 对比 `auto-launch`(+4 crate，含 shell 解析)：直接写注册表更可控 |
| 取消令牌 | **tokio-util 0.7** | Tokio 官方，1 个依赖 |
| 配置路径 | **自写 paths.rs** | 对比 `dirs`(+2 crate)：规则简单，无需抽象 |

**核心层依赖保持零增长**：`serde` / `serde_json` / `base64` / `libloading`（Windows）——与 Tauri 版完全一致。

### 4.4 依赖总量对比

| | Electron 版 | 本方案 | 变化 |
|---|-----------|--------|------|
| 运行时依赖 | Electron 188MB + Chromium + node_modules 1.6GB | — | — |
| 交付体积 | 75MB（便携版） | **1.6MB** | **↓ 97.9%** |
| 编译工具链 | Rust + Node + electron-builder + NSIS | **仅 Rust** | ↓3 套 |
| 非系统 DLL | WebView2Loader.dll 等 | **0 个** | 根治启动失败 |

---

## 五、分步实施计划

| 阶段 | 内容 | 验证方式 | 状态 |
|------|------|----------|------|
| 1 | 核心层迁移（零改动复用） | `cargo test -p prompt-forge-core` 13 passed | ✅ |
| 2 | 服务端骨架：paths / store / state | 配置读写往返、DPAPI 往返 | ✅ |
| 3 | API 层 8 端点 + 错误封装 | curl 逐端点返回预期 JSON | ✅ |
| 4 | 出站层四协议 + 重试 | mock LLM 服务实测增强链路 | ✅ |
| 5 | 静态托管（内嵌 + 热更） | HTTP 200 + Content-Type 正确 | ✅ |
| 6 | 前端 IPC→fetch 改造 | Playwright 实测 UI 交互 | ✅ |
| 7 | 取消语义复刻 | 10s 延迟 mock，1s 内取消返回 | ✅ |
| 8 | 发行构建 + 依赖审计 | objdump 确认零非系统 DLL | ✅ |

---

## 六、迁移风险点

| 风险 | 等级 | 缓解措施 |
|------|------|----------|
| **托盘常驻能力缺失** | P1 | 纯 Web 无系统托盘。降级：服务后台常驻 + 浏览器访问；自启改注册表实现。若需托盘须引入 GUI 依赖，与"轻量"目标冲突，故不做 |
| **关闭窗口到托盘语义变化** | P2 | 旧版 `close_to_tray` 保留字段但纯 Web 下无窗口可关，前端开关保留不报错 |
| **浏览器剪贴板权限** | P2 | `navigator.clipboard` 需安全上下文；已实现后端 `clip.exe` 兜底，双路径均实测通过 |
| **并发配置写覆盖** | P1 | `config_lock` 互斥 + 原子写（tmp→rename），已实现 |
| **配置损坏** | P1 | 解析失败自动留档 `config.json.corrupt` 并返回默认值，不静默丢数据（沿用旧版策略） |
| **端口冲突** | P2 | 默认端口被占用 → 向后试探 20 个并打印实际端口；**显式 `--port` 被占用 → 直接报错退出**（不静默换端口，避免防火墙/书签/脚本指向失效端口） |
| **API Key 明文返回给前端** | P2 | 与旧版同策略（前端需回显编辑）；DPAPI 保护的是**落盘**形态，非传输形态。服务仅绑 127.0.0.1 |
| **历史记录从内存改为落盘** | P2 | 旧版 Tauri 为进程内栈（重启丢失），本版落 `history.json` 持久化——**功能增强**，非破坏性变更 |

---

## 七、功能等价性对照

| 能力 | Tauri 版 | 本方案 | 等价性 |
|------|---------|--------|--------|
| 增强提示词（4 协议） | ✅ | ✅ | **完全等价**（出站层代码逐字迁移） |
| 3 个内置模式 + 自定义模式 | ✅ | ✅ | 完全等价 |
| 输出规整（think块/围栏/引号） | ✅ | ✅ | 完全等价（core 未改） |
| 错误码→动作按钮 | ✅ | ✅ | 完全等价（错误码与映射表未改） |
| 优雅取消 | ✅ | ✅ | **完全等价**（实测 1s 内生效） |
| API Key DPAPI 加密落盘 | ✅ | ✅ | 完全等价（实测 enc: 落盘 + 解密回读） |
| 配置/历史持久化 | ✅ | ✅ | **增强**（历史跨重启保留） |
| 多配置档 / 测试连接 | ✅ | ✅ | 完全等价 |
| 剪贴板复制 | ✅ | ✅ | 完全等价（原生 API + 后端兜底） |
| 开机自启 | ✅（插件） | ✅（注册表） | 语义等价，实现不同 |
| 关闭窗口到托盘 | ✅ | ⚠️ 降级 | 纯 Web 无窗口，托盘需 GUI 依赖，与轻量目标冲突 |
| 系统托盘图标 | ✅ | ❌ | 同上 |

---

## 八、两个试用版本

| 版本 | 形态 | 启动方式 | 适用场景 |
|------|------|----------|----------|
| **A 发行版** | 单文件 `PromptForge.exe`（1.6MB），前端内嵌 | `dist\启动-发行版.bat` | 日常使用、拷贝即用 |
| **B 开发版** | 同一 exe + `--web-dir web`，前端读磁盘 | `dist\启动-开发版.bat` | 改前端热更（改完刷新浏览器即生效） |

两者共享同一份配置（`%APPDATA%\prompt-forge\config.json`）与历史（`history.json`）。

---

## 九、功能与选项扩展（2026-11-05 v1.1）

> 参考 DSH（DeepSeek Harness）插件生态的设计思路：
> `zzyyun/dsh-prompt-enhance`、`Fishsb/dsh-prompt-enhancer`、`Yocaihua8/dsh-prompt-improver`。
> 取其**可配置维度**（分级模式 / 思考开关 / 角色解耦 / 模型可替换），舍其插件宿主绑定。

### 9.1 参考项目的可借鉴点与取舍

| DSH 插件做法 | 本作采纳 | 差异理由 |
|---|---|---|
| 5 种优化模式（基础/轻量/标准/专家/一键发布） | ✅ 采纳为**增强方式** 5 档 | DSH 是单列表平铺；本作拆成「模式(结构) × 方式(程度)」正交两轴，组合空间 3×5 |
| 模型链（多模型按序尝试） | ⏸️ 暂缓 | 本作定位单机单配置；多模型链属 P2，不在本轮范围 |
| 思考开关（bool） | ✅ 升级为**5 档强度** | DSH 只有开关，本作按 token 预算分级 |
| 增强模型与聊天模型解耦 | ✅ 体现为「角色 ≠ 模型」 | 本作用角色表达"以谁的身份"，比换模型更贴合提示词增强场景 |
| 预览后采用 / 可撤回 | ⏸️ 暂缓 | 当前是桌面工具直接输出，预览流价值有限 |
| 记忆链（多轮累积） | ⏸️ 暂缓 | 与"保真纪律"冲突（累积会引入语义漂移），本期不做 |

### 9.2 四维正交增强模型

新增能力不与既有「增强模式」混为一谈，而是拆成四个正交维度：

| 维度 | 取值 | 作用位置 | 实现载体 |
|---|---|---|---|
| **增强模式** mode | 目标任务 / 人物设定 / 全局（+自定义） | 提示词正文 | `modes.rs`（原有） |
| **增强方式** method | 通用 / 深度 / 代码 / 创意 / 严谨 | 提示词正文（COMMON 之后、模式之前） | `methods.rs`（新增） |
| **思考强度** thinking | 关闭 / 轻 / 标准 / 深度 / 极致 | **协议请求参数** | `thinking.rs`（新增） |
| **生效角色** role | 5 内置 + 自定义 | **独立 system 消息** | `roles.rs`（新增） |

提示词最终结构（顺序即优先级）：

```
COMMON 保真纪律（置顶，不可被削弱）
  → 增强方式策略段
    → 增强模式策略段
      → <raw_prompt> 用户输入
角色 → 独立 system 消息（不混入正文，避免污染被增强素材）
```

### 9.3 深度思考强度的协议适配与降级

不同协议的推理参数完全不同，且**大量中转站并不支持**——硬传会直接 400 打断流程。

| 协议 | 注入参数 | 预算映射 |
|---|---|---|
| OpenAI 兼容 | `reasoning_effort` + `enable_thinking` | low→low / standard→medium / deep·max→high |
| Anthropic | `thinking: {type:"enabled", budget_tokens:N}` | 1024 / 4096 / 16384 / 32768；`max_tokens` 自动抬到 2× 预算 |
| Gemini | `generationConfig.thinkingConfig.thinkingBudget` | 同上（-1 动态思考留给模型自决） |
| Ollama | `think: true` | 仅布尔 |

**降级机制**（关键设计）：首次携带推理参数请求 → 若响应 400/422 且错误体含
`unsupported` / `unknown parameter` 等特征词**且**命中参数名线索 → 判定为
"网关不支持该参数" → **自动去掉参数重试一次**（只降级一次，防死循环）。

实测证据（mock 网关拒绝 `reasoning_effort`）：
```
请求[0] 带推理参数=True  → 400 Unsupported parameter
请求[1] 带推理参数=False → 200 OK，降级成功
```
这条链路保证了：**不支持思考参数的服务仍能正常工作**，旧版行为不被破坏。

### 9.4 端点解析与全 URL 回填

用户常粘贴完整接口 URL（`https://x.com/v1/chat/completions`），而配置项要求基础地址。
`endpoint.rs` 提供纯函数解析，自动拆解并回填：

```
输入: https://note3-prev-api.askdiandian.com/v1/chat/completions
输出: scheme=https  host=note3-prev-api.askdiandian.com  port=-
      path=/v1/chat/completions
      base_url=https://note3-prev-api.askdiandian.com/v1   ← 回填值
      notes=["已识别接口路径 /chat/completions，基础地址回填为不含接口的根"]
```

覆盖：缺协议自动补（本地地址补 http，其余补 https）、带端口、query/fragment 剥离、
IPv6 字面量、Anthropic/Ollama/Gemini 各自的接口后缀识别。

### 9.5 界面重构

设计语言：**字节跳动 Arco（克制蓝 + 高密度信息）+ 阿里 Ant Design（卡片容器 + 8px 圆角 + 轻投影）**。

- 主色 字节蓝 `#165DFF` / 辅色 阿里橙 `#FF6A00`（角色标识、深度思考档位用橙色强调）
- 字体 阿里普惠体 + HarmonyOS Sans（中文设计系统官方字体）
- 卡片式布局：增强配置卡 + 输入卡 + 输出卡，统一 12px 圆角与两级阴影
- 顶栏常驻**生效角色标识**（橙色胶囊 + 呼吸点），点击直达角色管理
- 设置改为**右侧抽屉 + 标签页**（模型接入 / 角色管理 / 系统），替代原居中模态
- 响应式：1100px 双列→单列；760px 配置项纵向堆叠、按钮通栏；420px 顶栏精简

### 9.6 本轮文件清单

| 文件 | 状态 | 说明 |
|---|---|---|
| `crates/prompt-forge-core/src/methods.rs` | 新增 | 增强方式 5 档 + 思考强度 5 级枚举与预算映射 |
| `crates/prompt-forge-core/src/roles.rs` | 新增 | 5 个内置角色 + 自定义角色 + 解析回落 |
| `crates/prompt-forge-core/src/thinking.rs` | 新增 | 四协议推理参数生成 + 降级判定纯函数 |
| `crates/prompt-forge-core/src/endpoint.rs` | 新增 | URL 拆解与基础地址回填 |
| `crates/prompt-forge-core/src/config.rs` | 修改 | 新增 `EnhanceOptions` / `custom_roles`，全部 `serde(default)` 向后兼容 |
| `crates/prompt-forge-core/src/modes.rs` | 修改 | `build_prompt_with` / `build_custom_prompt_with` 支持方式注入 |
| `crates/prompt-forge-core/src/lib.rs` | 修改 | 注册 4 个新模块 |
| `crates/prompt-forge-server/src/llm.rs` | 修改 | `EnhanceRequest`（角色+思考）、system 消息注入、`send_with_degrade` 降级重试 |
| `crates/prompt-forge-server/src/api.rs` | 修改 | 新增 3 端点；`/api/enhance` 接受 method/thinking/role 覆盖并回显 `applied` |
| `crates/prompt-forge-server/src/main.rs` | 修改 | 路由注册 |
| `web/index.html` | 重写 | 卡片式布局、配置卡、抽屉式设置、URL 解析提示区 |
| `web/style.css` | 重写 | 完整设计系统（亮/暗双主题、响应式断点） |
| `web/app.js` | 重写 | 四维增强选项管理、角色 CRUD、URL 解析回填 |

**兼容性**：所有新增配置字段均带 `serde(default)`，旧 `config.json` 无需迁移即可加载
（实测：缺 `enhance` 字段的旧配置自动回落默认值，链路正常）。

### 9.7 新增 API 契约

| 方法 | 路径 | 返回 | 说明 |
|---|---|---|---|
| GET | `/api/methods` | `{methods[], thinking_levels[], default_method_id, default_thinking}` | 增强方式与思考强度枚举 |
| GET | `/api/roles` | `{roles[], current{}}` | 角色列表 + 当前生效角色 |
| POST | `/api/parse-url` | `{scheme, host, port, path, base_url, normalized, notes[], suggested_endpoint}` | 端点解析 |

`/api/enhance` 响应新增 `applied` 字段回显本次实际生效的组合：
```json
{"ok":true,"output":"...","applied":{"method_id":"deep","thinking":"standard","role_id":"builtin-product","role_name":"产品经理"}}
```

请求体新增可选字段 `method_id` / `thinking` / `role_id`——**优先级：请求体 > 已保存配置**，
前端切换即生效（无需先点保存）。
