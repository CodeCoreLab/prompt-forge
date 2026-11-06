// ─────────────────────────────────────────────────────────────────────────────
// 前端 XSS 回归自测（零依赖，纯 Node）
//
// 用法：
//   node tests/frontend-xss-selftest.cjs
//
// 说明：
// - app.js 是浏览器脚本（依赖 document / window / fetch），本脚本用 Node 内建
//   `vm` 模块搭一个最小 DOM 桩，把 web/app.js 真实源码跑起来，然后驱动
//   updateSummary() / updateUrlParse() 两个渲染函数，断言输出不含可执行的
//   HTML 注入。
// - 测的是**真实交付的 web/app.js**，不是复制粘贴的片段 —— 改坏 app.js 这里必红。
// - 无需浏览器、无需 Playwright、无需 npm 依赖。
//
// 覆盖范围：
//   A10 存储型 XSS —— 用户自建的「增强模式名 / 角色名」不得原样进 innerHTML
//   A10 存储型 XSS —— URL 解析结果（host / path / 接口 / 端口 / 协议）同样不得原样进 innerHTML
//
// 退出码：0 = 全部通过；1 = 有用例失败
// ─────────────────────────────────────────────────────────────────────────────

const fs = require("fs");
const path = require("path");
const vm = require("vm");

const WEB_DIR = path.resolve(__dirname, "..", "web");
const APP_JS = path.join(WEB_DIR, "app.js");

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
function section(name) {
  currentCase = name;
  console.log(`\n── ${name} ──`);
}

// ── 最小 DOM 桩 ─────────────────────────────────────────────────────────
/**
 * 造一个够用的假元素：只需覆盖 app.js 用到的属性与方法。
 * innerHTML 赋值时记录到 _html（测试断言读这个），不做真实解析 ——
 * 我们只关心「有没有把用户数据当 HTML 塞进去」。
 */
function makeEl(tag) {
  const el = {
    tagName: (tag || "div").toUpperCase(),
    _html: "",
    textContent: "",
    value: "",
    title: "",
    hidden: false,
    disabled: false,
    checked: false,
    className: "",
    dataset: {},
    style: {},
    options: [],
    children: [],
    classList: { add() {}, remove() {}, contains: () => false },
    appendChild(c) { this.children.push(c); return c; },
    remove() {},
    addEventListener() {},
    removeEventListener() {},
    focus() {},
    querySelector() { return null; },
    querySelectorAll() { return []; },
    setAttribute() {},
    getAttribute() { return null; },
    get innerHTML() { return this._html; },
    set innerHTML(v) { this._html = String(v); },
  };
  return el;
}

/**
 * 搭一个沙箱：document.getElementById 对任意 id 都返回稳定的假元素，
 * fetch 按路径返回预置响应。
 */
function makeSandbox(routes) {
  const els = new Map();
  const document = {
    getElementById(id) {
      if (!els.has(id)) els.set(id, makeEl("div"));
      return els.get(id);
    },
    createElement(tag) { return makeEl(tag); },
    querySelectorAll() { return []; },
    querySelector() { return null; },
    addEventListener() {},
  };
  const win = {
    addEventListener() {},
    isSecureContext: false,
  };
  const fetchImpl = async (url) => {
    const body = routes[url];
    if (body === undefined) {
      return { ok: false, status: 404, json: async () => ({ ok: false }) };
    }
    return { ok: true, status: 200, json: async () => body };
  };
  const ctx = vm.createContext({
    document,
    window: win,
    navigator: {},
    fetch: fetchImpl,
    console,
    setTimeout,
    clearTimeout,
    Promise,
  });
  ctx.globalThis = ctx;
  return { ctx, els };
}

/**
 * 加载真实 app.js 源码并执行。
 * app.js 末尾会调 init()（异步、无返回值），此处忽略其结果：
 * 我们只关心后面手动驱动的两个渲染函数。
 */
function loadApp(routes) {
  const src = fs.readFileSync(APP_JS, "utf8");
  const { ctx, els } = makeSandbox(routes);
  vm.runInContext(src, ctx, { filename: "app.js" });
  return { ctx, els };
}

/**
 * 判定一段 HTML 里是否出现了「用户数据被当成 HTML 标签解析」的注入。
 *
 * 判据只看**原始标签起始符**：输出里若出现未转义的 <img / <script / <svg /
 * <iframe / <object / <embed，说明用户数据是以 HTML 语义进入 DOM 的 —— XSS 成立。
 *
 * 注意：不能简单地在输出里搜 "onerror=" —— 转义后的文本
 * 「&lt;img src=x onerror=alert(1)&gt;」里同样含 "onerror="，那是**无害**的。
 * 所以「载荷原文是否原样出现」由 hasRawPayload() 单独判定。
 */
