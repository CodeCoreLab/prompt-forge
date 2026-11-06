// 真实 server 验证：前端 query/fragment 口径与 core 一致
// 用法: node tests/e2e-query-fragment.cjs [serverExe]
// 校验：① 前端行内拦截含 query 地址 ② server 端仍 fail-closed
//       ③ 点「回填基础地址」后保存成功且落盘不含 query
const fs = require("fs");
const os = require("os");
const path = require("path");
const { spawn } = require("child_process");

function loadPlaywright() {
  const candidates = [
    process.env.PLAYWRIGHT_PATH,
    "playwright",
    "E:/npm-global/node_modules/playwright",
    path.join(process.env.APPDATA || "", "npm/node_modules/playwright"),
  ].filter(Boolean);
  for (const c of candidates) {
    try { return require(c); } catch (_) { /* next */ }
  }
  throw new Error("找不到 playwright");
}
const { chromium } = loadPlaywright();

const REPO = path.resolve(__dirname, "..");
let serverExe = process.argv[2] || process.env.PF_SERVER_EXE || "";
if (!serverExe) {
  serverExe = path.join(
    REPO,
    process.platform === "win32" ? "target/debug/prompt-forge.exe" : "target/debug/prompt-forge"
  );
}
if (!fs.existsSync(serverExe)) {
  console.error("找不到 server:", serverExe);
  process.exit(2);
}

// 隔离配置目录（只注入子进程）
// 用 `PROMPT_FORGE_CONFIG_DIR` 显式覆盖：伪造 APPDATA / HOME / XDG_CONFIG_HOME
// 只覆盖了配置目录的父级，并行运行时仍会共用同一个 `prompt-forge` 子目录互相踩。
const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "pf-qf-"));
const env = { ...process.env };
env.PROMPT_FORGE_CONFIG_DIR = path.join(tmpRoot, "config");

const wantPort = 21000 + Math.floor(Math.random() * 20000);
const child = spawn(
  serverExe,
  ["--no-browser", "--port", String(wantPort), "--web-dir", path.join(REPO, "web")],
  { env, stdio: ["ignore", "pipe", "pipe"] }
);
let out = "", errOut = "";
child.stdout.setEncoding("utf8");
child.stderr.setEncoding("utf8");
child.stdout.on("data", (d) => { out += d; });
child.stderr.on("data", (d) => { errOut += d; });
let exited = false;
child.on("exit", () => { exited = true; });

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function waitListen(ms = 20000) {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    if (exited) throw new Error("server 退出\n" + out + "\n" + errOut);
    const m = out.match(/^LISTENING\s+(\S+)\s*$/m);
    if (m) return m[1].replace(/\/$/, "");
    await sleep(100);
  }
  throw new Error("等待 LISTENING 超时");
}
function cleanup() {
  try { if (!exited) child.kill(); } catch (_) { /* ignore */ }
  try { fs.rmSync(tmpRoot, { recursive: true, force: true }); } catch (_) { /* ignore */ }
}

let failed = 0;
function check(cond, label, extra) {
  if (!cond) failed++;
  console.log(`  [${cond ? "PASS" : "FAIL"}] ${label}${cond || !extra ? "" : ` — ${extra}`}`);
}

