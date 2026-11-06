// 提示词增强器 前端逻辑：纯原生 JS，无框架依赖
//
// 通信方式：HTTP + JSON API（后端为 Rust HTTP 服务，非 Electron IPC）。
// 错误语义与旧版完全一致：错误串格式 `code:message`，由 renderError 按 code 渲染动作。

const $ = (id) => document.getElementById(id);

// ── 常量区 ────────────────────────────
// 数值边界与后端保持一致（crates/prompt-forge-core/src/config.rs）
const DEFAULT_TEMPERATURE = 0.7;        // LlmConfig.temperature 默认值
const TEMPERATURE_MIN = 0;              // temperature 允许区间
const TEMPERATURE_MAX = 2;
const TIMEOUT_SECS_MIN = 0;             // timeout_secs 允许区间（0 = 用后端默认）
const TIMEOUT_SECS_MAX = 3600;
const MAX_TOKENS_MIN = 0;               // max_tokens 允许区间（0 = 按输入长度自动估算）
const MAX_TOKENS_MAX = 100000;

// 网络行为
const DEFAULT_TIMEOUT_SECS = 120;       // 与后端 LlmConfig::effective_timeout_secs 一致
const DEFAULT_API_TIMEOUT_MS = 20000;   // 普通 API 请求超时（配置读写等）

// 协议枚举：与后端 LlmConfig::PROTOCOLS 完全一致
// （crates/prompt-forge-core/src/config.rs: PROTOCOLS = openai / anthropic / ollama / gemini）
const ALLOWED_PROTOCOLS = ["openai", "anthropic", "ollama", "gemini"];
const HISTORY_CAP = 20;                 // 与后端 store::HISTORY_CAP 对齐
const URL_PARSE_DEBOUNCE_MS = 220;      // API 地址输入解析防抖
const URL_PARSE_PASTE_DELAY_MS = 60;    // 粘贴后立即解析（不等 input 防抖）
const SAVE_HINT_CLEAR_MS = 1500;        // 「已保存」提示自动消失
const PERSIST_DEBOUNCE_MS = 320;        // 增强选项落盘防抖
const RATE_LIMIT_RETRY_MS = 5000;       // 限流后自动重试延迟

/** 各表单字段的行内提示文案（校验通过时显示默认说明，出错时替换为错误信息） */
const PROFILE_HINTS = {
  protocol: { hintId: "pfProtocolHint", inputId: "pfProtocol", def: "openai / anthropic / ollama / gemini" },
  baseUrl: { hintId: "pfBaseUrlHint", inputId: "pfBaseUrl", def: "以 http:// 或 https:// 开头" },
  model: { hintId: "pfModelHint", inputId: "pfModel", def: "模型名，如 deepseek-chat" },
  temp: { hintId: "pfTempHint", inputId: "pfTemp", def: "0 ~ 2，越低越稳定" },
  timeout: { hintId: "pfTimeoutHint", inputId: "pfTimeout", def: "0 = 使用默认 120 秒" },
  tokens: { hintId: "pfMaxTokensHint", inputId: "pfMaxTokens", def: "0 = 按输入长度自动估算" },
};

/**
 * 统一错误类型。
 * 与后端 ApiError 形态一致：对外表现为字符串 `code:message`，
 * 由 renderError 按 code 解析并给出可操作按钮。
 *
 * requestId：服务端分配的权威轮次序号（后端错误体 / 成功体顶层字段 request_id）。
 * 前端本地也有 enhanceSeq 竞态守卫，两者并存：本地守卫拦「自己发起的旧请求」，
 * requestId 用于识别「服务端判定本轮已被取代」。
 */
class ApiFail extends Error {
  constructor(code, message, requestId) {
    super(message);
    this.code = code;
    this.text = `${code}:${message}`;
    this.requestId = requestId ?? null; // 服务端轮次序号（可能为 null）
  }
}

/** 提取错误文本（兼容 ApiFail / 字符串） */
function errorText(e) {
  if (!e) return "error:未知错误";
  if (typeof e === "string") return e;
  if (e.text) return e.text;
  return `error:${e.message || e}`;
}

/**
 * 调用后端 API。
 * 成功返回业务字段；失败抛出 `code:message` 形态的错误（与旧版 invoke 行为一致）。
 *
 * @param {string} path 端点路径
 * @param {{method?:string, body?:any, timeoutMs?:number}} opts
 *        timeoutMs 缺省 20s；长耗时调用（增强 / 测试连接）需显式传入。
 */
async function api(path, { method = "GET", body, timeoutMs = DEFAULT_API_TIMEOUT_MS } = {}) {
  const init = { method, headers: {} };
  if (body !== undefined) {
    init.headers["Content-Type"] = "application/json";
    init.body = JSON.stringify(body);
  }
  // 超时控制：AbortController 到点中断，避免请求永久悬挂
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  let resp;
  try {
    resp = await fetch(path, { ...init, signal: controller.signal });
  } catch (e) {
    clearTimeout(timer);
    // 网络异常（服务未启动 / 连接被拒）与业务错误区分开，不把 TypeError 抛给用户
    if (e && e.name === "AbortError") throw new ApiFail("timeout", "请求超时");
    throw new ApiFail("network", "无法连接本地服务");
  }
  clearTimeout(timer);
  let data;
  try {
    data = await resp.json();
  } catch (e) {
    throw new ApiFail("http", `响应解析失败 (HTTP ${resp.status})`);
  }
  if (!resp.ok || data.ok === false) {
    // 后端 error 字段本身就是 `code:message`，优先原样透传以保持错误码语义
    // request_id 在错误体顶层，是服务端权威轮次序号 → 一并带出，不丢弃
    if (typeof data.error === "string" && data.error.includes(":")) {
      const sep = data.error.indexOf(":");
      throw new ApiFail(data.error.slice(0, sep), data.error.slice(sep + 1), data.request_id);
    }
    throw new ApiFail(
      data.code || "http",
      data.message || `请求失败 (HTTP ${resp.status})`,
      data.request_id
    );
  }
  return data;
}

// ── 状态 ──────────────────────────────
let config = null;     // AppConfig（Rust 侧结构）
let modes = [];        // 增强模式列表
let methods = [];      // 增强方式列表
let thinkingLevels = [];// 思考强度分级
let roles = [];        // 角色列表（内置 + 自定义）

/** 当前生效的增强选项（与后端 EnhanceOptions 同构） */
let enhanceOpts = {
  method_id: "general",
  thinking: "standard",
  role_id: "builtin-general",
};

/** 设置面板中模型配置档表单的状态（区分「用户主动清空 API Key」与「从未加载」） */
const profileFormState = {
  loaded: false,      // 表单是否已被 config 初始化过
  apiKeyDirty: false, // 用户是否手动编辑过 API Key 输入框
};

/** 竞态守卫：只接受最后一次请求的结果 */
let enhanceSeq = 0;
let testSeq = 0;
let urlParseSeq = 0;

/**
 * 服务端权威轮次序号（最新一次收到的 request_id）。
 * 与本地 enhanceSeq 互补：本地守卫只能拦「本前端发起的旧请求」，
 * 而服务端可能因其他原因废弃旧轮（begin_enhance），此时 request_id 才是权威判据。
 */
let latestServerReqId = 0;

/**
 * 角色回落标记（服务端 /api/roles 可选字段）。
 * roleMissing = true → 原生效角色已不存在，后端已回落到默认角色；
 * 字段缺失时保持 false，前端行为与从前完全一致。
 */
let roleMissing = false;
let currentRoleId = null;

/** 历史记录数据（供事件委托读取） */
let historyItems = [];

/** 最近一次成功落盘的配置档快照（与磁盘一致），用于超时口径对齐 */
let savedProfiles = [];

/**
 * 配置落盘队列：所有 POST /api/config 串行执行。
 *
 * 原因：并发的整包 POST 会互相覆盖 —— 例如「增删档位落盘」与
 * 「切换档位落盘」同时发出时，后到的请求体可能不含前一个请求刚写入的变更，
 * 造成磁盘配置被旧快照覆盖（丢档位 / active_index 错位）。
 * 串行化后，后发的保存总是带着最新的 config 对象。
 */
let configSaveChain = Promise.resolve();
function queueConfigSave() {
  const task = () => {
    // 落盘快照：请求体序列化发生在 api() 调用的那一刻，
    // 此刻的 config 就是将要写进磁盘的内容 → 同时留一份快照供超时口径使用。
    const snapshot = (config.profiles || []).map((p) => ({ ...p }));
    return api("/api/config", { method: "POST", body: { config } }).then((r) => {
      savedProfiles = snapshot;
      return r;
    });
  };
  // 无论前一次成功还是失败，本次保存都要执行 → 两个分支都走 task
  configSaveChain = configSaveChain.then(task, task);
  return configSaveChain;
}

