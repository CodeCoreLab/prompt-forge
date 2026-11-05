// Prompt Forge 前端逻辑：纯原生 JS，无框架依赖
//
// 通信方式：HTTP + JSON API（后端为 Rust HTTP 服务，非 Electron IPC）。
// 错误语义与旧版完全一致：错误串格式 `code:message`，由 renderError 按 code 渲染动作。

const $ = (id) => document.getElementById(id);

/**
 * 调用后端 API。
 * 成功返回业务字段；失败抛出 `code:message` 形态的错误串（与旧版 invoke 行为一致）。
 */
async function api(path, { method = "GET", body } = {}) {
  const init = { method, headers: {} };
  if (body !== undefined) {
    init.headers["Content-Type"] = "application/json";
    init.body = JSON.stringify(body);
  }
  const resp = await fetch(path, init);
  let data;
  try {
    data = await resp.json();
  } catch (e) {
    throw `http:响应解析失败 (HTTP ${resp.status})`;
  }
  if (!resp.ok || data.ok === false) {
    throw data.error || `http:请求失败 (HTTP ${resp.status})`;
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
    methods = mt.methods || [];
    thinkingLevels = mt.thinking_levels || [];
    roles = rl.roles || [];
    // 恢复已保存的增强选项（持久化 → 重开仍在）
    enhanceOpts = {
      method_id: config.enhance?.method_id || mt.default_method_id || "general",
      thinking: config.enhance?.thinking || mt.default_thinking || "standard",
      role_id: config.enhance?.role_id || "builtin-general",
    };
  } catch (e) {
    setStatus(`初始化失败: ${e}`, "err");
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
    renderRoles();
    updateRoleChip();
  } catch (e) {
    // 刷新失败不阻断保存流程
  }
}

// ── 增强模式 ──────────────────────────
function renderModes() {
  const sel = $("modeSelect");
  sel.innerHTML = "";
  modes.forEach((m) => {
    const opt = document.createElement("option");
    opt.value = m.id;
    opt.textContent = m.name;
    opt.title = m.description;
    sel.appendChild(opt);
  });
}

// ── 增强方式 ──────────────────────────
function renderMethods() {
  const sel = $("methodSelect");
  sel.innerHTML = "";
  methods.forEach((m) => {
    const opt = document.createElement("option");
    opt.value = m.id;
    opt.textContent = m.name;
    opt.title = m.description;
    sel.appendChild(opt);
  });
  sel.value = enhanceOpts.method_id;
}

// ── 深度思考强度（分段控件）────────────
function renderThinking() {
  const seg = $("thinkingSeg");
  seg.innerHTML = "";
  thinkingLevels.forEach((l) => {
    const b = document.createElement("button");
    b.type = "button";
    b.setAttribute("role", "radio");
    b.dataset.value = l.id;
    b.textContent = l.name;
    b.title = l.description + (l.budget_tokens ? `（推理预算 ${l.budget_tokens} tokens）` : "");
    if (l.id === enhanceOpts.thinking) {
      b.classList.add("active");
      b.setAttribute("aria-checked", "true");
    }
    b.addEventListener("click", () => {
      enhanceOpts.thinking = l.id;
      renderThinking();
      updateSummary();
      persistEnhanceOpts();
    });
    seg.appendChild(b);
  });
}

// ── 角色 ──────────────────────────────
function renderRoles() {
  const sel = $("roleSelect");
  const prev = enhanceOpts.role_id;
  sel.innerHTML = "";
  roles.forEach((r) => {
    const opt = document.createElement("option");
    opt.value = r.id;
    opt.textContent = r.builtin ? r.name : `${r.name}（自定义）`;
    opt.title = r.description;
    sel.appendChild(opt);
  });
  sel.value = prev;
}

/** 更新顶栏「生效角色」标识 */
function updateRoleChip() {
  const r = roles.find((x) => x.id === enhanceOpts.role_id);
  const name = r ? r.name : "通用助手";
  $("roleChipName").textContent = name;
  $("roleChip").title = r ? r.description : "当前生效角色";
}