(async () => {
  const base = await waitListen();
  const cfgPath = (errOut.match(/配置:\s*(.+)/) || [])[1];
  console.log("server:", base, "\n配置路径:", cfgPath);

  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  const errs = [];
  page.on("pageerror", (e) => errs.push(String(e)));
  await page.goto(base + "/");
  await page.waitForFunction(() => {
    try { return typeof config !== "undefined" && config !== null; } catch (_) { return false; }
  });

  // ① 前端行内拦截
  const lineErr = await page.evaluate(() => {
    document.getElementById("pfBaseUrl").value = "https://x.com/v1?key=abc";
    return validateProfileForm().baseUrl || null;
  });
  check(!!lineErr, "① 前端行内拦截含 query 地址", JSON.stringify(lineErr));
  check(
    !!lineErr && lineErr.includes("查询参数或片段"),
    "① 文案含「查询参数或片段」",
    JSON.stringify(lineErr)
  );
  check(
    !!lineErr && !lineErr.includes("key=abc"),
    "① 文案不回显 query 内容",
    JSON.stringify(lineErr)
  );

  // ② server 端仍 fail-closed
  const cfg = await (await fetch(base + "/api/config")).json();
  const bad = {
    ...cfg.config,
    profiles: [{
      name: "PF_Q", protocol: "openai", base_url: "https://x.com/v1?key=abc",
      model: "gpt-4o", api_key: "sk-x", temperature: 0.7, timeout_secs: 0, max_tokens: 0,
    }],
  };
  const res = await fetch(base + "/api/config", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ config: bad }),
  });
  const rj = await res.json();
  check(rj.ok === false, "② server 端仍拒绝含 query 的 base_url（fail-closed 不变）", JSON.stringify(rj));
  check(
    String(rj.error || "").includes("查询参数或片段"),
    "② server 错误文案含「查询参数或片段」",
    JSON.stringify(rj.error || "")
  );
  check(
    !String(rj.error || "").includes("key=abc"),
    "② server 错误文案不回显 query 内容",
    JSON.stringify(rj.error || "")
  );

  // ③ 点「回填基础地址」→ 保存成功 → 落盘不含 query
  // 全新安装（无档位）时设置面板会自动弹出；已有档位才需要手动点开。
  const overlayOpen = await page.evaluate(() => !document.getElementById("settingsOverlay").hidden);
  if (!overlayOpen) await page.click("#settingsBtn");
  await page.fill("#pfBaseUrl", "https://x.com/v1?key=abc");
  await page.waitForTimeout(700);
  const notes = await page.$$eval("#urlParse .url-note", (els) => els.map((e) => e.textContent));
  check(
    notes.some((t) => t.includes("已忽略地址中的查询参数或片段")),
    "③ 解析区出现「已忽略地址中的查询参数或片段」提示",
    JSON.stringify(notes)
  );
  check(
    notes.every((t) => !t.includes("key=abc")),
    "③ 解析提示不回显 query 内容",
    JSON.stringify(notes)
  );
  const hasFill = await page.$("#urlFillBtn");
  check(!!hasFill, "③ 存在「回填基础地址」按钮");
  if (hasFill) {
    await page.click("#urlFillBtn");
    await page.waitForTimeout(500);
    const val = await page.inputValue("#pfBaseUrl");
    check(!val.includes("?") && !val.includes("#"), "③ 回填后输入框不含 query/fragment", val);
    // 补齐其余必填项再保存
    await page.fill("#pfName", "PF_Q");
    await page.fill("#pfModel", "gpt-4o");
    await page.fill("#pfApiKey", "sk-x");
    await page.click("#saveSettings");
    await page.waitForFunction(() => document.getElementById("saveHint").textContent !== "");
    const hint = await page.textContent("#saveHint");
    check(hint.includes("已保存"), "③ 回填后保存成功", hint);
    const after = await (await fetch(base + "/api/config")).json();
    const p0 = after.config.profiles[0] || {};
    check(
      !String(p0.base_url).includes("?") && !String(p0.base_url).includes("#"),
      "③ 落盘 base_url 不含 query/fragment",
      String(p0.base_url)
    );
    if (cfgPath && fs.existsSync(cfgPath)) {
      const disk = JSON.parse(fs.readFileSync(cfgPath, "utf8"));
      const d0 = (disk.profiles || [])[0] || {};
      check(
        !String(d0.base_url).includes("?") && !String(d0.base_url).includes("#"),
        "③ 磁盘 config.json base_url 不含 query/fragment",
        String(d0.base_url)
      );
    }
  }
  check(errs.length === 0, "无 JS 运行时错误", errs.join(" | "));

  await browser.close();
  cleanup();
  console.log(failed === 0 ? "\nRESULT: PASS" : `\nRESULT: FAIL（${failed} 项）`);
  process.exit(failed === 0 ? 0 : 1);
})().catch((e) => {
  console.error(e);
  cleanup();
  process.exit(2);
});