// ── 初始化 ────────────────────────────
async function init() {
  try {
    const [m, cfg, mt, rl] = await Promise.all([
      api("/api/modes").then((d) => d.modes),
      api("/api/config").then((d) => d.config),
      api("/api/methods").then((d) => d),
      api("/api/roles").then((d) => d),
    ]);
    modes = m;
    config = cfg;
    // 磁盘快照初值：GET /api/config 返回的就是磁盘配置
    savedProfiles = (cfg.profiles || []).map((p) => ({ ...p }));
    methods = mt.methods || [];
    thinkingLevels = mt.thinking_levels || [];
    roles = rl.roles || [];
    // role_missing：后端可选字段。原选角色已失效、后端已回落到默认角色时为 true；
    // 字段缺失时保持 false，前端行为与从前完全一致。
    roleMissing = rl.role_missing === true;
    currentRoleId = rl.current_role_id ?? null;
    // 恢复已保存的增强选项（持久化 → 重开仍在）
    enhanceOpts = {
      method_id: config.enhance?.method_id || mt.default_method_id || "general",
      thinking: config.enhance?.thinking || mt.default_thinking || "standard",
      role_id: config.enhance?.role_id || "builtin-general",
    };
  } catch (e) {
    // 初始化失败：给出可点击的「重试」入口，避免用户只能刷新页面
    const el = $("statusText");
    el.className = "status err";
    el.textContent = `初始化失败: ${errorText(e)}`;
    const retry = document.createElement("button");
    retry.className = "ghost-btn btn-sm";
    retry.style.marginLeft = "8px";
    retry.textContent = "重试";
    retry.addEventListener("click", () => init());
    el.appendChild(retry);
    config = {
      active_index: 0, profiles: [], close_to_tray: true, autostart: true,
      custom_modes: [], custom_roles: [],
      enhance: { method_id: "general", thinking: "standard", role_id: "builtin-general" },
    };
  }
  renderModes();
  renderMethods();
  renderThinking();
  renderRoles();
  renderProfileSelect();
  updateRoleChip();
  updateSummary();
  $("inputBox").focus();
  // 角色失效（后端 role_missing）：顶栏芯片已显示「已回退」，状态栏再给一次明确告知
  if (roleMissing) {
    setStatus("原生效角色已不存在，已回退到默认角色，请重新选择", "err");
  }
  // 无任何配置时自动打开设置面板引导
  if (!config.profiles || config.profiles.length === 0) openSettings();
}

/** 保存后刷新模式下拉（自定义模式可能变更） */
async function refreshModes() {
  try {
    modes = (await api("/api/modes")).modes;
    renderModes();
  } catch (e) {
    // 刷新失败不阻断保存流程
  }
}

/** 保存后刷新角色列表（自定义角色可能变更） */
async function refreshRoles() {
  try {
    const d = await api("/api/roles");
    roles = d.roles || [];
    // role_missing：后端可选字段（所选角色不存在、已回退到默认角色时返回）。
    // 字段缺失时前端行为完全不变，不提示也不报错。
    roleMissing = d.role_missing === true;
    currentRoleId = d.current_role_id ?? null;
    renderRoles();
    updateRoleChip();
    if (roleMissing) {
      const el = $("crStatus");
      el.textContent = "当前生效角色不存在，已回退到默认角色";
      el.className = "status err";
    }
  } catch (e) {
    // 刷新失败不阻断保存流程
  }
}

// ── 增强模式 ──────────────────────────
function renderModes() {
  const sel = $("modeSelect");
  sel.replaceChildren();
  modes.forEach((m) => {
    const opt = document.createElement("option");
    opt.value = m.id;
    opt.textContent = m.name;
    opt.title = m.description || "";
    sel.appendChild(opt);
  });
}

// ── 增强方式 ──────────────────────────
function renderMethods() {
  const sel = $("methodSelect");
  sel.replaceChildren();
  methods.forEach((m) => {
    const opt = document.createElement("option");
    opt.value = m.id;
    opt.textContent = m.name;
    opt.title = m.description || "";
    sel.appendChild(opt);
  });
  sel.value = enhanceOpts.method_id;
}

// ── 深度思考强度（分段控件）────────────
// 采用事件委托（见下方 $("thinkingSeg") 监听），渲染时不再逐个 addEventListener
function renderThinking() {
  const seg = $("thinkingSeg");
  seg.replaceChildren();
  thinkingLevels.forEach((l) => {
    const b = document.createElement("button");
    b.type = "button";
    b.setAttribute("role", "radio");
    b.dataset.value = l.id;
    b.textContent = l.name;
    b.title = l.description + (l.budget_tokens ? `（推理预算 ${l.budget_tokens} tokens）` : "");
    b.setAttribute("aria-checked", l.id === enhanceOpts.thinking ? "true" : "false");
    if (l.id === enhanceOpts.thinking) b.classList.add("active");
    seg.appendChild(b);
  });
}

// ── 角色 ──────────────────────────────
function renderRoles() {
  const sel = $("roleSelect");
  const prev = enhanceOpts.role_id;
  sel.replaceChildren();
  roles.forEach((r) => {
    const opt = document.createElement("option");
    opt.value = r.id;
    opt.textContent = r.builtin ? r.name : `${r.name}（自定义）`;
    opt.title = r.description || "";
    sel.appendChild(opt);
  });
  sel.value = prev;
}

/** 更新顶栏「生效角色」标识 */
function updateRoleChip() {
  const r = roles.find((x) => x.id === enhanceOpts.role_id);
  // 角色失效时不能静默显示「通用助手」——顶栏是常驻可见的，
  // 用户必须知道自己实际用的不是所选角色。
  // 判定：生效角色 id 已不在角色列表中（服务端已回落到默认角色），
  // 或后端显式给了 role_missing 标记。
  const missing = !r;
  const name = r
    ? r.name
    : ((roleMissing || missing) ? "（角色已失效，已回退）" : "通用助手");
  $("roleChipName").textContent = name;
  $("roleChip").title = r
    ? r.description
    : ((roleMissing || missing)
      ? `原生效角色（${currentRoleId || enhanceOpts.role_id}）已不存在，服务端已回退到默认角色`
      : "当前生效角色");
}

/**
 * 底部配置摘要：把当前组合说清楚，避免用户"不知道自己在用什么"。
 * 模式 / 方式 / 角色名可能来自用户自定义配置 → 一律用 textContent 写入，禁止用 HTML 字符串拼接。
 */
function updateSummary() {
  const mode = modes.find((m) => m.id === $("modeSelect").value);
  const method = methods.find((m) => m.id === enhanceOpts.method_id);
  const think = thinkingLevels.find((l) => l.id === enhanceOpts.thinking);
  const role = roles.find((r) => r.id === enhanceOpts.role_id);
  const el = $("configSummary");
  el.replaceChildren();
  el.appendChild(document.createTextNode("当前组合："));
  const parts = [];
  if (mode) parts.push(["模式", mode.name]);
  if (method) parts.push(["方式", method.name]);
  if (think) parts.push(["思考", think.name]);
  if (role) parts.push(["角色", role.name]);
  parts.forEach(([label, name], i) => {
    if (i > 0) el.appendChild(document.createTextNode(" · "));
    el.appendChild(document.createTextNode(`${label} `));
    const b = document.createElement("b");
    b.textContent = name; // 用户自定义名称 → textContent
    el.appendChild(b);
  });
  if (think && think.budget_tokens) {
    el.appendChild(document.createTextNode(` · 推理预算 ${think.budget_tokens} tokens`));
  }
}

/**
 * 持久化增强选项。
 * 立即落盘（不阻塞 UI），失败仅提示不打断操作——增强请求本身也带上选项，
 * 因此即使落盘失败，本次增强仍按用户当前选择生效。
 */
let persistTimer = null;
function persistEnhanceOpts() {
  clearTimeout(persistTimer);
  persistTimer = setTimeout(async () => {
    try {
      config.enhance = { ...enhanceOpts };
      await queueConfigSave();
    } catch (e) {
      // 静默：下次保存时会一并写入
    }
  }, PERSIST_DEBOUNCE_MS);
}

// ── 增强 ──────────────────────────────
/**
 * 生效超时秒数 —— **与后端 `LlmConfig::effective_timeout_secs()` 同一口径**。
 *
 * 后端真源：`crates/prompt-forge-core/src/config.rs`
 * ```rust
 * pub fn effective_timeout_secs(&self) -> u64 {
 *     if self.timeout_secs == 0 { 120 } else { self.timeout_secs }
 * }
 * ```
 * （`timeout_secs` 是 `u64` 且 `#[serde(default)]` —— 字段缺失时反序列化为 0，
 *   最终同样落到 120。）
 *
 * 因此前端口径必须是：**0 / 缺失 / 非法 → 120（`DEFAULT_TIMEOUT_SECS`）**，
 * 其余取原值。非法值（NaN / 字符串 / 负数 / Infinity）同样按 120 处理，
 * 避免前端算出一个与服务端不一致的门槛。
 *
 * 所有需要「某配置档的有效超时」的地方都必须走本函数，
 * 不允许各自写一遍 `> 0 ? … : 120` —— 两处判定一旦漂移，
 * 前端门槛就可能小于服务端墙钟，导致双花（见 [`enhanceTimeoutMs`]）。
 */
function effectiveTimeoutSecs(raw) {
  const n = Number(raw);
  if (!Number.isFinite(n) || n <= 0) return DEFAULT_TIMEOUT_SECS;
  return n;
}

/**
 * 计算长耗时调用（/api/enhance、/api/test）的整体 abort 门槛。
 *
 * 服务端口径（后端已实现，前端不得改）：
 * - 单次出站超时 = T（T = 该配置档 timeout_secs，0/缺失按 120s）
 * - 服务端整体墙钟 = T × 6 + 30
 *   （×6 = 最大尝试次数 3 × 降级重试 2，+30 = 退避余量；
 *     该预算由服务端统一计算与约束，前端只按同一口径推算，不依赖其内部实现）
 *
 * 前端 = T × 6 + 90：比服务端多等 60s（30s 落盘余量 + 30s 网络与调度余量）。
 *
 * ⚠ 前端门槛必须**严格大于**服务端墙钟。否则前端会先 abort，
 * 而服务端仍在后台跑完整条重试链并写入历史 → 用户看到「请求超时」，
 * 随后历史里凭空多出一条，用户重试就是重复扣费（双花）。
 */
