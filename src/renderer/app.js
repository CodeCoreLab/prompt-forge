// Prompt Forge 前端逻辑 — Electron 版（替换 Tauri IPC 为 Electron IPC）
const { promptForge } = window;

const $ = (id) => document.getElementById(id);

// ── 状态 ──────────────────────────────
let config = null;   // AppConfig
let modes = [];      // 增强模式列表

// ── 初始化 ────────────────────────────
async function init() {
  try {
    [modes, config] = await Promise.all([
      promptForge.getModes(),
      promptForge.getConfig(),
    ]);
  } catch (e) {
    setStatus(`初始化失败: ${e}`, "err");
    config = { active_index: 0, profiles: [], close_to_tray: true, autostart: true, custom_modes: [] };
  }
  renderModes();
  renderProfileSelect();
  if (!config.profiles || config.profiles.length === 0) openSettings();
}

// ── 模式 ──────────────────────────────
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
  const cancelBtnMode = btn.dataset.canceling === "1";
  if (cancelBtnMode) {
    try { await promptForge.cancelEnhance(); } catch (e) { /* 忽略 */ }
    return;
  }
  btn.disabled = false;
  btn.textContent = "取消";
  btn.dataset.canceling = "1";
  $("outputBox").value = "";
  $("copyBtn").disabled = true;
  setStatus("", "");
  const statusEl = $("statusText");
  statusEl.innerHTML = '<span class="spin"></span>增强中…';

  try {
    const result = await promptForge.enhancePrompt(
      input,
      $("modeSelect").value,
    );
    $("outputBox").value = result;
    $("copyBtn").disabled = false;
    statusEl.textContent = "";
  } catch (e) {
    statusEl.innerHTML = "";
    const s = String(e);
    if (s === "cancelled:已取消") {
      setStatus("已取消", "");
    } else {
      renderError(s);
    }
  } finally {
    btn.textContent = "增强";
    btn.dataset.canceling = "0";
    btn.disabled = false;
  }
}

// 结构化错误渲染
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
    btn.className = "ghost-btn";
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
  try {
    await promptForge.copyToClipboard(text);
    setStatus("已复制到剪贴板", "ok");
  } catch (e) {
    setStatus(`复制失败: ${e}`, "err");
  }
}

function setStatus(msg, cls) {
  const el = $("statusText");
  el.textContent = msg;
  el.className = "status" + (cls ? " " + cls : "");
}

// ── 设置面板 ──────────────────────────
function openSettings() {
  syncSettingsUI();
  $("settingsOverlay").classList.remove("hidden");
}
function closeSettings() {
  $("settingsOverlay").classList.add("hidden");
  $("saveHint").textContent = "";
  $("testResult").textContent = "";
}

function syncSettingsUI() {
  renderProfileSelect();
  renderCustomModeSelect();
  $("optCloseTray").checked = config.close_to_tray;
  $("optAutostart").checked = config.autostart;
  loadProfileForm(config.active_index || 0);
  loadCustomModeForm(config.custom_modes?.[0]?.id || null);
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
  $("pfTemp").value = p?.temperature ?? 0.7;
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
  commitCustomModeForm();
  try {
    await promptForge.saveConfig(config);
    $("saveHint").textContent = "已保存";
    $("saveHint").className = "status ok";
    await refreshModes();
    setTimeout(() => ($("saveHint").textContent = ""), 1500);
  } catch (e) {
    $("saveHint").textContent = `保存失败: ${e}`;
    $("saveHint").className = "status err";
  }
}

// ── 自定义模式管理 ──────────────────────
let cmEditingId = null;

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

function commitCustomModeForm() {
  if (cmEditingId === null) return;
  const id = $("cmId").value.trim();
  if (!id) return;
  const m = {
    id,
    name: $("cmName").value.trim() || id,
    type: $("cmType").value,
    template: $("cmTemplate").value,
  };
  config.custom_modes = config.custom_modes || [];
  const idx = config.custom_modes.findIndex((x) => x.id === cmEditingId);
  if (idx >= 0) {
    if (config.custom_modes.some((x, i) => i !== idx && x.id === id)) return;
    config.custom_modes[idx] = m;
  } else {
    config.custom_modes.push(m);
  }
}

function setStatusTip(msg) {
  setStatus(msg, "");
}

async function testConnection() {
  const profile = collectProfileForm();
  if (!profile.base_url || !profile.model) {
    $("testResult").textContent = "请先填写 Base URL 和模型";
    $("testResult").className = "status err";
    return;
  }
  $("testBtn").disabled = true;
  $("testResult").textContent = "测试中…";
  $("testResult").className = "status";
  try {
    await promptForge.testConnection(profile);
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

$("settingsBtn").addEventListener("click", openSettings);
$("closeSettings").addEventListener("click", closeSettings);
$("settingsOverlay").addEventListener("click", (e) => {
  if (e.target === e.currentTarget) closeSettings();
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

$("testBtn").addEventListener("click", testConnection);
$("saveSettings").addEventListener("click", saveAll);

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
  setStatusTip("填写后点保存，即新建自定义模式");
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

// ── 历史记录 ──────────────────────────
$("historyBtn").addEventListener("click", async () => {
  const panel = $("historyPanel");
  if (!panel.classList.contains("hidden")) {
    panel.classList.add("hidden");
    return;
  }
  try {
    const hist = await promptForge.getHistory();
    panel.innerHTML = "";
    if (!hist.length) {
      panel.innerHTML = '<div style="padding:8px;color:#888">暂无历史记录</div>';
    }
    hist.forEach((h) => {
      const row = document.createElement("div");
      row.style.cssText = "padding:6px 8px;cursor:pointer;border-bottom:1px solid rgba(128,128,128,.15);white-space:nowrap;overflow:hidden;text-overflow:ellipsis";
      const t = new Date(h.ts * 1000).toLocaleTimeString();
      row.textContent = `[${t}·${h.mode_id}] ${h.input}`;
      row.title = h.output;
      row.addEventListener("click", () => {
        $("inputBox").value = h.input;
        $("outputBox").value = h.output;
        $("copyBtn").disabled = false;
        const sel = $("modeSelect");
        if ([...sel.options].some((o) => o.value === h.mode_id)) sel.value = h.mode_id;
        panel.classList.add("hidden");
        setStatus("已回填历史记录", "ok");
      });
      panel.appendChild(row);
    });
    panel.classList.remove("hidden");
  } catch (e) {
    setStatus(`历史读取失败: ${e}`, "err");
  }
});

// ── 键盘快捷键 ─────────────────────────
window.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && !$("settingsOverlay").classList.contains("hidden")) {
    closeSettings();
  }
});

// ── 启动 ──────────────────────────────
init();