/** 底部配置摘要：把当前组合说清楚，避免用户"不知道自己在用什么" */
function updateSummary() {
  const mode = modes.find((m) => m.id === $("modeSelect").value);
  const method = methods.find((m) => m.id === enhanceOpts.method_id);
  const think = thinkingLevels.find((l) => l.id === enhanceOpts.thinking);
  const role = roles.find((r) => r.id === enhanceOpts.role_id);
  const parts = [
    mode ? `模式 <b>${mode.name}</b>` : "",
    method ? `方式 <b>${method.name}</b>` : "",
    think ? `思考 <b>${think.name}</b>` : "",
    role ? `角色 <b>${role.name}</b>` : "",
  ].filter(Boolean);
  $("configSummary").innerHTML =
    `当前组合：${parts.join(" · ")}${think && think.budget_tokens ? ` · 推理预算 ${think.budget_tokens} tokens` : ""}`;
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
      await api("/api/config", { method: "POST", body: { config } });
    } catch (e) {
      // 静默：下次保存时会一并写入
    }
  }, 320);
}

// ── 增强 ──────────────────────────────
async function enhance() {
  const input = $("inputBox").value.trim();
  if (!input) { setStatus("请先输入提示词", "err"); return; }
  if (!config.profiles || config.profiles.length === 0) {
    setStatus("尚未配置模型，请先打开设置", "err");
    openSettings();
    return;
  }

  const btn = $("enhanceBtn");
  if (btn.dataset.canceling === "1") {
    // 按钮处于「取消」态 → 本次点击是取消请求
    try { await api("/api/cancel", { method: "POST" }); } catch (e) { /* 忽略 */ }
    return;
  }
  btn.dataset.canceling = "1";
  btn.querySelector(".btn-label").textContent = "取消增强";
  btn.classList.add("canceling");
  $("outputBox").value = "";
  $("outputCounter").hidden = true;
  $("copyBtn").disabled = true;
  const statusEl = $("statusText");
  statusEl.className = "status busy";
  statusEl.innerHTML = '<span class="spin"></span>增强中…';

  try {
    const result = await api("/api/enhance", {
      method: "POST",
      body: {
        input,
        mode_id: $("modeSelect").value,
        method_id: enhanceOpts.method_id,
        thinking: enhanceOpts.thinking,
        role_id: enhanceOpts.role_id,
      },
    });
    $("outputBox").value = result.output;
    $("copyBtn").disabled = false;
    const counter = $("outputCounter");
    counter.hidden = false;
    counter.textContent = `${result.output.length} 字`;
    const ap = result.applied || {};
    statusEl.className = "status ok";
    statusEl.textContent = `增强完成 · ${ap.role_name || ""}`;
  } catch (e) {
    statusEl.className = "";
    const s = String(e);
    if (s === "cancelled:已取消") {
      setStatus("已取消", "");
    } else {
      renderError(s);
    }
  } finally {
    btn.querySelector(".btn-label").textContent = "开始增强";
    btn.classList.remove("canceling");
    btn.dataset.canceling = "0";
  }
}