function enhanceTimeoutMs(timeoutSecs) {
  const secs = effectiveTimeoutSecs(timeoutSecs);
  return (secs * 6 + 90) * 1000;
}

/**
 * 当前生效配置档的 timeout_secs（秒）—— **以磁盘（已落盘）值为准**。
 *
 * 服务端 /api/enhance 用磁盘配置档的 timeout_secs 计算整体墙钟
 * （口径：effective_timeout_secs × 6 + 30），前端门槛必须严格大于它，
 * 否则前端先 abort、服务端仍在后台跑并写历史，用户重试就是重复扣费（双花）。
 *
 * 因此这里取 savedProfiles（最近一次成功落盘的快照，初值来自 GET /api/config），
 * 而不是内存里的 config.profiles —— 后者可能含用户尚未保存的表单改动。
 */
function currentProfileTimeoutSecs() {
  const idx = currentProfileIdx();
  const diskList = savedProfiles || [];
  // 磁盘快照里有这个下标 → 一律以磁盘为准。
  //
  // ⚠ timeout_secs == 0 是**合法值**（后端 effective_timeout_secs 按 120 处理），
  // 不能当成「缺失」而回退到内存值。否则用户把表单超时改小（5~109s）但没保存时：
  //   磁盘 0    → 服务端墙钟 120×6+30 = 750s
  //   内存 5    → 前端门槛   5×6+90   = 120s
  // 前端比服务端早 630 秒 abort → 服务端仍在后台跑完整条重试链并写入历史 →
  // 用户看到「请求超时」，历史凭空多一条，重试即重复扣费（双花）。
  if (idx < diskList.length) {
    // 口径与后端 `effective_timeout_secs` 一致：0 / 缺失 / 非法 → 120
    return effectiveTimeoutSecs(diskList[idx]?.timeout_secs);
  }
  // 磁盘快照里根本没有这个下标（如新增档位尚未落盘成功）→ 才回退内存，
  // 同样把 0 当作 120（与后端 effective_timeout_secs 一致）。
  return effectiveTimeoutSecs((config?.profiles || [])[idx]?.timeout_secs);
}

async function enhance() {
  const btn = $("enhanceBtn");
  // ── 取消分支必须最先判断 ──
  // 若放在输入/配置校验之后，用户在增强过程中清空输入框再点「取消增强」，
  // 会提前 return，/api/cancel 永远发不出去，增强任务无法中断。
  if (btn.dataset.canceling === "1") {
    await cancelEnhance();
    return;
  }

  const input = $("inputBox").value.trim();
  if (!input) { setStatus("请先输入提示词", "err"); return; }
  if (!config) { setStatus("正在初始化，请稍候", "err"); return; }
  if (!config.profiles || config.profiles.length === 0) {
    setStatus("尚未配置模型，请先打开设置", "err");
    openSettings();
    return;
  }
  // 等待在途的配置落盘完成：服务端 /api/enhance 用请求体的 profile_index
  // 去索引**磁盘**配置里的 profiles。若「增删档位」的落盘还在途中，
  // 磁盘与内存的 profiles 数组会错位 → profile_index 命中错误的档位。
  await configSaveChain.catch(() => {});

  const seq = ++enhanceSeq; // 竞态守卫：只有最后一次请求的结果可以写回界面
  clearTimeout(rateLimitTimer); // 新一轮开始 → 取消待触发的限流自动重试
  rateLimitTimer = null;
  btn.dataset.canceling = "1";
  btn.querySelector(".btn-label").textContent = "取消增强";
  btn.classList.add("canceling");
  $("outputBox").value = "";
  $("outputCounter").hidden = true;
  $("copyBtn").disabled = true;
  const statusEl = $("statusText");
  statusEl.className = "status busy";
  statusEl.replaceChildren();
  const spin = document.createElement("span");
  spin.className = "spin";
  statusEl.appendChild(spin);
  statusEl.appendChild(document.createTextNode("增强中…"));

  try {
    const result = await api("/api/enhance", {
      method: "POST",
      // 门槛 = T×6+90（服务端墙钟 T×6+30，再多等 60s 落盘余量），
      // 必须动态计算且严格大于服务端预算，否则前端会先 abort、
      // 把仍在后端跑重试链的请求甩在后台（重复扣费 + 历史多出一条）。
      timeoutMs: enhanceTimeoutMs(currentProfileTimeoutSecs()),
      body: {
        input,
        mode_id: $("modeSelect").value,
        method_id: enhanceOpts.method_id,
        thinking: enhanceOpts.thinking,
        role_id: enhanceOpts.role_id,
        // ── 配置档标识（双保险之一）──
        // 后端 /api/enhance 支持用请求体的 profile_index 覆盖磁盘上的 active_index，
        // 避免「前端切了档位但没保存」时请求打到旧配置档。
        // 取当前界面下拉框的下标（= profiles 数组下标），与后端契约一致；
        // profileSelect 的 change 处理器会同步写回 config.active_index（双保险之二）。
        profile_index: currentProfileIdx(),
      },
    });
    if (seq !== enhanceSeq) return; // 已被更新的请求取代 → 丢弃本次结果
    // 服务端权威轮次守卫：若本响应携带的 request_id 早于已收到的最新轮次 → 旧轮，静默丢弃
    if (typeof result.request_id === "number") {
      if (result.request_id < latestServerReqId) return;
      latestServerReqId = result.request_id;
    }
    const output = result.output || "";
    $("outputBox").value = output;
    $("copyBtn").disabled = false;
    const counter = $("outputCounter");
    counter.hidden = false;
    counter.textContent = `${output.length} 字`;
    const ap = result.applied || {};
    statusEl.className = "status ok";
    statusEl.textContent = `增强完成 · ${ap.role_name || ""}`;
  } catch (e) {
    if (seq !== enhanceSeq) return; // 旧请求的错误也不得覆盖新状态
    const s = errorText(e);
    // ── superseded：服务端判定本轮已被新请求取代 ──
    // 这是正常的竞态收敛（用户连点两次增强、或服务端 begin_enhance 废弃旧轮），
    // 不是故障 → 不进错误提示区、不弹任何提示。
    // 只清掉「增强中…」的转圈，避免界面停在等待态；
    // 若本前端已发起更新的请求，上面的 seq 守卫会先行 return，不会走到这里。
    if (s.startsWith("superseded:")) {
      statusEl.className = "";
      statusEl.textContent = "";
      return;
    }
    // 服务端权威轮次守卫：错误的 request_id 早于已收到的最新轮次 → 旧轮，静默丢弃
    if (e && typeof e.requestId === "number") {
      if (e.requestId < latestServerReqId) return;
      latestServerReqId = e.requestId;
    }
    statusEl.className = "";
    // cancelled 分支用 startsWith：后端改一个字（如「已取消」→「请求已取消」）
    // 前端不应因此漏判、把它当未知错误弹成红色。
    if (s.startsWith("cancelled:")) {
      setStatus("已取消", "");
    } else {
      renderError(s);
    }
  } finally {
    if (seq === enhanceSeq) {
      btn.querySelector(".btn-label").textContent = "开始增强";
      btn.classList.remove("canceling");
      btn.dataset.canceling = "0";
    }
  }
}

/**
 * 取消当前增强任务：必定发出 POST /api/cancel，并立即进入「取消中」状态。
 */
async function cancelEnhance() {
  const btn = $("enhanceBtn");
  btn.querySelector(".btn-label").textContent = "取消中…";
  btn.classList.add("canceling");
  setStatus("正在取消…", "");
  try {
    await api("/api/cancel", { method: "POST", timeoutMs: DEFAULT_API_TIMEOUT_MS });
    // 成功时不再改写状态：随后增强请求返回 "cancelled:已取消" 会给出最终提示
  } catch (e) {
    setStatus(`取消失败: ${errorText(e)}`, "err");
  }
}

/**
 * 限流后的自动重试定时器。
 * 存为模块级变量：新一轮增强开始时必须 clearTimeout，
 * 否则用户转做别的事后，这个定时器仍会突然发起请求。
 */
let rateLimitTimer = null;
function scheduleRateLimitRetry() {
  clearTimeout(rateLimitTimer);
  rateLimitTimer = setTimeout(() => {
    rateLimitTimer = null;
    enhance();
  }, RATE_LIMIT_RETRY_MS);
}

// 结构化错误渲染：错误串格式 `code:message`，按 code 给可操作按钮
function renderError(errStr) {
  const sep = errStr.indexOf(":");
  const code = sep > 0 ? errStr.slice(0, sep) : "";
  const msg = sep > 0 ? errStr.slice(sep + 1) : errStr;
  const el = $("statusText");
  el.className = "status err";
  el.textContent = msg; // 错误文本可能含用户配置内容 → textContent，不拼 HTML

  const actions = {
    unauthorized: { label: "打开设置改 Key", fn: () => openSettings() },
    // forbidden 有两个来源：上游 403（Key 权限不足）与服务端来源守卫（跨源请求被拒）。
    // 两种都指向「设置」，具体原因由后端 message 展示。
    forbidden: { label: "打开设置检查", fn: () => openSettings() },
    config: { label: "打开设置", fn: () => openSettings() },
    no_profile: { label: "打开设置", fn: () => openSettings() },
    timeout: { label: "重试", fn: () => enhance() },
    rate_limited: { label: "稍后再试", fn: () => scheduleRateLimitRetry() },
    server: { label: "重试", fn: () => enhance() },
    network: { label: "重试", fn: () => enhance() },
    empty_result: { label: "重试", fn: () => enhance() },
  };
  const act = actions[code];
  if (act) {
    const btn = document.createElement("button");
    btn.className = "ghost-btn btn-sm";
    btn.textContent = act.label;
    btn.style.marginLeft = "8px";
    btn.addEventListener("click", () => {
      btn.remove();
      act.fn();
    });
    el.appendChild(btn);
  }
}

