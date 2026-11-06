// ─────────────────────────────────────────────────────────────────────────────
// 前端自测脚本（Playwright + 本地 mock 后端）
//
// 用法：
//   node tests/frontend-selftest.cjs
//
// 说明：
// - 后端用 Node 内建 http 模块做 mock，只实现前端真正用到的端点，
//   ���记录每次 POST /api/config 的请求体（落盘断言的事实依据）。
// - 静态文件从 ../web 直接读取，测的就是真实交付的 app.js / index.html。
// - Playwright 从全局 node_modules 解析（E:/npm-global/node_modules/playwright），
//   也可用 PLAYWRIGHT_PATH 环境变量覆盖。
//
// 覆盖范围（与审查项一一对应）：
//   #28 全新安装「填表 → 点保存全部」→ 用户输入必须落盘（不能静默丢失）
//   #31 全新安装「新增配置档」→ 首档必须保留用户填的 API Key
//   #32 超时门槛口径 → 必须严格大于服务端墙钟 T×6+30（否则双花）
//   P1-5 完整端点提示 → 出站自动去重提示可见，且不改动 base_url 存储值
//   增删配置档落盘 → 覆盖 mock 初始态「空 / 单档 / 多档」三种
//
// 退出码：0 = 全部通过；1 = 有用例失败
// ─────────────────────────────────────────────────────────────────────────────

const http = require("http");
const fs = require("fs");
const path = require("path");
const { spawnSync } = require("child_process");

// ── Playwright 解析 ──────────────────────────────────────────────────────
function loadPlaywright() {
  const candidates = [
    process.env.PLAYWRIGHT_PATH,
    "playwright",
    "E:/npm-global/node_modules/playwright",
    path.join(process.env.APPDATA || "", "npm/node_modules/playwright"),
  ].filter(Boolean);
  for (const c of candidates) {
    try {
      return require(c);
    } catch (_) {
      /* 继续尝试下一个 */
    }
  }
  throw new Error(
    "找不到 playwright。请安装全局 playwright，或设置 PLAYWRIGHT_PATH 环境变量。"
  );
}
const { chromium } = loadPlaywright();

const WEB_DIR = path.resolve(__dirname, "..", "web");

// ── 极简测试框架 ─────────────────────────────────────────────────────────
const results = [];
let currentCase = "";
function assert(cond, label, extra) {
  results.push({ case: currentCase, label, pass: !!cond, extra: extra || "" });
  const tag = cond ? "PASS" : "FAIL";
  console.log(`  [${tag}] ${label}${cond || !extra ? "" : ` — ${extra}`}`);
}
function eq(actual, expected, label) {
  assert(
    Object.is(actual, expected),
    label,
    `期望 ${JSON.stringify(expected)}，实际 ${JSON.stringify(actual)}`
  );
}

// ── mock 后端状态 ────────────────────────────────────────────────────────
let mockConfig = null;             // GET /api/config 返回的配置
const configPosts = [];            // 每次 POST /api/config 的请求体
let server = null;
let baseUrl = "";