// 结构化错误渲染：错误串格式 `code:message`，按 code 给可操作按钮
function renderError(errStr) {
  const sep = errStr.indexOf(":");
  const code = sep > 0 ? errStr.slice(0, sep) : "";
  const msg = sep > 0 ? errStr.slice(sep + 1) : errStr;
  const el = $("statusText");
  el.className = "status err";
  el.textContent = msg;

  const actions = {
    unauthorized: { label: "打开设置改 Key", fn: () => openSettings() },
    forbidden: { label: "检查 Key 权限", fn: () => openSettings() },
    config: { label: "打开设置", fn: () => openSettings() },
    no_profile: { label: "打开设置", fn: () => openSettings() },
    timeout: { label: "重试", fn: () => enhance() },
    rate_limited: { label: "稍后再试", fn: () => setTimeout(() => enhance(), 5000) },
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

async function copyResult() {
  const text = $("outputBox").value;
  if (!text) return;
  // 优先用浏览器原生剪贴板 API；受限环境（如 http 非安全上下文）回退到后端
  try {
    if (navigator.clipboard && window.isSecureContext) {
      await navigator.clipboard.writeText(text);
    } else {
      await api("/api/clipboard", { method: "POST", body: { text } });
    }
    setStatus("已复制到剪贴板", "ok");
  } catch (e) {
    // 原生 API 失败再走后端兜底
    try {
      await api("/api/clipboard", { method: "POST", body: { text } });
      setStatus("已复制到剪贴板", "ok");
    } catch (e2) {
      setStatus(`复制失败: ${e2}`, "err");
    }
  }
}

function setStatus(msg, cls) {
  const el = $("statusText");
  el.textContent = msg;
  el.className = "status" + (cls ? " " + cls : "");
}

// ── 设置抽屉 ──────────────────────────
function openSettings() {
  syncSettingsUI();
  $("settingsOverlay").hidden = false;
}
function closeSettings() {
  $("settingsOverlay").hidden = true;
  $("saveHint").textContent = "";
  $("testResult").textContent = "";
  $("crStatus").textContent = "";
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
  sel.innerHTML = "";
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
    : 0.7;
  updateUrlParse();
}

function collectProfileForm() {
  return {
    name: $("pfName").value.trim() || "未命名配置",
    protocol: $("pfProtocol").value,
    base_url: $("pfBaseUrl").value.trim(),
    model: $("pfModel").value.trim(),
    api_key: $("pfApiKey").value,
    temperature: parseFloat($("pfTemp").value) || 0.7,
  };
}

function currentProfileIdx() {
  const idx = parseInt($("profileSelect").value, 10);
  return Number.isInteger(idx) ? idx : 0;
}

async function saveAll() {
  config.close_to_tray = $("optCloseTray").checked;
  config.autostart = $("optAutostart").checked;
  // 增强选项一并保存（保证下次启动仍在）
  config.enhance = { ...enhanceOpts };
  // 保存前把表单中正在编辑的自定义模式 / 角色写回
  commitCustomModeForm();
  commitCustomRoleForm();
  try {
    await api("/api/config", { method: "POST", body: { config } });
    $("saveHint").textContent = "已保存";
    $("saveHint").className = "status ok";
    await refreshModes();
    await refreshRoles();
    setTimeout(() => ($("saveHint").textContent = ""), 1500);
  } catch (e) {
    $("saveHint").textContent = `保存失败: ${e}`;
    $("saveHint").className = "status err";
  }
}

// ── API 地址解析与回填 ─────────────────
// 粘贴完整 URL 后自动解析出 host / 端口 / 路径 / 接口路径，
// 并提供一键回填为基础地址（去掉接口后缀）。
let urlParseData = null;

async function updateUrlParse() {
  const raw = $("pfBaseUrl").value.trim();
  const box = $("urlParse");
  if (!raw) {
    box.hidden = true;
    urlParseData = null;
    return;
  }
  try {
    const d = await api("/api/parse-url", {
      method: "POST",
      body: { url: raw, protocol: $("pfProtocol").value },
    });
    urlParseData = d;
    const tags = [];
    tags.push(`<span class="url-tag"><span class="url-tag-label">协议</span><b>${d.scheme}</b></span>`);
    tags.push(`<span class="url-tag"><span class="url-tag-label">主机</span><b>${d.host}</b></span>`);
    if (d.port) {
      tags.push(`<span class="url-tag"><span class="url-tag-label">端口</span><b>${d.port}</b></span>`);
    }
    tags.push(`<span class="url-tag"><span class="url-tag-label">路径</span><b>${d.path}</b></span>`);
    if (d.suggested_endpoint) {
      tags.push(`<span class="url-tag"><span class="url-tag-label">接口</span><b>${d.suggested_endpoint}</b></span>`);
    }
    (d.notes || []).forEach((n) => {
      tags.push(`<span class="url-tag url-note">${escapeHtml(n)}</span>`);
    });
    // 回填按钮：仅当解析出的基础地址与当前输入不同才显示
    if (d.base_url && d.base_url !== raw) {
      tags.push(
        `<button type="button" class="url-fill" id="urlFillBtn">回填基础地址</button>`
      );
    }
    box.innerHTML = tags.join("");
    box.hidden = false;
    const fill = $("urlFillBtn");
    if (fill) {
      fill.addEventListener("click", () => {
        $("pfBaseUrl").value = d.normalized || d.base_url;
        updateUrlParse();
      });
    }
  } catch (e) {
    box.hidden = true;
    urlParseData = null;
  }
}

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  }[c]));
}