/**
 * 复制结果到剪贴板。
 * 降级链：浏览器原生 API → 后端 /api/clipboard（Windows）→ 再试原生 → 提示手动复制。
 * 后端在非 Windows 或调用失败时会报错，因此后端失败必须能降级到原生 API。
 */
async function copyResult() {
  const text = $("outputBox").value;
  if (!text) return;
  const copyNative = async () => {
    if (!navigator.clipboard || !window.isSecureContext) {
      throw new ApiFail("clipboard", "原生剪贴板不可用");
    }
    await navigator.clipboard.writeText(text);
  };
  const copyBackend = async () => {
    await api("/api/clipboard", { method: "POST", body: { text }, timeoutMs: DEFAULT_API_TIMEOUT_MS });
  };
  try {
    await copyNative();
    setStatus("已复制到剪贴板", "ok");
    return;
  } catch (nativeErr) {
    try {
      await copyBackend();
      setStatus("已复制到剪贴板", "ok");
      return;
    } catch (backendErr) {
      // 后端也失败 → 再降级回原生 API（两条路都失败才提示用户手动复制）
      try {
        await copyNative();
        setStatus("已复制到剪贴板", "ok");
        return;
      } catch (e) {
        setStatus(`复制失败（${errorText(backendErr)}），请手动复制输出内容`, "err");
      }
    }
  }
}

function setStatus(msg, cls) {
  const el = $("statusText");
  el.textContent = msg;
  el.className = "status" + (cls ? " " + cls : "");
}

// ── 设置抽屉 ──────────────────────────
let lastFocusedEl = null;

function openSettings() {
  lastFocusedEl = document.activeElement;
  syncSettingsUI();
  $("settingsOverlay").hidden = false;
  // 焦点移入抽屉，方便键盘操作
  const closeBtn = $("closeSettings");
  if (closeBtn) closeBtn.focus();
}
function closeSettings() {
  $("settingsOverlay").hidden = true;
  $("saveHint").textContent = "";
  $("testResult").textContent = "";
  $("crStatus").textContent = "";
  renderProfileHints({});
  // 焦点归还给触发者
  if (lastFocusedEl && typeof lastFocusedEl.focus === "function") lastFocusedEl.focus();
  lastFocusedEl = null;
}

function syncSettingsUI() {
  renderProfileSelect();
  renderCustomModeSelect();
  renderCustomRoleSelect();
  $("optCloseTray").checked = config.close_to_tray;
  $("optAutostart").checked = config.autostart;
  loadProfileForm(config.active_index || 0);
  loadCustomModeForm(config.custom_modes?.[0]?.id || null);
  loadCustomRoleForm(config.custom_roles?.[0]?.id || null);
}

function renderProfileSelect() {
  const sel = $("profileSelect");
  sel.replaceChildren();
  (config.profiles || []).forEach((p, i) => {
    const opt = document.createElement("option");
    opt.value = i;
    opt.textContent = p.name || `配置 ${i + 1}`;
    sel.appendChild(opt);
  });
  sel.value = String(config.active_index || 0);
}

function loadProfileForm(idx) {
  const p = (config.profiles || [])[idx];
  $("pfName").value = p?.name || "";
  $("pfProtocol").value = p?.protocol || "openai";
  $("pfBaseUrl").value = p?.base_url || "";
  $("pfModel").value = p?.model || "";
  $("pfApiKey").value = p?.api_key || "";
  // f32 精度：0.7 存为 0.699999988079071，直接显示会泄露二进制误差 → 保留一位小数
  $("pfTemp").value = typeof p?.temperature === "number"
    ? Number(p.temperature.toFixed(1))
    : DEFAULT_TEMPERATURE;
  $("pfTimeout").value = Number.isFinite(p?.timeout_secs) ? p.timeout_secs : 0;
  $("pfMaxTokens").value = Number.isFinite(p?.max_tokens) ? p.max_tokens : 0;
  // 表单已初始化：此后 API Key 输入框为空才代表「用户主动清空」
  profileFormState.loaded = true;
  profileFormState.apiKeyDirty = false;
  renderProfileHints({});
  updateUrlParse();
}

/** 数字输入解析：空串按默认值处理，其余转 number */
function numOrDefault(raw, dflt) {
  if (raw === undefined || raw === null || String(raw).trim() === "") return dflt;
  const n = Number(raw);
  return Number.isFinite(n) ? n : dflt;
}

function collectProfileForm() {
  return {
    name: $("pfName").value.trim() || "未命名配置",
    protocol: $("pfProtocol").value,
    base_url: $("pfBaseUrl").value.trim(),
    model: $("pfModel").value.trim(),
    api_key: $("pfApiKey").value,
    temperature: numOrDefault($("pfTemp").value, DEFAULT_TEMPERATURE),
    timeout_secs: numOrDefault($("pfTimeout").value, 0),
    max_tokens: numOrDefault($("pfMaxTokens").value, 0),
  };
}

/**
 * 把「设置」面板中正在编辑的配置档表单写回内存中的 config.profiles[idx]。
 * 必须在 saveAll 提交 payload 之前调用，否则用户改过的内容会 100% 丢失。
 *
 * 保护条件：
 * - 设置面板必须已打开（未打开时表单是空的，不能拿空表单覆盖已有配置）
 * - 表单必须已被 config 初始化过
 * - 必须存在目标配置档
 * - API Key：仅当用户手动编辑过才覆盖，避免「从未加载」把已有 Key 清空
 *
 * @param {number} idx 目标配置档下标，缺省为当前选中项
 * @returns {boolean} 是否成功回写
 */
function commitProfileForm(idx = currentProfileIdx()) {
  if ($("settingsOverlay").hidden) return false;
  if (!profileFormState.loaded) return false;
  const p = (config.profiles || [])[idx];
  if (!p) return false;
  const form = collectProfileForm();
  p.name = form.name;
  p.protocol = form.protocol;
  p.base_url = form.base_url;
  p.model = form.model;
  p.temperature = form.temperature;
  p.timeout_secs = form.timeout_secs;
  p.max_tokens = form.max_tokens;
  if (profileFormState.apiKeyDirty) {
    p.api_key = form.api_key; // 空串 = 用户主动清空
  }
  return true;
}

function currentProfileIdx() {
  const idx = parseInt($("profileSelect").value, 10);
  return Number.isInteger(idx) ? idx : 0;
}

/**
 * 轻量校验「接口地址」是否合法（前端提示层，权威校验在后端）。
 * 规则与后端 core::endpoint::validate_base_url 对齐（判定顺序也一致）：
 * - 必须以 http:// 或 https:// 开头
 * - 不得包含 query / fragment（`?…` / `#…`）
 * - 不得包含 userinfo（user:pass@host）
 * - 主机名不得为空
 * - 不得包含空白与控制字符
 *
 * @param {string} raw
 * @returns {string|null} 错误文案，合法时返回 null
 */
function validateBaseUrl(raw) {
  const s = String(raw || "").trim();
  if (!s) return "接口地址不能为空";
  const sep = s.indexOf("://");
  if (sep <= 0) return "接口地址必须以 http:// 或 https:// 开头";
  const scheme = s.slice(0, sep).toLowerCase();
  if (scheme !== "http" && scheme !== "https") {
    return `接口地址协议必须是 http 或 https，当前：${scheme}`;
  }
  const rest = s.slice(sep + 3);
  // query / fragment：core 侧一律拒绝（base_url 契约不含 query / fragment）。
  //
  // 为什么必须拦：base_url 会**明文存进 config.json**。放行 `?key=xxx` 这类
  // query，等于提供一条「API Key 绕过 DPAPI 明文落盘」的路径，与本项目
  // 既有的安全模型（Key 走 DPAPI 加密 + Authorization 请求头）直接冲突。
  //
  // 判定只看「scheme 之后」的部分（与 core 同口径）：http/https 的 scheme
  // 里不可能出现 `?` / `#`，所以与「整串 includes」等价，但更精确。
  //
  // 文案与后端**逐字一致**（避免前后端说法不一），且**不回显 query 内容** ——
  // `?key=xxx` 里可能带着真实凭据，回显等于二次泄露。
  if (rest.includes("?") || rest.includes("#")) {
    return "接口地址不得包含查询参数或片段（?… / #…）；密钥请填写在 API Key 输入框";
  }
  if (/[\s\u0000-\u001f]/.test(s)) return "接口地址不能包含空格或控制字符";
  const hostport = rest.split("/")[0];
  if (!hostport) return "接口地址缺少主机名";
  if (hostport.includes("@")) {
    return "接口地址不得包含用户名/密码（user:pass@host），这类地址常被用于钓鱼";
  }
  return null;
}

/**
 * 校验配置档表单。
 *
 * 覆盖：
 * - protocol：必须是后端 LlmConfig::PROTOCOLS 中的枚举值
 * - base_url：轻量合法性校验（权威校验在后端 validate_base_url）
 * - model：四个协议都需要模型名
 * - temperature / timeout_secs / max_tokens：数值区间
 *
 * 说明：协议 / 地址 / 模型三项在「已存在配置档」**或「表单里已经填了配置档内容」**时校验 ——
 * - 已有档位：地址/模型是必填项，非法地址必须拦住（后端写入侧也有闸门）。
 * - 全新安装且用户已填地址/模型/Key：saveAll 会走「创建首个配置档」分支，
 *   此时**必须**按完整规则校验，否则非法地址会被写进配置。
 * - 全新安装且表单一个字段都没填：放行，允许只保存自定义角色/模式。
 *
 * @returns {{protocol?:string, baseUrl?:string, model?:string, temp?:string, timeout?:string, tokens?:string}}
 */