const RAW_DANGEROUS_TAGS = ["<img", "<script", "<svg", "<iframe", "<object", "<embed"];
function hasExecutableHtml(html) {
  const lower = String(html).toLowerCase();
  return RAW_DANGEROUS_TAGS.some((t) => lower.includes(t));
}

/**
 * 判定「用户载荷原文」是否原样出现在输出里。
 * 原样出现 = 未被转义 = 会被浏览器当 HTML 解析。
 */
function hasRawPayload(html, payload) {
  return String(html).includes(payload);
}

// ── 用例 ────────────────────────────────────────────────────────────────
const XSS_NAME = '<img src=x onerror=alert(1)>';
const XSS_URL_HOST = 'a"><img src=x onerror=alert(1)>';

// A10-1：配置摘要里的自定义模式名 / 角色名不得原样进 innerHTML
section("A10-1 配置摘要（自定义模式名 / 角色名）");
{
  const { ctx, els } = loadApp({
    "/api/modes": { ok: true, modes: [{ id: "m1", name: XSS_NAME, description: "" }] },
    "/api/config": { ok: true, config: { active_index: 0, profiles: [], custom_modes: [], custom_roles: [] } },
    "/api/methods": { ok: true, methods: [], thinking_levels: [], default_method_id: "general", default_thinking: "standard" },
    "/api/roles": { ok: true, roles: [{ id: "r1", name: XSS_NAME, description: "", builtin: false }] },
  });

  // 直接驱动渲染函数：设置全局数据源后调用 updateSummary()
  vm.runInContext(
    `
    modes = [{ id: "m1", name: ${JSON.stringify(XSS_NAME)}, description: "" }];
    roles = [{ id: "r1", name: ${JSON.stringify(XSS_NAME)}, description: "", builtin: false }];
    methods = [{ id: "general", name: "通用", description: "" }];
    thinkingLevels = [{ id: "standard", name: "标准", description: "" }];
    enhanceOpts = { method_id: "general", thinking: "standard", role_id: "r1" };
    document.getElementById("modeSelect").value = "m1";
    updateSummary();
    `,
    ctx
  );

  const html = els.get("configSummary")._html;
  console.log(`     输出: ${html}`);
  assert(
    !hasRawPayload(html, XSS_NAME),
    "配置摘要不含原样注入载荷",
    `载荷原文仍出现在输出中: ${html}`
  );
  assert(
    !hasExecutableHtml(html),
    "配置摘要不含可执行 HTML 标签",
    `输出仍含未转义标签: ${html}`
  );
  assert(
    html.includes("&lt;img"),
    "配置摘要对自定义名做了 HTML 转义",
    `输出未见转义后的 &lt;img: ${html}`
  );
}

// A10-2：URL 解析结果（host / path / 接口 / 端口 / 协议）不得原样进 innerHTML
section("A10-2 URL 解析结果（host / path / 接口）");
{
  const { ctx, els } = loadApp({
    "/api/parse-url": {
      ok: true,
      scheme: XSS_NAME,
      host: XSS_URL_HOST,
      port: XSS_NAME,
      path: XSS_NAME,
      suggested_endpoint: XSS_NAME,
      notes: [XSS_NAME],
      base_url: "",
      normalized: "",
    },
  });

  vm.runInContext(
    `
    document.getElementById("pfBaseUrl").value = "https://example.com/v1/chat";
    document.getElementById("pfProtocol").value = "openai";
    `,
    ctx
  );

  // updateUrlParse 是 async：等它跑完再断言
  vm.runInContext("updateUrlParse()", ctx);
  // 微任务队列排空（await 链最多几层）
  vm.runInContext(
    "new Promise(r => setTimeout(r, 0))",
    ctx
  ).then(() => {
    const html = els.get("urlParse")._html;
    console.log(`     输出: ${html}`);
    assert(
      !hasRawPayload(html, XSS_NAME),
      "URL 解析标签不含原样注入载荷",
      `载荷原文仍出现在输出中: ${html}`
    );
    assert(
      !hasRawPayload(html, XSS_URL_HOST),
      "URL 解析标签的 host 未被原样输出",
      `host 载荷原文仍出现在输出中: ${html}`
    );
    assert(
      !hasExecutableHtml(html),
      "URL 解析标签不含可执行 HTML 标签",
      `输出仍含未转义标签: ${html}`
    );
    assert(
      html.includes("&lt;img"),
      "URL 解析标签对解析结果做了 HTML 转义",
      `输出未见转义后的 &lt;img: ${html}`
    );

    // ── 汇总 ──
    const failed = results.filter((r) => !r.pass);
    console.log("\n" + "=".repeat(62));
    console.log(`前端 XSS 自测：${results.length - failed.length}/${results.length} 通过`);
    console.log("=".repeat(62));
    if (failed.length) {
      failed.forEach((f) => console.log(`  FAIL  [${f.case}] ${f.label} ${f.extra}`));
      process.exit(1);
    }
    process.exit(0);
  });
}
