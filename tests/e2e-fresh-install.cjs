// ─────────────────────────────────────────────────────────────────────────────
// 真实二进制端到端复现：全新安装 → 填表 → 点「保存全部」→ 查磁盘 config.json
//
// 用法：
//   node tests/e2e-fresh-install.cjs [serverExe] [--web-dir <目录>] [--keep-temp]
//
//   serverExe   默认 target/debug/prompt-forge.exe（也可用 PF_SERVER_EXE 环境变量）
//   --web-dir   传给 server 的前端目录参数（默认 ../web，走磁盘热更，
//               测的就是当前工作区的 web/app.js，不受内嵌资源时间滞后影响）
//
// ⚠ 隔离（重要）：
//   本脚本**自己启动 server**，并为该子进程单独设置隔离的配置目录环境变量：
//     Windows: APPDATA=<临时目录>
//     Linux  : XDG_CONFIG_HOME=<临时目录>
//     macOS  : HOME=<临时目录>
//   这些变量**只注入到被 spawn 的 server 子进程**（不写全局环境变量），
//   因此不会影响用户机器上的浏览器 / Electron / 其他程序。
//   启动后会解析 server stderr 打印的「配置: <路径>」行做二次确认：
//   若该路径不在临时目录内 → 立即中止并杀掉进程，绝不碰用户真实配置。
//
//   端口不用猜：server 端口被占用时会顺延，脚本从 stdout 的
//   `LISTENING <url>` 行取实际地址。
//
//   跑完自动杀掉 server 并删除临时目录。
//
// 退出码：0 = PASS；1 = FAIL（输入丢失）；2 = 脚本/环境错误
// ─────────────────────────────────────────────────────────────────────────────

const fs = require("fs");
const os = require("os");
const path = require("path");
const { spawn } = require("child_process");

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

const REPO = path.resolve(__dirname, "..");

// ── 参数解析 ─────────────────────────────────────────────────────────────
const argv = process.argv.slice(2);
let serverExe = process.env.PF_SERVER_EXE || "";
let webDir = path.join(REPO, "web");
let keepTemp = false;
for (let i = 0; i < argv.length; i++) {
  const a = argv[i];
  if (a === "--web-dir") { webDir = path.resolve(argv[++i] || ""); }
  else if (a === "--keep-temp") { keepTemp = true; }
  else if (!a.startsWith("--") && !serverExe) { serverExe = path.resolve(a); }
}
if (!serverExe) {
  serverExe = path.join(
    REPO,
    process.platform === "win32" ? "target/debug/prompt-forge.exe" : "target/debug/prompt-forge"
  );
}
if (!fs.existsSync(serverExe)) {
  console.error("找不到 server 可执行文件:", serverExe);
  console.error("请先 cargo build -p prompt-forge-server，或显式传入路径。");
  process.exit(2);
}

// ── 隔离配置目录 ─────────────────────────────────────────────────────────
// 用 `PROMPT_FORGE_CONFIG_DIR` 显式覆盖，而不是伪造 APPDATA / HOME / XDG_CONFIG_HOME：
// 后者只覆盖了配置目录的**父级**，多个 e2e 并行时仍会共用同一个 `prompt-forge`
// 子目录互相踩；显式覆盖则完全隔离，也不会碰到用户真实配置。
const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "pf-e2e-"));
const env = { ...process.env };
env.PROMPT_FORGE_CONFIG_DIR = path.join(tmpRoot, "config");

// ── 启动 server ──────────────────────────────────────────────────────────
// 端口取一个随机高位端口：core::ports::parse_port 明确拒绝 0（无随机端口语义），
// 且 server 端口被占用时会顺延 → 实际地址一律从 stdout 的 LISTENING 行取，不猜。
const wantPort = 18000 + Math.floor(Math.random() * 20000);
const child = spawn(
  serverExe,
  ["--no-browser", "--port", String(wantPort), "--web-dir", webDir],
  { env, stdio: ["ignore", "pipe", "pipe"] }
);

let stdoutBuf = "";
let stderrBuf = "";
child.stdout.setEncoding("utf8");
child.stderr.setEncoding("utf8");
child.stdout.on("data", (d) => { stdoutBuf += d; });
child.stderr.on("data", (d) => { stderrBuf += d; });

let exited = false;
child.on("exit", (code, sig) => {
  exited = true;
  console.error(`server 已退出 (code=${code}, sig=${sig})`);
  console.error("--- stdout ---\n" + stdoutBuf);
  console.error("--- stderr ---\n" + stderrBuf);
});

function sleep(ms) { return new Promise((r) => setTimeout(r, ms)); }