function validateProfileForm() {
  const errs = {};
  const hasProfile = (config?.profiles || []).length > 0;
  const baseUrlRaw = $("pfBaseUrl").value.trim();
  const modelRaw = $("pfModel").value.trim();
  const apiKeyRaw = $("pfApiKey").value;
  // 「有档位」或「表单里已经填了配置档内容」都要走完整校验。
  // 只有「一个字段都没填」才放行（允许只保存自定义角色/模式）。
  const shouldValidate = hasProfile || !!baseUrlRaw || !!modelRaw || !!apiKeyRaw;
  if (shouldValidate) {
    const protocol = $("pfProtocol").value;
    if (!ALLOWED_PROTOCOLS.includes(protocol)) {
      errs.protocol = `协议非法：${protocol}（可选：${ALLOWED_PROTOCOLS.join(" / ")}）`;
    }
    const baseUrl = $("pfBaseUrl").value.trim();
    const urlErr = validateBaseUrl(baseUrl);
    if (urlErr) {
      // 危险地址（userinfo / 非 http(s) / 缺主机名）→ 阻止保存，
      // 避免这类地址被写进配置（后端写入侧也有闸门，这里提前告知）
      errs.baseUrl = urlErr;
    }
    // 注意：解析失败（urlParseError）本身不阻断保存 ——
    // 后端 parse 的白名单可能比 validate_base_url 更严，
    // 若因解析失败就拦住保存，会出现「地址其实合法却存不下去」的死锁。
    // 解析失败原因由 renderProfileHints 以行内提示展示（见下）。
    if (!$("pfModel").value.trim()) {
      errs.model = "模型名不能为空（四个协议都需要指定模型）";
    }
  }
  const temp = numOrDefault($("pfTemp").value, DEFAULT_TEMPERATURE);
  if (temp < TEMPERATURE_MIN || temp > TEMPERATURE_MAX) {
    errs.temp = `Temperature 需在 ${TEMPERATURE_MIN} ~ ${TEMPERATURE_MAX} 之间`;
  }
  const timeout = numOrDefault($("pfTimeout").value, 0);
  if (timeout < TIMEOUT_SECS_MIN || timeout > TIMEOUT_SECS_MAX) {
    errs.timeout = `请求超时需在 ${TIMEOUT_SECS_MIN} ~ ${TIMEOUT_SECS_MAX} 秒之间`;
  }
  const tokens = numOrDefault($("pfMaxTokens").value, 0);
  if (tokens < MAX_TOKENS_MIN || tokens > MAX_TOKENS_MAX) {
    errs.tokens = `最大输出 Tokens 需在 ${MAX_TOKENS_MIN} ~ ${MAX_TOKENS_MAX} 之间`;
  }
  return errs;
}

/** 渲染行内校验提示（每个字段一条，出错时替换默认说明文字） */
function renderProfileHints(errs) {
  Object.keys(PROFILE_HINTS).forEach((key) => {
    const { hintId, inputId, def } = PROFILE_HINTS[key];
    const hint = $(hintId);
    const input = $(inputId);
    const err = errs[key];
    // 地址解析失败 → 在地址字段旁同步显示原因（解析区已有同样的可见提示）。
    // 这是「提示」而非「拦截项」：解析失败不阻断保存。
    let msg = err;
    if (!msg && key === "baseUrl" && urlParseError) {
      const cur = $("pfBaseUrl").value.trim();
      // urlParseError.message 已是完整可读文案，不再叠加前缀（避免出现
      // 「地址无法解析：地址无法解析：…」这种重复）
      if (urlParseError.value === cur) msg = urlParseError.message;
    }
    if (hint) {
      hint.textContent = msg || def;
      hint.style.color = msg ? "var(--danger)" : "";
    }
    // aria-invalid 只对「真正阻断保存」的校验项置位
    if (input) input.setAttribute("aria-invalid", err ? "true" : "false");
  });
}

async function saveAll() {
  // 提交前做表单校验（协议 / 地址 / 模型 / 数值区间），错误在字段旁行内提示
  const errs = validateProfileForm();
  renderProfileHints(errs);
  if (Object.keys(errs).length) {
    $("saveHint").textContent = "请先修正表单中标红的问题";
    $("saveHint").className = "status err";
    return;
  }

  config.close_to_tray = $("optCloseTray").checked;
  config.autostart = $("optAutostart").checked;
  // 增强选项一并保存（保证下次启动仍在）
  config.enhance = { ...enhanceOpts };
  // 选中档位与界面保持一致
  config.active_index = currentProfileIdx();
  // 保存前把表单中正在编辑的自定义模式 / 角色 / 配置档写回
  commitCustomModeForm();
  commitCustomRoleForm();
  // ← 关键：配置档表单回写，缺失这一步用户改过的 base_url / model 等会全部丢失
  if (!config.profiles || config.profiles.length === 0) {
    // ── 全新安装：表单内容就是「第一个配置档」──
    // 不做这一步的话，commitProfileForm 在空 profiles 里找不到目标档位直接
    // return false → 表单内容无处安放 → 落盘 profiles: []，
    // 而界面照样显示绿色「已保存」，用户输入 100% 丢失。
    const form = collectProfileForm();
    if (form.base_url && form.model) {
      config.profiles = [{
        name: form.name || "配置 1",
        protocol: form.protocol,
        base_url: form.base_url,
        model: form.model,
        // 首档的 Key 就是用户刚填的 Key（没有「旧档 Key 发往新主机」的风险）
        api_key: form.api_key,
        temperature: form.temperature,
        timeout_secs: form.timeout_secs,
        max_tokens: form.max_tokens,
      }];
      config.active_index = 0;
      renderProfileSelect();
      loadProfileForm(0);
    }
    // 表单完全为空（用户只想先保存自定义角色/模式）→ 不建档，正常保存
  } else if (!commitProfileForm(config.active_index)) {
    // 有档位但表单回写失败（表单未初始化 / 目标下标越界）→
    // 必须让用户知道，不能静默落盘一份「没包含用户改动」的旧配置。
    $("saveHint").textContent = "保存失败：配置档表单未能写回，请重新打开设置面板后重试";
    $("saveHint").className = "status err";
    return;
  }
  try {
    await queueConfigSave();
    $("saveHint").textContent = "已保存";
    $("saveHint").className = "status ok";
    await refreshModes();
    await refreshRoles();
    setTimeout(() => ($("saveHint").textContent = ""), SAVE_HINT_CLEAR_MS);
  } catch (e) {
    $("saveHint").textContent = `保存失败: ${errorText(e)}`;
    $("saveHint").className = "status err";
  }
}

// ── API 地址解析与回填 ─────────────────
// 粘贴完整 URL 后自动解析出 host / 端口 / 路径 / 接口路径，
// 并提供一键回填为基础地址（去掉接口后缀）。
let urlParseData = null;

/**
 * 从 `/api/parse-url` 的响应里判断「用户填的是完整接口端点」，并返回被识别出的接口后缀。
 *
 * 判据（**不在前端复制 core 的接口后缀表**，避免与后端规则漂移）：
 * 服务端返回的 `base_url` = origin + 「剥离接口后缀**后**的路径」，
 * 而 `path` 是剥离**前**的原始路径 → 两者路径部分不一致 ⇔ 命中了接口后缀。
 * origin 由响应里的 scheme/host/port 拼出，与服务端同构。
 *
 * @param {object} d `/api/parse-url` 的响应体
 * @returns {string|null} 被识别出的后缀（如 `/v1/messages`、`:generateContent`）；未识别到则 null
 */
function detectStrippedEndpointSuffix(d) {
  if (!d || !d.base_url || !d.path) return null;
  const origin = `${d.scheme || "https"}://${d.host || ""}${d.port ? `:${d.port}` : ""}`;
  if (!d.base_url.startsWith(origin)) return null;
  const basePath = d.base_url.slice(origin.length);
  const fullPath = String(d.path);
  const trimEnd = (p) => p.replace(/\/+$/, "");
  const kept = trimEnd(basePath);
  // 两边一致 → 服务端没有剥离任何后缀（这就是一个普通的基础地址）
  if (kept === trimEnd(fullPath)) return null;
  // 前缀关系不成立 → 无法可靠推断被剥掉的是哪一段，不臆测
  if (kept && !fullPath.startsWith(kept)) return null;
  const stripped = fullPath.slice(kept.length);
  return stripped ? stripped : null;
}