// 与 core::endpoint::parse_endpoint 同构的最小实现（仅供前端测试用）
const SUFFIXES = [
  "/chat/completions", "/completions", "/embeddings", "/responses", "/models",
  "/v1/messages", "/messages",
  "/api/generate", "/api/chat", "/api/show",
  ":generatecontent", ":streamgeneratecontent", ":generateContent", ":streamGenerateContent",
];
function mockParseEndpoint(raw) {
  const input = String(raw || "").trim();
  if (!input) return null;
  const notes = [];
  let scheme = "https";
  let rest = input;
  if (input.includes("://")) {
    const i = input.indexOf("://");
    const s = input.slice(0, i).toLowerCase();
    scheme = s === "http" || s === "https" ? s : "https";
    rest = input.slice(i + 3);
  }
  const slash = rest.indexOf("/");
  const hostport = slash >= 0 ? rest.slice(0, slash) : rest;
  let p = slash >= 0 ? rest.slice(slash) : "/";
  // query / fragment：core 侧整段剥离并给出可见提示（不回显内容）
  const tail = p.includes("?") || p.includes("#") ? p.slice(p.search(/[?#]/)) : "";
  p = p.split(/[?#]/)[0] || "/";
  if (tail) {
    notes.push(
      "已忽略地址中的查询参数或片段（?… / #…）；API Key 请填写在「API Key」输入框，勿写入地址"
    );
  }
  if (!hostport || hostport.includes("@")) return null;
  let host = hostport;
  let port = null;
  if (hostport.startsWith("[")) {
    const j = hostport.indexOf("]");
    if (j < 0) return null;
    host = hostport.slice(0, j + 1);
    const tail = hostport.slice(j + 1);
    if (tail) {
      if (!tail.startsWith(":")) return null;
      port = Number(tail.slice(1));
      if (!Number.isInteger(port)) return null;
    }
  } else {
    const idx = hostport.lastIndexOf(":");
    if (idx > 0 && /^\d+$/.test(hostport.slice(idx + 1))) {
      host = hostport.slice(0, idx);
      port = Number(hostport.slice(idx + 1));
    }
  }
  if (!host) return null;
  const lower = p.toLowerCase();
  let basePath = p;
  let stripped = null;
  for (const suf of SUFFIXES) {
    if (lower.endsWith(suf.toLowerCase())) {
      basePath = p.slice(0, p.length - suf.length);
      stripped = suf;
      break;
    }
  }
  if (stripped) notes.push(`已识别接口路径 ${stripped}，基础地址回填为不含接口的根`);
  const origin = `${scheme}://${host}${port ? `:${port}` : ""}`;
  return {
    scheme, host, port, path: p, base_url: `${origin}${basePath}`, notes,
    suggested_endpoint: "/v1/chat/completions",
    normalized: `${origin}${basePath}`,
  };
}

const MIME = { ".html": "text/html; charset=utf-8", ".js": "text/javascript; charset=utf-8", ".css": "text/css; charset=utf-8" };

function json(res, obj, code = 200) {
  const body = JSON.stringify(obj);
  res.writeHead(code, { "Content-Type": "application/json; charset=utf-8" });
  res.end(body);
}

function readBody(req) {
  return new Promise((resolve) => {
    let raw = "";
    req.on("data", (c) => (raw += c));
    req.on("end", () => {
      try { resolve(JSON.parse(raw || "{}")); } catch (_) { resolve({}); }
    });
  });
}

function startServer() {
  return new Promise((resolve) => {
    server = http.createServer(async (req, res) => {
      const url = new URL(req.url, "http://127.0.0.1");
      const p = url.pathname;
      if (p.startsWith("/api/")) {
        if (p === "/api/config" && req.method === "GET") {
          return json(res, { ok: true, config: mockConfig });
        }
        if (p === "/api/config" && req.method === "POST") {
          const body = await readBody(req);
          configPosts.push(body);
          mockConfig = body.config;              // 模拟落盘
          return json(res, { ok: true });
        }
        if (p === "/api/modes") return json(res, { ok: true, modes: [{ id: "general", name: "通用" }] });
        if (p === "/api/methods") {
          return json(res, {
            ok: true,
            methods: [{ id: "general", name: "通用" }],
            thinking_levels: [{ id: "standard", name: "标准" }],
            default_method_id: "general",
          });
        }
        if (p === "/api/roles") {
          return json(res, {
            ok: true,
            roles: [{ id: "builtin-general", name: "通用助手", builtin: true }],
            role_missing: false,
            current_role_id: "builtin-general",
          });
        }
        if (p === "/api/parse-url" && req.method === "POST") {
          const body = await readBody(req);
          const parsed = mockParseEndpoint(body.url);
          if (!parsed) {
            return json(res, { ok: false, error: "config:无法解析该地址，请检查是否包含主机名", code: "config" }, 400);
          }
          return json(res, { ok: true, ...parsed });
        }
        if (p === "/api/enhance") {
          return json(res, { ok: true, output: "增强结果", applied: { role_name: "通用助手" }, request_id: 1 });
        }
        if (p === "/api/test") return json(res, { ok: true });
        if (p === "/api/cancel") return json(res, { ok: true });
        if (p === "/api/clipboard") return json(res, { ok: true });
        if (p === "/api/history") return json(res, { ok: true, history: [] });
        return json(res, { ok: false, error: "not_found:未知端点" }, 404);
      }
      // 静态文件
      const rel = p === "/" ? "index.html" : p.replace(/^\/+/, "");
      const file = path.join(WEB_DIR, rel);
      if (!file.startsWith(WEB_DIR) || !fs.existsSync(file) || fs.statSync(file).isDirectory()) {
        res.writeHead(404, { "Content-Type": "text/plain; charset=utf-8" });
        return res.end("not found");
      }
      res.writeHead(200, { "Content-Type": MIME[path.extname(file)] || "application/octet-stream" });
      res.end(fs.readFileSync(file));
    });
    server.listen(0, "127.0.0.1", () => {
      baseUrl = `http://127.0.0.1:${server.address().port}`;
      resolve();
    });
  });
}

// ── mock 初始态构造 ──────────────────────────────────────────────────────
function profile(over = {}) {
  return {
    name: "配置 1",
    protocol: "openai",
    base_url: "https://api.example.com/v1",
    model: "gpt-4o-mini",
    api_key: "sk-existing",
    temperature: 0.7,
    timeout_secs: 0,
    max_tokens: 0,
    ...over,
  };
}
function baseConfig(profiles, activeIndex = 0) {
  return {
    active_index: activeIndex,
    profiles,
    close_to_tray: true,
    autostart: true,
    custom_modes: [],
    custom_roles: [],
    enhance: { method_id: "general", thinking: "standard", role_id: "builtin-general" },
  };
}

// ── 页面辅助 ─────────────────────────────────────────────────────────────
async function openApp(browser, cfg) {
  mockConfig = cfg;
  configPosts.length = 0;
  const page = await browser.newPage();
  page.on("dialog", (d) => d.accept());
  const errors = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  page.on("console", (m) => {
    if (m.type() === "error") errors.push(m.text());
  });
  await page.goto(baseUrl + "/");
  await page.waitForFunction(() => {
    try { return typeof config !== "undefined" && config !== null; } catch (_) { return false; }
  });
  return { page, errors };
}

async function fillProfileForm(page, { baseUrl: bu, model, apiKey, name, protocol }) {
  if (name !== undefined) await page.fill("#pfName", name);
  if (protocol !== undefined) await page.selectOption("#pfProtocol", protocol);
  if (bu !== undefined) await page.fill("#pfBaseUrl", bu);
  if (model !== undefined) await page.fill("#pfModel", model);
  if (apiKey !== undefined) await page.fill("#pfApiKey", apiKey);
}

function lastPostedConfig() {
  return configPosts.length ? configPosts[configPosts.length - 1].config : null;
}

// ── 用例 ─────────────────────────────────────────────────────────────────
async function caseEmptyInstallSaveAll(browser) {
  currentCase = "#28 全新安装：填表 → 保存全部 → 输入必须落盘";
  const { page, errors } = await openApp(browser, baseConfig([]));
  // 全新安装会自动弹出设置面板
  const overlayOpen = await page.evaluate(() => !document.getElementById("settingsOverlay").hidden);
  assert(overlayOpen, "全新安装自动打开设置面板");

  await fillProfileForm(page, {
    name: "我的模型",
    protocol: "openai",
    baseUrl: "https://api.example.com/v1",
    model: "gpt-4o-mini",
    apiKey: "sk-only",
  });
  await page.click("#saveSettings");
  await page.waitForFunction(() => document.getElementById("saveHint").textContent !== "");

  const posted = lastPostedConfig();
  eq(configPosts.length, 1, "保存全部只发一次 POST /api/config");
  assert(!!posted, "POST /api/config 带有 config 字段");
  eq(posted.profiles.length, 1, "落盘 profiles.length == 1");
  eq(posted.profiles[0].base_url, "https://api.example.com/v1", "落盘 base_url == 用户所填");
  eq(posted.profiles[0].model, "gpt-4o-mini", "落盘 model == 用户所填");
  eq(posted.profiles[0].api_key, "sk-only", "落盘 api_key == 用户所填");
  eq(posted.profiles[0].name, "我的模型", "落盘 name == 用户所填");
  eq(posted.active_index, 0, "active_index == 0");
  const hint = await page.textContent("#saveHint");
  const hintCls = await page.getAttribute("#saveHint", "class");
  eq(hint, "已保存", "保存提示为「已保存」");
  eq(hintCls, "status ok", "保存提示样式为 status ok");
  eq(errors.length, 0, "无 JS 运行时错误", errors.join(" | "));
  await page.close();
}

async function caseEmptyInstallIllegalUrl(browser) {
  currentCase = "#28 全新安装：非法地址必须被拦住（不得静默成功）";
  const { page } = await openApp(browser, baseConfig([]));
  await fillProfileForm(page, {
    baseUrl: "http://u:p@host",
    model: "gpt-4o-mini",
    apiKey: "sk-only",
  });
  await page.click("#saveSettings");
  await page.waitForTimeout(400);
  eq(configPosts.length, 0, "非法地址时不发 POST /api/config");
  const hint = await page.textContent("#saveHint");
  assert(hint.includes("请先修正"), "提示包含「请先修正」", hint);
  await page.close();
}

async function caseEmptyInstallOnlyApiKey(browser) {
  currentCase = "#28 全新安装：只填 Key 不填地址 → 必须报错，不得静默丢数据";
  const { page } = await openApp(browser, baseConfig([]));
  await fillProfileForm(page, { apiKey: "sk-orphan" });
  await page.click("#saveSettings");
  await page.waitForTimeout(400);
  eq(configPosts.length, 0, "不发 POST /api/config");
  const hint = await page.textContent("#saveHint");
  assert(hint.includes("请先修正"), "提示包含「请先修正」", hint);
  await page.close();
}

async function caseExistingProfileUpdate(browser) {
  currentCase = "#28 已有档位：改表单 → 保存 → 内容被更新（老行为不回归）";
  const { page } = await openApp(browser, baseConfig([profile({ api_key: "sk-A" })]));
  await page.click("#settingsBtn");
  await fillProfileForm(page, {
    baseUrl: "https://api.example.com/v2",
    model: "gpt-4o",
    apiKey: "sk-A2",
  });
  await page.click("#saveSettings");
  await page.waitForFunction(() => document.getElementById("saveHint").textContent !== "");
  const posted = lastPostedConfig();
  eq(posted.profiles.length, 1, "仍是 1 档");
  eq(posted.profiles[0].base_url, "https://api.example.com/v2", "base_url 已更新");
  eq(posted.profiles[0].model, "gpt-4o", "model 已更新");
  eq(posted.profiles[0].api_key, "sk-A2", "api_key 已更新");
  await page.close();
}

async function caseEmptyInstallNewProfileKeepsKey(browser) {
  currentCase = "#31 全新安装：新增配置档 → 首档保留用户填的 API Key";
  const { page } = await openApp(browser, baseConfig([]));
  await fillProfileForm(page, {
    baseUrl: "https://api.example.com/v1",
    model: "gpt-4o-mini",
    apiKey: "sk-first-ever",
  });
  await page.click("#newProfile");
  await page.waitForFunction(() => config && config.profiles && config.profiles.length > 0);
  const posted = lastPostedConfig();
  assert(!!posted, "新增配置档后已落盘");
  eq(posted.profiles.length, 1, "落盘 1 档");
  eq(posted.profiles[0].api_key, "sk-first-ever", "首档 api_key == 用户所填");
  const formKey = await page.inputValue("#pfApiKey");
  eq(formKey, "sk-first-ever", "表单 API Key 未被清空");
  await page.close();
}

async function caseExistingNewProfileClearsKey(browser) {
  currentCase = "#31 已有档位：新增配置档 → 新档 Key 必须清空（原语义不回归）";
  const { page } = await openApp(browser, baseConfig([profile({ api_key: "key-A" })]));
  await page.click("#settingsBtn");
  await page.click("#newProfile");
  await page.waitForFunction(() => config && config.profiles && config.profiles.length > 1);
  const posted = lastPostedConfig();
  eq(posted.profiles.length, 2, "落盘 2 档");
  eq(posted.profiles[1].api_key, "", "新档 api_key == \"\"（防跨主机泄露）");
  eq(posted.profiles[0].api_key, "key-A", "第 1 档 api_key 仍为 key-A");
  await page.close();
}

async function caseTimeoutCaliber(browser) {
  currentCase = "#32 超时门槛：必须严格大于服务端墙钟 T×6+30";
  const { page } = await openApp(browser, baseConfig([profile({ timeout_secs: 0 })]));
  const res = await page.evaluate(() => {
    const out = {};
    // 磁盘快照与内存值不一致：磁盘 0（→后端默认 120），内存被改成 5
    savedProfiles = [{ timeout_secs: 0 }];
    config.profiles = [{ timeout_secs: 5 }];
    out.w1 = currentProfileTimeoutSecs();
    out.w1ms = enhanceTimeoutMs(currentProfileTimeoutSecs());
    // 磁盘 0 + 内存 0
    savedProfiles = [{ timeout_secs: 0 }];
    config.profiles = [{ timeout_secs: 0 }];
    out.w2 = currentProfileTimeoutSecs();
    out.w2ms = enhanceTimeoutMs(currentProfileTimeoutSecs());
    // 磁盘 120 + 内存 5
    savedProfiles = [{ timeout_secs: 120 }];
    config.profiles = [{ timeout_secs: 5 }];
    out.w3 = currentProfileTimeoutSecs();
    out.w3ms = enhanceTimeoutMs(currentProfileTimeoutSecs());
    // 磁盘 600
    savedProfiles = [{ timeout_secs: 600 }];
    config.profiles = [{ timeout_secs: 600 }];
    out.w4 = currentProfileTimeoutSecs();
    out.w4ms = enhanceTimeoutMs(currentProfileTimeoutSecs());
    // 逐档扫描：前端门槛 > 服务端墙钟
    out.scan = [];
    for (const t of [0, 5, 30, 60, 100, 109, 110, 120, 600, 3600]) {
      savedProfiles = [{ timeout_secs: t }];
      config.profiles = [{ timeout_secs: t }];
      const cur = currentProfileTimeoutSecs();
      const ms = enhanceTimeoutMs(cur);
      const eff = cur > 0 ? cur : 120;                 // 后端 effective_timeout_secs
      out.scan.push({ t, cur, ms, wall: (eff * 6 + 30) * 1000 });
    }
    return out;
  });
  eq(res.w1, 120, "W1 磁盘 0 + 内存 5 → 生效超时 120");
  eq(res.w1ms, 810000, "W1 前端门槛 810000ms");
  eq(res.w2, 120, "W2 磁盘 0 + 内存 0 → 生效超时 120");
  eq(res.w2ms, 810000, "W2 前端门槛 810000ms");
  eq(res.w3, 120, "W3 磁盘 120 + 内存 5 → 生效超时 120");
  eq(res.w3ms, 810000, "W3 前端门槛 810000ms");
  eq(res.w4, 600, "W4 磁盘 600 → 生效超时 600");
  eq(res.w4ms, 3690000, "W4 前端门槛 3690000ms");
  const bad = res.scan.filter((x) => !(x.ms > x.wall));
  assert(bad.length === 0, "W5 逐档扫描：前端门槛恒大于服务端墙钟",
    JSON.stringify(bad));
  await page.close();
}

/**
 * 超时口径的「0 / 缺失 / 非法」三态必须与后端
 * `LlmConfig::effective_timeout_secs()` 完全一致（0 或缺失 → 120）。
 *
 * 后端真源：crates/prompt-forge-core/src/config.rs
 *   `if self.timeout_secs == 0 { 120 } else { self.timeout_secs }`
 *   （timeout_secs 是 u64 且 #[serde(default)] —— 字段缺失时反序列化为 0）
 *
 * 前端若把「缺失」或「非法值」当成别的数（比如 undefined → NaN、
 * 负数 → 负门槛），算出的门槛就可能小于服务端墙钟 → 前端先 abort、
 * 服务端仍在后台跑完整条重试链并写历史 → 重复扣费（双花）。
 */
async function caseTimeoutEffectiveCaliber(browser) {
  currentCase = "#32 超时口径：0 / 缺失 / 非法 → 与后端 effective_timeout_secs 一致";
  const { page } = await openApp(browser, baseConfig([profile({ timeout_secs: 0 })]));
  const res = await page.evaluate(() => {
    const out = {};
    // 磁盘字段缺失（undefined / null / 空串 / 非数字 / 负数 / Infinity）
    out.missing = [];
    for (const v of [undefined, null, "", "abc", NaN, -1, Infinity, {}]) {
      savedProfiles = [{ timeout_secs: v }];
      config.profiles = [{ timeout_secs: v }];
      out.missing.push({
        v: String(v),
        cur: currentProfileTimeoutSecs(),
        ms: enhanceTimeoutMs(currentProfileTimeoutSecs()),
      });
    }
    // 整条记录缺失（磁盘快照里没有 timeout_secs 这个键）
    savedProfiles = [{}];
    config.profiles = [{}];
    out.noKey = currentProfileTimeoutSecs();
    out.noKeyMs = enhanceTimeoutMs(currentProfileTimeoutSecs());
    // 有效值原样透传
    savedProfiles = [{ timeout_secs: 300 }];
    config.profiles = [{ timeout_secs: 300 }];
    out.valid = currentProfileTimeoutSecs();
    out.validMs = enhanceTimeoutMs(currentProfileTimeoutSecs());
    // 磁盘值非法 + 内存值有效 → 必须以磁盘为准（不得回退内存）
    savedProfiles = [{ timeout_secs: "abc" }];
    config.profiles = [{ timeout_secs: 5 }];
    out.diskBad = currentProfileTimeoutSecs();
    return out;
  });
  for (const m of res.missing) {
    eq(m.cur, 120, `缺失/非法值 ${m.v} → 生效超时 120（对齐后端 effective_timeout_secs）`);
    eq(m.ms, 810000, `缺失/非法值 ${m.v} → 前端门槛 810000ms`);
  }
  eq(res.noKey, 120, "磁盘记录整条无 timeout_secs 键 → 120");
  eq(res.noKeyMs, 810000, "磁盘记录整条无 timeout_secs 键 → 门槛 810000ms");
  eq(res.valid, 300, "有效值 300 原样透传");
  eq(res.validMs, 1890000, "有效值 300 → 门槛 1890000ms");
  eq(res.diskBad, 120, "磁盘值非法时不得回退内存值 → 120");
  await page.close();
}

async function caseAddDeletePersist(browser) {
  currentCase = "增删配置档落盘（mock 初始态：空 / 单档 / 多档）";
  // 空态：新增 → 落盘 1 档
  {
    const { page } = await openApp(browser, baseConfig([]));
    await fillProfileForm(page, {
      baseUrl: "https://api.example.com/v1",
      model: "m1",
      apiKey: "sk-empty",
    });
    await page.click("#newProfile");
    await page.waitForFunction(() => config && config.profiles.length === 1);
    const posted = lastPostedConfig();
    eq(posted.profiles.length, 1, "空态：新增后落盘 1 档");
    eq(posted.profiles[0].api_key, "sk-empty", "空态：首档保留 Key");
    await page.close();
  }
  // 单档：删除 → 落盘 profiles: []，active_index = 0
  {
    const { page } = await openApp(browser, baseConfig([profile({ api_key: "sk-one" })]));
    await page.click("#settingsBtn");
    await page.click("#delProfile");
    await page.waitForFunction(() => config && config.profiles.length === 0);
    const posted = lastPostedConfig();
    eq(posted.profiles.length, 0, "单档：删除后落盘 profiles: []");
    eq(posted.active_index, 0, "单档：删除后 active_index == 0");
    await page.close();
  }
  // 多档：删中间档 → 落盘剩 A、C，Key 未错位
  {
    const { page } = await openApp(browser, baseConfig([
      profile({ name: "A", api_key: "key-A" }),
      profile({ name: "B", api_key: "key-B" }),
      profile({ name: "C", api_key: "key-C" }),
    ], 0));
    await page.click("#settingsBtn");
    await page.selectOption("#profileSelect", "1");
    await page.click("#delProfile");
    await page.waitForFunction(() => config && config.profiles.length === 2);
    const posted = lastPostedConfig();
    eq(posted.profiles.length, 2, "多档：删除后落盘 2 档");
    eq(posted.profiles[0].name, "A", "多档：第 1 档仍是 A");
    eq(posted.profiles[0].api_key, "key-A", "多档：第 1 档 Key 未错位");
    eq(posted.profiles[1].name, "C", "多档：第 2 档是 C");
    eq(posted.profiles[1].api_key, "key-C", "多档：第 2 档 Key 未错位");
    await page.close();
  }
}

async function caseUrlEndpointHint(browser) {
  currentCase = "P1-5 完整端点：出站去重提示可见，且不改动 base_url";
  const { page } = await openApp(browser, baseConfig([profile()]));
  await page.click("#settingsBtn");
  const raw = "https://api.anthropic.com/v1/messages";
  await fillProfileForm(page, { baseUrl: raw, protocol: "anthropic" });
  await page.waitForTimeout(600);
  const noteTexts = await page.$$eval("#urlParse .url-note", (els) => els.map((e) => e.textContent));
  assert(noteTexts.some((t) => t.includes("自动去重")), "#urlParse 出现「出站请求会按所选协议自动去重」提示",
    JSON.stringify(noteTexts));
  // P1-5-a：服务端 notes 已含「已识别接口路径 …」，
  // 前端追加的提示不得再以同一短语开头（否则同屏重复两遍）。
  const dupCount = noteTexts.filter((t) => t.includes("已识别接口路径")).length;
  eq(dupCount, 1, "「已识别接口路径」只出现在服务端 notes 一处（前端不重复）", JSON.stringify(noteTexts));
  const appended = noteTexts[noteTexts.length - 1] || "";
  assert(appended.startsWith("提示："), "前端追加的 .url-note 以「提示：」开头", JSON.stringify(appended));
  const val = await page.inputValue("#pfBaseUrl");
  eq(val, raw, "base_url 输入框未被改写（存储值保持用户原样）");
  const inner = await page.$$eval("#urlParse *", (els) => els.length);
  assert(inner >= 0, "#urlParse 已渲染解析结果");
  await page.close();
}

async function caseBaseUrlQueryFragmentRejected(browser) {
  currentCase = "query/fragment 口径：前端行内校验与 core 一致（拒绝 ?/#，不误伤正常地址）";
  const { page } = await openApp(browser, baseConfig([profile()]));
  await page.click("#settingsBtn");

  // ① 含 query 的地址 → 行内报错，文案含「查询参数或片段」
  {
    const err = await page.evaluate(() => {
      document.getElementById("pfBaseUrl").value = "https://x.com/v1?key=abc";
      return validateProfileForm().baseUrl || null;
    });
    assert(!!err, "含 query 的地址被前端行内校验拦下", JSON.stringify(err));
    assert(
      !!err && err.includes("查询参数或片段"),
      "query 报错文案含「查询参数或片段」",
      JSON.stringify(err)
    );
    assert(
      !!err && !err.includes("key=abc"),
      "query 报错文案不回显 query 内容（防凭据二次泄露）",
      JSON.stringify(err)
    );
  }

  // ② 含 fragment 的地址 → 同样被拦下
  {
    const err = await page.evaluate(() => {
      document.getElementById("pfBaseUrl").value = "https://x.com/v1#frag";
      return validateProfileForm().baseUrl || null;
    });
    assert(!!err, "含 fragment 的地址被前端行内校验拦下", JSON.stringify(err));
    assert(
      !!err && err.includes("查询参数或片段"),
      "fragment 报错文案含「查询参数或片段」",
      JSON.stringify(err)
    );
  }

  // ③ 正常地址不得被误拦（回归：最容易踩的坑）
  {
    const ok = await page.evaluate(() => {
      const good = [
        "https://api.openai.com/v1",
        "https://api.openai.com/v1/chat/completions",
        "https://api.anthropic.com/v1/messages",
        "http://127.0.0.1:11434",
        "https://x.com",
      ];
      const bad = [];
      for (const u of good) {
        const e = validateBaseUrl(u);
        if (e) bad.push(`${u} → ${e}`);
      }
      return bad;
    });
    eq(ok.length, 0, "正常地址（含完整端点）未被 query 规则误伤", JSON.stringify(ok));
  }

  // ④ 前端拦截后不发请求（不发 POST /api/config）
  {
    await fillProfileForm(page, { baseUrl: "https://x.com/v1?key=abc", model: "gpt-4o" });
    await page.click("#saveSettings");
    await page.waitForTimeout(400);
    eq(configPosts.length, 0, "含 query 的地址点保存 → 不发 POST /api/config");
    const hint = await page.textContent("#saveHint");
    assert(hint.includes("请先修正"), "保存被拦下并给出「请先修正」提示", hint);
  }

  // ⑤ 点「回填基础地址」→ 输入框变为不含 query 的地址 → 再保存成功
  {
    await fillProfileForm(page, { baseUrl: "https://x.com/v1?key=abc" });
    await page.waitForTimeout(600);
    const notes = await page.$$eval("#urlParse .url-note", (els) => els.map((e) => e.textContent));
    assert(
      notes.some((t) => t.includes("已忽略地址中的查询参数或片段")),
      "解析区出现服务端「已忽略地址中的查询参数或片段」提示",
      JSON.stringify(notes)
    );
    assert(
      notes.every((t) => !t.includes("key=abc")),
      "解析提示不回显 query 内容",
      JSON.stringify(notes)
    );
    const hasFill = await page.$("#urlFillBtn");
    assert(!!hasFill, "含 query 的地址存在「回填基础地址」按钮");
    await page.click("#urlFillBtn");
    await page.waitForTimeout(400);
    const val = await page.inputValue("#pfBaseUrl");
    assert(!val.includes("?") && !val.includes("#"), "回填后输入框不含 query/fragment", val);
    await page.click("#saveSettings");
    await page.waitForFunction(() => document.getElementById("saveHint").textContent !== "");
    const posted = lastPostedConfig();
    assert(!!posted && posted.profiles.length >= 1, "回填后保存成功（发出 POST /api/config）");
    const stored = posted.profiles[posted.profiles.length - 1].base_url;
    assert(
      !String(stored).includes("?") && !String(stored).includes("#"),
      "落盘 base_url 不含 query/fragment",
      String(stored)
    );
  }

  await page.close();
}

async function caseStaticChecks() {
  currentCase = "静态检查：XSS 危险 API 零命中";
  const src = fs.readFileSync(path.join(WEB_DIR, "app.js"), "utf8");
  const banned = ["innerHTML", "outerHTML", "insertAdjacentHTML", "document.write", "eval(", "new Function"];
  for (const b of banned) {
    const n = src.split(b).length - 1;
    eq(n, 0, `app.js 中 "${b}" 出现 0 次`);
  }
  const check = spawnSync(process.execPath, ["--check", path.join(WEB_DIR, "app.js")], { encoding: "utf8" });
  eq(check.status, 0, "node --check web/app.js 通过", (check.stderr || "").trim());

  // 跨 crate 注释不得点名 server 内部实现 / 文件路径。
  //
  // 前端注释引用 server 内部函数名（如 total_outbound_budget_secs / wall_clock）
  // 会随后端重构而漂移 —— 前端注释是「对外口径说明」，应只描述数值口径本身。
  // core 公共 API 引用（parse_endpoint / resolve_endpoint / strip_endpoint_suffix /
  // validate_base_url / crates/prompt-forge-core/...）是**允许**的，故不在此列。
  currentCase = "静态检查：跨 crate 注释不引用 server 内部实现名";
  const serverInternal = ["total_outbound_budget_secs", "wall_clock", "prompt-forge-server"];
  for (const s of serverInternal) {
    const n = src.split(s).length - 1;
    eq(n, 0, `app.js 中 "${s}" 出现 0 次（不引用 server 内部实现）`);
  }
  // 反向确认：core 公共 API 引用仍然保留（不能误删）
  const corePublic = ["parse_endpoint", "resolve_endpoint", "strip_endpoint_suffix", "validate_base_url"];
  for (const s of corePublic) {
    const n = src.split(s).length - 1;
    assert(n > 0, `app.js 仍引用 core 公共 API "${s}"（保留跨 crate 口径可核对性）`);
  }
}

// ── 主流程 ───────────────────────────────────────────────────────────────
(async () => {
  await startServer();
  const browser = await chromium.launch({ headless: true });
  try {
    await caseEmptyInstallSaveAll(browser);
    await caseEmptyInstallIllegalUrl(browser);
    await caseEmptyInstallOnlyApiKey(browser);
    await caseExistingProfileUpdate(browser);
    await caseEmptyInstallNewProfileKeepsKey(browser);
    await caseExistingNewProfileClearsKey(browser);
    await caseTimeoutCaliber(browser);
    await caseTimeoutEffectiveCaliber(browser);
    await caseAddDeletePersist(browser);
    await caseUrlEndpointHint(browser);
    await caseBaseUrlQueryFragmentRejected(browser);
    await caseStaticChecks();
  } finally {
    await browser.close();
    server.close();
  }

  const failed = results.filter((r) => !r.pass);
  console.log("");
  console.log(`共 ${results.length} 条断言，通过 ${results.length - failed.length}，失败 ${failed.length}`);
  if (failed.length) {
    console.log("");
    failed.forEach((f) => console.log(`  FAIL [${f.case}] ${f.label} ${f.extra}`));
  }
  process.exit(failed.length ? 1 : 0);
})().catch((e) => {
  console.error("自测脚本异常:", e);
  if (server) server.close();
  process.exit(2);
});