// ── 自定义模式管理 ────────────────────
let cmEditingId = null; // 正在编辑的自定义模式 id（null = 未进入编辑）

function renderCustomModeSelect() {
  const sel = $("cmSelect");
  sel.innerHTML = "";
  (config.custom_modes || []).forEach((m) => {
    const opt = document.createElement("option");
    opt.value = m.id;
    opt.textContent = `${m.name} (${m.id})`;
    sel.appendChild(opt);
  });
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
  sel.innerHTML = "";
  (config.custom_roles || []).forEach((r) => {
    const opt = document.createElement("option");
    opt.value = r.id;
    opt.textContent = `${r.name} (${r.id})`;
    sel.appendChild(opt);
  });
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
  } else {
    config.custom_roles.push(r);
  }
}

async function testConnection() {
  const profile = collectProfileForm();
  if (!profile.base_url || !profile.model) {
    $("testResult").textContent = "请先填写 API 地址和模型";
    $("testResult").className = "status err";
    return;
  }
  $("testBtn").disabled = true;
  $("testResult").textContent = "测试中…";
  $("testResult").className = "status";
  try {
    await api("/api/test", { method: "POST", body: { profile } });
    $("testResult").textContent = "连接成功";
    $("testResult").className = "status ok";
  } catch (e) {
    $("testResult").textContent = `失败: ${e}`;
    $("testResult").className = "status err";
  } finally {
    $("testBtn").disabled = false;
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
$("roleChip").addEventListener("click", () => {
  openSettings();
  switchTab("role");
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

$("profileSelect").addEventListener("change", () => {
  config.active_index = currentProfileIdx();
  loadProfileForm(config.active_index);
});
$("newProfile").addEventListener("click", () => {
  config.profiles = config.profiles || [];
  config.profiles.push(collectProfileForm());
  config.active_index = config.profiles.length - 1;
  renderProfileSelect();
  loadProfileForm(config.active_index);
});
$("delProfile").addEventListener("click", () => {
  if (!config.profiles?.length) return;
  config.profiles.splice(currentProfileIdx(), 1);
  config.active_index = 0;
  renderProfileSelect();
  loadProfileForm(config.active_index);
});

// API 地址：输入即解析（防抖）
let urlTimer = null;
$("pfBaseUrl").addEventListener("input", () => {
  clearTimeout(urlTimer);
  urlTimer = setTimeout(updateUrlParse, 220);
});
$("pfBaseUrl").addEventListener("paste", () => {
  setTimeout(updateUrlParse, 60); // 粘贴后立即解析（不等 input 防抖）
});
$("pfProtocol").addEventListener("change", updateUrlParse);

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
    const hist = (await api("/api/history")).history || [];
    panel.innerHTML = "";
    if (!hist.length) {
      panel.innerHTML = '<div class="history-empty">暂无历史记录</div>';
    }
    hist.forEach((h) => {
      const row = document.createElement("div");
      row.className = "history-row";
      const t = new Date(h.ts * 1000).toLocaleTimeString();
      row.innerHTML = `<span class="history-tag">${escapeHtml(h.mode_id)}</span>${escapeHtml(h.input)}`;
      row.title = h.output;
      row.addEventListener("click", () => {
        $("inputBox").value = h.input;
        $("outputBox").value = h.output;
        $("inputCounter").textContent = `${h.input.length} 字`;
        $("outputCounter").hidden = false;
        $("outputCounter").textContent = `${h.output.length} 字`;
        $("copyBtn").disabled = false;
        // 模式下拉回选（存在才选）
        const sel = $("modeSelect");
        if ([...sel.options].some((o) => o.value === h.mode_id)) sel.value = h.mode_id;
        updateSummary();
        panel.hidden = true;
        setStatus("已回填历史记录", "ok");
      });
      panel.appendChild(row);
    });
    panel.hidden = false;
  } catch (e) {
    setStatus(`历史读取失败: ${e}`, "err");
  }
});

// Esc 关闭设置面板
window.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && !$("settingsOverlay").hidden) {
    closeSettings();
  }
});

init();