/**
 * 判断「被识别出的接口后缀」是否属于**所选协议**。
 *
 * ## 为什么必须判
 *
 * 服务端 `core::endpoint::parse_endpoint` 剥离后缀时用的是**四套后缀表的并集**
 * （endpoint.rs 里 OPENAI/ANTHROPIC/OLLAMA/GEMINI 串起来一起匹配），
 * **完全不看 protocol**；而真正出站时 `core::endpoint::resolve_endpoint`
 * → `resolve_openai_endpoint` 等**只按所选协议自己的后缀表**剥离
 * （`strip_endpoint_suffix` 明确「不跨协议乱剥」，core 里有专门的测试锁定）。
 *
 * 于是「协议选了 openai、地址粘的是 anthropic 的完整端点」时：
 *   解析阶段剥掉 `/v1/messages`（并集表命中）→ 前端以为「会自动去重」
 *   出站阶段 openai 表不认识 `/v1/messages` → **不剥** → 拼出
 *   `…/v1/messages/chat/completions` 这种**双语义拼接**的畸形 URL。
 *
 * ## 判据（不在前端复制 core 的接口后缀表，避免与后端规则漂移）
 *
 * 1. 后端若给出 `stripped_protocol`（权威答案，新增可选字段）→ 直接采信。
 * 2. 否则用**服务端按所选协议返回的 `suggested_endpoint`**（= 该协议的规范接口
 *    路径）做「同族」判定：被剥掉的后缀若是它的结尾 → 同协议；否则 → 异协议。
 *    - gemini 的规范端点含 `{model}` 占位（`/v1beta/models/{model}:generateContent`），
 *      所以只能用「结尾包含」而不是「最后一段相等」。
 *    - gemini 的方法名后缀（`:generateContent`）在 core 里是**大小写不敏感**的，
 *      这里同样大小写不敏感；`/` 开头的路径后缀在 core 里大小写敏感，这里同样敏感。
 *
 * ## 兜底方向
 *
 * 判不出来 → 返回 false（**不承诺**自动去重）。宁可少说一句提示，
 * 也不能对用户做出「会自动去重」这种**假承诺** —— 假承诺会让用户以为
 * 粘贴完整端点万事大吉，实际出站 URL 是畸形的。
 *
 * @param {object} d `/api/parse-url` 的响应体
 * @param {string} strippedSuffix 被识别出的接口后缀（如 `/v1/messages`）
 * @param {string} protocol 当前所选协议
 * @returns {boolean} true = 该后缀属于所选协议（出站会按该协议去重）
 */
function endpointSuffixBelongsToProtocol(d, strippedSuffix, protocol) {
  if (!d || !strippedSuffix) return false;
  // ① 后端权威答案（可选字段，缺失时走 ②）
  const owner = typeof d.stripped_protocol === "string" ? d.stripped_protocol : "";
  if (owner) return owner === protocol;
  // ② 用该协议的规范端点做「同族」判定
  const suggested = String(d.suggested_endpoint || "");
  if (!suggested) return false;
  // Gemini 方法名后缀（`:` 开头）大小写不敏感；路径后缀（`/` 开头）大小写敏感
  if (strippedSuffix.startsWith(":")) {
    return suggested.toLowerCase().endsWith(strippedSuffix.toLowerCase());
  }
  return suggested.endsWith(strippedSuffix);
}

/**
 * 最近一次解析失败的信息：{ value: 原始输入, message: 可读错误 }。
 * value 用于判断「这条错误是否仍对应当前输入框内容」——
 * 用户改了输入后旧错误即失效，不应再拦住保存。
 */
let urlParseError = null;

/** 构造一个 URL 解析标签（标签与值都来自用户输入的 URL → 一律 textContent） */
function makeUrlTag(label, value, extraClass) {
  const span = document.createElement("span");
  span.className = extraClass ? `url-tag ${extraClass}` : "url-tag";
  const l = document.createElement("span");
  l.className = "url-tag-label";
  l.textContent = label;
  const b = document.createElement("b");
  b.textContent = value === undefined || value === null ? "" : String(value);
  span.appendChild(l);
  span.appendChild(b);
  return span;
}

/** 在解析区展示错误（可见提示，不静默吞掉） */
function showUrlParseError(message) {
  const box = $("urlParse");
  box.replaceChildren();
  const note = document.createElement("span");
  note.className = "url-tag url-note";
  // 错误文本可能含用户输入的地址 → 一律 textContent，不拼 HTML
  note.textContent = message;
  note.style.color = "var(--danger)";
  box.appendChild(note);
  box.hidden = false;
}

async function updateUrlParse() {
  const raw = $("pfBaseUrl").value.trim();
  const box = $("urlParse");
  if (!raw) {
    box.hidden = true;
    box.replaceChildren();
    urlParseData = null;
    urlParseError = null;
    renderProfileHints({});
    return;
  }
  // 竞态守卫：只有最后一次解析的结果可以写回界面
  // （连续粘贴两个地址时，先发的请求可能后返回）
  const seq = ++urlParseSeq;
  try {
    const d = await api("/api/parse-url", {
      method: "POST",
      body: { url: raw, protocol: $("pfProtocol").value },
      timeoutMs: DEFAULT_API_TIMEOUT_MS,
    });
    if (seq !== urlParseSeq) return; // 已有更新的解析请求 → 丢弃本次结果
    urlParseData = d;
    urlParseError = null;
    box.replaceChildren();
    box.appendChild(makeUrlTag("协议", d.scheme));
    box.appendChild(makeUrlTag("主机", d.host));
    if (d.port) box.appendChild(makeUrlTag("端口", d.port));
    box.appendChild(makeUrlTag("路径", d.path));
    if (d.suggested_endpoint) box.appendChild(makeUrlTag("接口", d.suggested_endpoint));
    (d.notes || []).forEach((n) => {
      const note = document.createElement("span");
      note.className = "url-tag url-note";
      note.textContent = String(n);
      box.appendChild(note);
    });
    // ── 识别到接口后缀 → 按「该后缀是否属于所选协议」分叉提示 ──
    // 服务端出站前用 core::endpoint::resolve_endpoint 归一出站 URL，
    // 它**只按所选协议自己的后缀表**剥离（不跨协议乱剥）。
    // 因此只有「被识别的后缀属于所选协议」时，出站才会真的去重；
    // 跨协议时若照旧说「会自动去重」就是对用户的**假承诺**（出站会拼出双语义 URL）。
    // 这里只做提示：base_url 仍按用户原样保存，是否回填由用户自己决定。
    const strippedSuffix = detectStrippedEndpointSuffix(d);
    if (strippedSuffix) {
      const note = document.createElement("span");
      note.className = "url-tag url-note";
      const canFill = Boolean(d.base_url) && d.base_url !== raw;
      const sameProto = endpointSuffixBelongsToProtocol(d, strippedSuffix, $("pfProtocol").value);
      if (sameProto) {
        // 同协议：出站确实会按该协议去重 → 承诺成立
        note.textContent = canFill
          ? "提示：出站请求会按所选协议自动去重；如需手动控制，可点「回填基础地址」。"
          : "提示：出站请求会按所选协议自动去重。";
      } else {
        // 跨协议：出站**不会**去重 → 明确告知，避免假承诺
        note.textContent = canFill
          ? "提示：识别到的接口后缀不属于当前所选协议；出站不会自动去重，直接保存可能拼出错误地址。建议改协议，或点「回填基础地址」只保留基础地址。"
          : "提示：识别到的接口后缀不属于当前所选协议；出站不会自动去重，直接保存可能拼出错误地址。建议改协议。";
      }
      box.appendChild(note);
    }
    // 回填按钮：仅当解析出的基础地址与当前输入不同才显示
    if (d.base_url && d.base_url !== raw) {
      const fill = document.createElement("button");
      fill.type = "button";
      fill.className = "url-fill";
      fill.id = "urlFillBtn";
      fill.textContent = "回填基础地址";
      box.appendChild(fill);
    }
    box.hidden = false;
  } catch (e) {
    // ── 解析失败不再静默吞掉 ──
    // 后端会给出可读原因（如 `config:无法解析该地址，请检查是否包含主机名`），
    // 这里直接展示给用户；用户已填的内容保持原样，不清空表单。
    if (seq !== urlParseSeq) return; // 已有更新的解析请求 → 丢弃本次结果
    urlParseData = null;
    const msg = errorText(e);
    const sep = msg.indexOf(":");
    const code = sep > 0 ? msg.slice(0, sep) : "";
    // 后端错误串是 `code:message`，展示时只保留可读部分
    const readable = sep > 0 ? msg.slice(sep + 1) : msg;
    // 按错误类别给出不同的可读提示，避免把「服务不可用」说成「地址写错了」
    let tip;
    if (code === "timeout" || code === "network") {
      tip = `地址解析服务暂时不可用（${readable}），请稍后重试或手动填写地址`;
    } else {
      // 后端 message 本身已是完整可读文案（如「无法解析该地址，请检查是否包含主机名」），
      // 不再叠加前缀，避免出现「地址无法解析：无法解析该地址…」的重复措辞。
      tip = readable;
    }
    urlParseError = { value: raw, message: tip };
    showUrlParseError(tip);
    renderProfileHints({});
  }
}

// ── 自定义模式管理 ────────────────────
let cmEditingId = null; // 正在编辑的自定义模式 id（null = 未进入编辑）

/** 自定义项为空时补一个不可选占位项：空 select 展开只有一个极小的空白面板 */
function appendEmptyOption(sel, text) {
  const opt = document.createElement("option");
  opt.value = "";
  opt.textContent = text;
  opt.disabled = true;
  opt.selected = true;
  sel.appendChild(opt);
}

function renderCustomModeSelect() {
  const sel = $("cmSelect");
  sel.replaceChildren();
  (config.custom_modes || []).forEach((m) => {
    const opt = document.createElement("option");
    opt.value = m.id;
    opt.textContent = `${m.name} (${m.id})`;
    sel.appendChild(opt);
  });
  if (!sel.options.length) appendEmptyOption(sel, "暂无自定义模式 · 点「新建」创建");
  if (cmEditingId && config.custom_modes?.some((m) => m.id === cmEditingId)) {
    sel.value = cmEditingId;
  }
}

function loadCustomModeForm(id) {
  const m = (config.custom_modes || []).find((x) => x.id === id);
  cmEditingId = m ? id : null;
  $("cmId").value = m?.id || "";
  $("cmName").value = m?.name || "";
  $("cmType").value = m?.type || "extend";
  $("cmTemplate").value = m?.template || "";
}