async function waitForListen(timeoutMs = 20000) {
  const t0 = Date.now();
  while (Date.now() - t0 < timeoutMs) {
    if (exited) throw new Error("server 启动即退出，见上方日志");
    const m = stdoutBuf.match(/^LISTENING\s+(\S+)\s*$/m);
    if (m) return m[1].replace(/\/$/, "");
    await sleep(100);
  }
  throw new Error("等待 LISTENING 行超时，stdout:\n" + stdoutBuf + "\nstderr:\n" + stderrBuf);
}

/** 从 server stderr 的「配置: <路径>」行取出实际配置路径 */
function parseConfigPath() {
  const m = stderrBuf.match(/配置:\s*(.+)/);
  return m ? m[1].trim() : null;
}

function cleanup() {
  try { if (!exited) child.kill(); } catch (_) { /* ignore */ }
  if (keepTemp) {
    console.log("临时目录保留（--keep-temp）:", tmpRoot);
  } else {
    try { fs.rmSync(tmpRoot, { recursive: true, force: true }); } catch (_) { /* ignore */ }
  }
}

(async () => {
  const base = await waitForListen();
  console.log("server 实际地址:", base);

  // 隔离二次确认：配置路径必须落在临时目录内，否则立即中止
  const cfgPath = parseConfigPath();
  console.log("server 配置路径  :", cfgPath);
  const tmpNorm = path.resolve(tmpRoot).toLowerCase();
  const cfgNorm = cfgPath ? path.resolve(cfgPath).toLowerCase() : "";
  if (!cfgPath || !cfgNorm.startsWith(tmpNorm)) {
    console.error("✗ 隔离校验失败：配置路径不在临时目录内，拒绝执行以免污染用户真实配置。");
    cleanup();
    process.exit(2);
  }

  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  const errs = [];
  page.on("pageerror", (e) => errs.push(String(e)));
  await page.goto(base + "/");
  await page.waitForFunction(() => {
    try { return typeof config !== "undefined" && config !== null; } catch (_) { return false; }
  });

  const overlayOpen = await page.evaluate(() => !document.getElementById("settingsOverlay").hidden);
  console.log("设置面板自动打开:", overlayOpen);

  // 走真实 input/change 事件（等价于用户逐字输入）
  await page.fill("#pfName", "我的首个档位");
  await page.fill("#pfBaseUrl", "https://api.example.com/v1");
  await page.fill("#pfModel", "gpt-4o");
  await page.fill("#pfApiKey", "sk-test-123");
  await page.click("#saveSettings");
  await page.waitForTimeout(1500);

  const hint = await page.textContent("#saveHint");
  const hintCls = await page.getAttribute("#saveHint", "class");
  const opts = await page.$$eval("#profileSelect option", (els) => els.map((e) => e.textContent));
  const memProfiles = await page.evaluate(() => (config.profiles || []).map((p) => ({
    name: p.name, base_url: p.base_url, model: p.model, api_key: p.api_key,
  })));
  console.log("saveHint.textContent =", JSON.stringify(hint));
  console.log("saveHint.className   =", JSON.stringify(hintCls));
  console.log("profileSelect options =", JSON.stringify(opts));
  console.log("内存 config.profiles  =", JSON.stringify(memProfiles));
  console.log("页面 JS 错误         =", errs.length ? errs.join(" | ") : "无");

  const api = await (await fetch(base + "/api/config")).json();
  console.log("GET /api/config profiles =", JSON.stringify(api.config.profiles));

  let diskProfiles = null;
  if (cfgPath && fs.existsSync(cfgPath)) {
    const disk = JSON.parse(fs.readFileSync(cfgPath, "utf8"));
    diskProfiles = disk.profiles;
    console.log("磁盘 config.json profiles =", JSON.stringify(diskProfiles));
  } else {
    console.log("磁盘 config.json 不存在:", cfgPath);
  }

  const p = api.config.profiles[0] || {};
  // 磁盘上的 api_key 是 DPAPI 密文（形如 enc:...），无法与明文直接比对；
  // 这里断言「密文存在且非空」，明文比对只在 /api/config 返回（已解密）时做。
  const dp = (diskProfiles && diskProfiles[0]) || {};
  const diskKeyOk = typeof dp.api_key === "string" && dp.api_key.length > 0;
  const ok = api.config.profiles.length >= 1
    && p.base_url === "https://api.example.com/v1"
    && p.model === "gpt-4o"
    && p.api_key === "sk-test-123"
    && Array.isArray(diskProfiles)
    && diskProfiles.length >= 1
    && dp.base_url === "https://api.example.com/v1"
    && dp.model === "gpt-4o"
    && diskKeyOk;
  console.log(ok ? "\nRESULT: PASS —— 输入已落盘" : "\nRESULT: FAIL —— 输入丢失");
  await browser.close();
  cleanup();
  process.exit(ok ? 0 : 1);
})().catch((e) => {
  console.error(e);
  cleanup();
  process.exit(2);
});