// 把表单当前值写回 config.custom_modes（保存前调用）
function commitCustomModeForm() {
  if (cmEditingId === null) return;
  const id = $("cmId").value.trim();
  if (!id) return; // 空 id 不提交，交给后端校验报错
  const m = {
    id,
    name: $("cmName").value.trim() || id,
    type: $("cmType").value,
    template: $("cmTemplate").value,
  };
  config.custom_modes = config.custom_modes || [];
  const idx = config.custom_modes.findIndex((x) => x.id === cmEditingId);
  if (idx >= 0) {
    // id 改名时防重
    if (config.custom_modes.some((x, i) => i !== idx && x.id === id)) return;
    config.custom_modes[idx] = m;
  } else {
    config.custom_modes.push(m);
  }
}

// ── 自定义角色管理 ────────────────────
let crEditingId = null; // 正在编辑的自定义角色 id（null = 新建）

function renderCustomRoleSelect() {
  const sel = $("crSelect");
  sel.replaceChildren();
  (config.custom_roles || []).forEach((r) => {
    const opt = document.createElement("option");
    opt.value = r.id;
    opt.textContent = `${r.name} (${r.id})`;
    sel.appendChild(opt);
  });
  if (!sel.options.length) appendEmptyOption(sel, "暂无自定义角色 · 点「新建」创建");
  if (crEditingId && config.custom_roles?.some((r) => r.id === crEditingId)) {
    sel.value = crEditingId;
  }
}

function loadCustomRoleForm(id) {
  const r = (config.custom_roles || []).find((x) => x.id === id);
  crEditingId = r ? id : null;
  $("crId").value = r?.id || "";
  $("crName").value = r?.name || "";
  $("crPrompt").value = r?.system_prompt || "";
}

/** 把表单当前值写回 config.custom_roles */
function commitCustomRoleForm() {
  if (crEditingId === null) return;
  const id = $("crId").value.trim();
  if (!id) return;
  const r = {
    id,
    name: $("crName").value.trim() || id,
    system_prompt: $("crPrompt").value,
  };
  config.custom_roles = config.custom_roles || [];
  const idx = config.custom_roles.findIndex((x) => x.id === crEditingId);
  if (idx >= 0) {
    if (config.custom_roles.some((x, i) => i !== idx && x.id === id)) return;
    config.custom_roles[idx] = r;
    // 角色 id 改名后，生效角色引用必须同步更新，
    // 否则 enhanceOpts.role_id 仍指向旧 id，后端会因角色不存在而拒绝请求。
    if (enhanceOpts.role_id === `custom-${crEditingId}` && id !== crEditingId) {
      enhanceOpts.role_id = `custom-${id}`;
      renderRoles();
      updateRoleChip();
      updateSummary();
    }
  } else {
    config.custom_roles.push(r);
  }
}

/**
 * 测试连接。
 * 用自增序号守卫：后发先至的旧响应不得覆盖新状态。
 */
async function testConnection() {
  const errs = validateProfileForm();
  renderProfileHints(errs);
  if (Object.keys(errs).length) {
    $("testResult").textContent = "请先修正表单中标红的问题";
    $("testResult").className = "status err";
    return;
  }
  const profile = collectProfileForm();
  if (!profile.base_url || !profile.model) {
    $("testResult").textContent = "请先填写 API 地址和模型";
    $("testResult").className = "status err";
    return;
  }
  const seq = ++testSeq;
  $("testBtn").disabled = true;
  $("testResult").textContent = "测试中…";
  $("testResult").className = "status";
  try {
    await api("/api/test", {
      method: "POST",
      body: { profile },
      // /api/test 同样走完整的 llm::enhance 重试链（内部共享 deadline = T*6+30），
      // 因此用与 /api/enhance 相同的动态门槛，前端等待必须大于服务端预算。
      timeoutMs: enhanceTimeoutMs(profile.timeout_secs),
    });
    if (seq !== testSeq) return; // 已有更新的测试请求 → 丢弃本次结果
    $("testResult").textContent = "连接成功";
    $("testResult").className = "status ok";
  } catch (e) {
    if (seq !== testSeq) return;
    $("testResult").textContent = `失败: ${errorText(e)}`;
    $("testResult").className = "status err";
  } finally {
    if (seq === testSeq) $("testBtn").disabled = false;
  }
}

// ── 事件绑定 ──────────────────────────
$("enhanceBtn").addEventListener("click", enhance);
$("copyBtn").addEventListener("click", copyResult);
$("inputBox").addEventListener("keydown", (e) => {
  if ((e.ctrlKey || e.metaKey) && e.key === "Enter") enhance();
});
$("inputBox").addEventListener("input", () => {
  $("inputCounter").textContent = `${$("inputBox").value.length} 字`;
});

$("modeSelect").addEventListener("change", updateSummary);
$("methodSelect").addEventListener("change", () => {
  enhanceOpts.method_id = $("methodSelect").value;
  updateSummary();
  persistEnhanceOpts();
});
$("roleSelect").addEventListener("change", () => {
  enhanceOpts.role_id = $("roleSelect").value;
  updateRoleChip();
  updateSummary();
  persistEnhanceOpts();
});
// 顶栏角色标识 → 直接切到角色管理页
function openRoleManager() {
  openSettings();
  switchTab("role");
}
$("roleChip").addEventListener("click", openRoleManager);
// 键盘可达：roleChip 是 div，需自行处理 Enter / Space
$("roleChip").addEventListener("keydown", (e) => {
  if (e.key === "Enter" || e.key === " " || e.key === "Spacebar") {
    e.preventDefault();
    openRoleManager();
  }
});

$("settingsBtn").addEventListener("click", openSettings);
$("closeSettings").addEventListener("click", closeSettings);
$("settingsOverlay").addEventListener("click", (e) => {
  if (e.target === e.currentTarget) closeSettings();
});

// 标签页切换
function switchTab(name) {
  document.querySelectorAll("#tabs .tab").forEach((t) => {
    t.classList.toggle("active", t.dataset.tab === name);
  });
  document.querySelectorAll(".tab-pane").forEach((p) => {
    p.classList.toggle("active", p.dataset.pane === name);
  });
}
$("tabs").addEventListener("click", (e) => {
  const tab = e.target.closest(".tab");
  if (tab) switchTab(tab.dataset.tab);
});

/**
 * 切换配置档。
 *
 * 双保险之二：立即把 active_index 写回 config 并落盘，
 * 这样即使后端暂不读请求体的 profile_index，磁盘上的 active_index 也已与界面一致。
 * （后端已支持 profile_index 时，请求体那一路即可生效，两路互为兜底。）
 */
$("profileSelect").addEventListener("change", () => {
  const prevIdx = config.active_index || 0;
  // 旧档位未保存的编辑先写回内存，避免切换后丢失。
  // 这里不做校验拦截 —— 校验只影响「保存」，不丢弃用户已输入的内容；
  // 真正的拦截发生在 saveAll（会在字段旁给出行内提示）。
  commitProfileForm(prevIdx);
  config.active_index = currentProfileIdx();
  loadProfileForm(config.active_index);
  // 切换档位时清掉上一个档位的残留状态（测试结果 / 提示 / 校验提示）
  $("testResult").textContent = "";
  $("testResult").className = "status";
  $("saveHint").textContent = "";
  // 立即落盘 active_index（失败不阻断界面，下次保存会重试）
  queueConfigSave().catch(() => {});
});

/**
 * 增删配置档后必须把配置**立即落盘**。
 *
 * 原因：服务端 /api/enhance 用请求体的 profile_index 去索引**磁盘**配置里的
 * profiles。若前端只在内存里增删档位
 * 而不落盘，磁盘与内存的 profiles 数组就会错位 →
 * profile_index 命中的可能是用户已经删掉的档位 → 把该档的明文 API Key
 * 发往它自己的服务方（真实凭据泄露）。
 *
 * 落盘失败时必须回滚内存变更并提示用户，
 * 不能留下「内存已改、磁盘未改」的分裂状态。
 */
$("newProfile").addEventListener("click", async () => {
  config.profiles = config.profiles || [];
  // 此前有没有档位 → 决定新档是否继承 API Key。
  // 已有档位：清空 Key（防用户改完地址后把旧 Key 发到新主机）。
  // 全新安装：这是**首个**档位，用户刚填的 Key 就是它自己的 Key，
  //           不存在「旧 Key 发往新主机」的风险 → 必须保留。
  const hadProfiles = config.profiles.length > 0;
  // 新建档位 = 复制当前表单的连接参数，但**不复制 API Key**：
  // 复制 Key 会让用户改完地址后把旧 Key 发到新主机。
  // 其余字段（协议/地址/模型/数值）沿用当前表单，方便在此基础上改。
  const base = collectProfileForm();
  // 落盘前先做轻量校验：新档位必须带一个能通过后端写入侧闸门的接口地址
  // （后端 save_config 逐档调 validate_base_url，地址非法/为空会整份保存失败）。
  // 全新安装时表单是空的 → 这里给出可读提示，而不是先建档再保存失败回滚。
  const urlErr = validateBaseUrl(base.base_url);
  if (urlErr) {
    renderProfileHints({ baseUrl: urlErr });
    $("saveHint").textContent = `无法新增配置档：${urlErr}`;
    $("saveHint").className = "status err";
    return;
  }
  // 校验通过后再把当前表单写回正在编辑的档位，
  // 避免「新增」动作把用户对旧档的编辑冲掉。
  commitProfileForm(currentProfileIdx());
  const snapshot = {
    profiles: config.profiles.map((p) => ({ ...p })),
    active_index: config.active_index,
  };
  config.profiles.push({
    ...base,
    name: `配置 ${config.profiles.length + 1}`,
    // 已有档位 → 新档继承的是「旧档地址」语境，清空 Key 防跨主机泄露；
    // 全新安装 → 这是首个档位，用户刚填的 Key 就是它的 Key，必须保留。
    api_key: hadProfiles ? "" : base.api_key,
  });
  config.active_index = config.profiles.length - 1;
  renderProfileSelect();
  loadProfileForm(config.active_index);
  $("testResult").textContent = "";
  $("saveHint").textContent = "";
  renderProfileHints({});
  // 立即落盘：失败则回滚内存变更（否则磁盘与内存 profiles 数组错位）
  try {
    await queueConfigSave();
  } catch (e) {
    config.profiles = snapshot.profiles;
    config.active_index = snapshot.active_index;
    renderProfileSelect();
    loadProfileForm(config.active_index);
    $("saveHint").textContent = `新增配置档失败，已撤销: ${errorText(e)}`;
    $("saveHint").className = "status err";
  }
});

$("delProfile").addEventListener("click", async () => {
  if (!config.profiles?.length) return;
  const delIdx = currentProfileIdx();
  const delName = config.profiles[delIdx]?.name || `配置 ${delIdx + 1}`;
  // 删除会立即落盘，不可撤销 → 先确认
  if (!window.confirm(`确定删除配置档「${delName}」吗？该操作会立即保存。`)) return;
  const snapshot = {
    profiles: config.profiles.map((p) => ({ ...p })),
    active_index: config.active_index,
  };
  config.profiles.splice(delIdx, 1);
  // active_index 必须仍指向一个存在的档位（越界会被 AppConfig::validate 拒绝 → 保存失败）
  if (config.profiles.length === 0) {
    config.active_index = 0; // 空列表 + index 0 = 未配置态（后端视为合法）
  } else if (delIdx === snapshot.active_index) {
    config.active_index = Math.min(delIdx, config.profiles.length - 1);
  } else if (config.active_index > delIdx) {
    config.active_index -= 1; // 删的是前面的档位 → 后面档位下标前移
  }
  renderProfileSelect();
  loadProfileForm(config.active_index);
  $("testResult").textContent = "";
  renderProfileHints({});
  // 立即落盘：失败则回滚内存变更
  try {
    await queueConfigSave();
    $("saveHint").textContent = "已删除配置档并保存";
    $("saveHint").className = "status ok";
  } catch (e) {
    config.profiles = snapshot.profiles;
    config.active_index = snapshot.active_index;
    renderProfileSelect();
    loadProfileForm(config.active_index);
    $("saveHint").textContent = `删除配置档失败，已撤销: ${errorText(e)}`;
    $("saveHint").className = "status err";
  }
});

// API 地址：输入即解析（防抖）
let urlTimer = null;
$("pfBaseUrl").addEventListener("input", () => {
  clearTimeout(urlTimer);
  urlTimer = setTimeout(updateUrlParse, URL_PARSE_DEBOUNCE_MS);
});
$("pfBaseUrl").addEventListener("paste", () => {
  // 粘贴后立即解析（不等 input 防抖）。
  // 必须复用同一个 urlTimer：否则粘贴的解析与后续 input 防抖是两个独立定时器，
  // 旧结果可能后到并覆盖新结果（连续粘贴两个地址时会写回错误的地址）。
  clearTimeout(urlTimer);
  urlTimer = setTimeout(updateUrlParse, URL_PARSE_PASTE_DELAY_MS);
});
$("pfProtocol").addEventListener("change", updateUrlParse);

// URL 解析结果里的「回填基础地址」按钮：事件委托，避免每次渲染重复绑定
$("urlParse").addEventListener("click", (e) => {
  const btn = e.target.closest("#urlFillBtn");
  if (!btn || !urlParseData) return;
  $("pfBaseUrl").value = urlParseData.normalized || urlParseData.base_url || "";
  updateUrlParse();
});

// 深度思考强度：事件委托（渲染时不再逐个 addEventListener）
$("thinkingSeg").addEventListener("click", (e) => {
  const b = e.target.closest("button[data-value]");
  if (!b) return;
  enhanceOpts.thinking = b.dataset.value;
  renderThinking();
  updateSummary();
  persistEnhanceOpts();
});

// API Key 被手动编辑过 → 允许 commitProfileForm 覆盖已有 Key（含主动清空）
$("pfApiKey").addEventListener("input", () => {
  profileFormState.apiKeyDirty = true;
});

$("cmSelect").addEventListener("change", () => {
  commitCustomModeForm();
  renderCustomModeSelect();
  loadCustomModeForm($("cmSelect").value);
});
$("cmNew").addEventListener("click", () => {
  commitCustomModeForm();
  cmEditingId = null;
  $("cmId").value = "";
  $("cmName").value = "";
  $("cmType").value = "extend";
  $("cmTemplate").value = "";
  $("cmId").focus();
});
$("cmDel").addEventListener("click", () => {
  if (!cmEditingId) {
    if ($("cmSelect").value) loadCustomModeForm($("cmSelect").value);
    return;
  }
  config.custom_modes = (config.custom_modes || []).filter((x) => x.id !== cmEditingId);
  cmEditingId = null;
  renderCustomModeSelect();
  loadCustomModeForm(config.custom_modes?.[0]?.id || null);
});

$("crSelect").addEventListener("change", () => {
  commitCustomRoleForm();
  renderCustomRoleSelect();
  loadCustomRoleForm($("crSelect").value);
});
$("crNew").addEventListener("click", () => {
  commitCustomRoleForm();
  crEditingId = null;
  $("crId").value = "";
  $("crName").value = "";
  $("crPrompt").value = "";
  $("crId").focus();
  $("crStatus").textContent = "填写后点「保存全部」即新建角色";
});
$("crDel").addEventListener("click", () => {
  if (!crEditingId) return;
  config.custom_roles = (config.custom_roles || []).filter((x) => x.id !== crEditingId);
  // 若删掉的正是当前生效角色，回落到默认
  if (enhanceOpts.role_id === `custom-${crEditingId}`) {
    enhanceOpts.role_id = "builtin-general";
  }
  crEditingId = null;
  renderCustomRoleSelect();
  loadCustomRoleForm(config.custom_roles?.[0]?.id || null);
});
$("crApply").addEventListener("click", () => {
  commitCustomRoleForm();
  const id = $("crId").value.trim();
  if (!id) {
    $("crStatus").textContent = "请先填写角色 ID";
    $("crStatus").className = "status err";
    return;
  }
  enhanceOpts.role_id = `custom-${id}`;
  renderRoles();
  updateRoleChip();
  updateSummary();
  $("crStatus").textContent = "已设为生效角色，点「保存全部」持久化";
  $("crStatus").className = "status ok";
  persistEnhanceOpts();
});

$("testBtn").addEventListener("click", testConnection);
$("saveSettings").addEventListener("click", saveAll);

// ── 历史记录 ──────────────────────────
$("historyBtn").addEventListener("click", async () => {
  const panel = $("historyPanel");
  if (!panel.hidden) {
    panel.hidden = true;
    return;
  }
  try {
    const hist = (await api("/api/history", { timeoutMs: DEFAULT_API_TIMEOUT_MS })).history || [];
    // 与后端 HISTORY_CAP 对齐，避免展示超出上限的条目
    historyItems = hist.slice(0, HISTORY_CAP);
    panel.replaceChildren();
    if (!historyItems.length) {
      const empty = document.createElement("div");
      empty.className = "history-empty";
      empty.textContent = "暂无历史记录";
      panel.appendChild(empty);
    }
    historyItems.forEach((h, i) => {
      const row = document.createElement("div");
      row.className = "history-row";
      row.dataset.idx = String(i);
      const tag = document.createElement("span");
      tag.className = "history-tag";
      tag.textContent = h.mode_id || ""; // 模式 id 来自用户配置 → textContent
      row.appendChild(tag);
      row.appendChild(document.createTextNode(h.input || ""));
      row.title = h.output || "";
      panel.appendChild(row);
    });
    panel.hidden = false;
  } catch (e) {
    setStatus(`历史读取失败: ${errorText(e)}`, "err");
  }
});

// 历史行点击：事件委托（列表每次重绘，重复绑定会导致一次点击触发多次）
$("historyPanel").addEventListener("click", (e) => {
  const row = e.target.closest(".history-row");
  if (!row) return;
  const h = historyItems[Number(row.dataset.idx)];
  if (!h) return;
  $("inputBox").value = h.input || "";
  $("outputBox").value = h.output || "";
  $("inputCounter").textContent = `${(h.input || "").length} 字`;
  $("outputCounter").hidden = false;
  $("outputCounter").textContent = `${(h.output || "").length} 字`;
  $("copyBtn").disabled = false;
  // 模式下拉回选（存在才选）
  const sel = $("modeSelect");
  if ([...sel.options].some((o) => o.value === h.mode_id)) sel.value = h.mode_id;
  updateSummary();
  $("historyPanel").hidden = true;
  setStatus("已回填历史记录", "ok");
});

/** 模态框内可聚焦元素 */
const FOCUSABLE_SELECTOR =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

// Esc 关闭设置面板 + Tab 焦点陷阱
window.addEventListener("keydown", (e) => {
  const overlay = $("settingsOverlay");
  if (overlay.hidden) return;
  if (e.key === "Escape") {
    closeSettings();
    return;
  }
  if (e.key !== "Tab") return;
  const items = [...overlay.querySelectorAll(FOCUSABLE_SELECTOR)].filter(
    (el) => el.offsetParent !== null || el === document.activeElement
  );
  if (!items.length) return;
  const first = items[0];
  const last = items[items.length - 1];
  if (e.shiftKey && document.activeElement === first) {
    e.preventDefault();
    last.focus();
  } else if (!e.shiftKey && document.activeElement === last) {
    e.preventDefault();
    first.focus();
  }
});

init();
