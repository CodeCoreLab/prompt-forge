// ─────────────────────────────────────────────────────────────────────────────
// 配置档生命周期端到端验证（真实 server + 真实 web/app.js）
//
// 用法：
//   node tests/e2e-profile-lifecycle.cjs [serverExe]
//
// 覆盖 audit-server 提出的、需要人工确认的两条语义：
//   ① 首档保留 API Key —— 全新安装创建的第一个档位必须保留用户刚填的 Key
//      （不存在「旧 Key 发往新主机」的风险）
//   ② 后续新增档清空 Key —— 已有档位时新增必须清空 API Key（防跨主机泄露）
//   ③ 删除档位后磁盘与内存同步（profiles 不错位、active_index 正确）
//
// 隔离（重要）：
//   本脚本**自己启动 server**，只给该子进程注入隔离的配置目录环境变量
//   （Windows: APPDATA / Linux: XDG_CONFIG_HOME / macOS: HOME），
//   **不写全局环境变量** —— 用户机器上的浏览器 / Electron 不受影响。
//   启动后解析 server stderr 的「配置: <路径>」行做二次确认：
//   若该路径不在临时目录内 → 立即中止并杀掉进程，绝不碰用户真实配置。
//
//   端口不猜：从 server stdout 的 `LISTENING <url>` 行取实际地址。
//
// 退出码：0 = 全部通过；1 = 有用例失败；2 = 脚本/环境错误
// ─────────────────────────────────────────────────────────────────────────────

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
  throw new Error(
    "找不到 playwright。请安装全局 playwright，或设置 PLAYWRIGHT_PATH 环境变量。"
  );
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
  console.error("找不到 server 可执行文件:", serverExe);
  console.error("请先 cargo build -p prompt-forge-server，或显式传入路径。");
  process.exit(2);
}

// ── 隔离配置目录 ─────────────────────────────────────────────────────────
const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "pf-life-"));
const env = { ...process.env };
if (process.platform === "win32") {
  env.APPDATA = path.join(tmpRoot, "appdata");
} else if (process.platform === "darwin") {
  env.HOME = path.join(tmpRoot, "home");
} else {
  env.XDG_CONFIG_HOME = path.join(tmpRoot, "config");
}

// ── 启动 server ──────────────────────────────────────────────────────────
const wantPort = 25000 + Math.floor(Math.random() * 20000);
const child = spawn(
  serverExe,
  ["--no-browser", "--port", String(wantPort), "--web-dir", path.join(REPO, "web")],
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

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function waitForListen(timeoutMs = 20000) {
  const t0 = Date.now();
  while (Date.now() - t0 < timeoutMs) {
    if (exited) throw new Error("server 启动即退出，见上方日志");
    const m = stdoutBuf.match(/^LISTENING\s+(\S+)\s*$/m);
    if (m) return m[1].replace(/\/$/, "");
    await sleep(100);
  }
  throw new Error("等待 LISTENING 行超时");
}

function parseConfigPath() {
  const m = stderrBuf.match(/配置:\s*(.+)/);
  return m ? m[1].trim() : null;
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
  const base = await waitForListen();
  console.log("server 实际地址:", base);

  // 隔离二次确认
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
  // 删除配置档会弹 window.confirm（不可撤销）→ 一律接受
  page.on("dialog", (d) => d.accept());
  const errs = [];
  page.on("pageerror", (e) => errs.push(String(e)));
  await page.goto(base + "/");
  await page.waitForFunction(() => {
    try { return typeof config !== "undefined" && config !== null; } catch (_) { return false; }
  });

  const overlayOpen = await page.evaluate(() => !document.getElementById("settingsOverlay").hidden);
  check(overlayOpen, "全新安装自动打开设置面板");

  // ── ① 首档保留 API Key ─────────────────────────────────────────────────
  await page.fill("#pfName", "档位A");
  await page.fill("#pfBaseUrl", "https://api.example.com/v1");
  await page.fill("#pfModel", "gpt-4o");
  await page.fill("#pfApiKey", "sk-first-keep-me");
  await page.click("#saveSettings");
  await page.waitForFunction(() => document.getElementById("saveHint").textContent !== "");
  await page.waitForTimeout(600);

  const afterFirst = await (await fetch(base + "/api/config")).json();
  const first = afterFirst.config.profiles[0] || {};
  check(afterFirst.config.profiles.length === 1, "首档落盘：profiles 长度为 1",
    String(afterFirst.config.profiles.length));
  check(first.api_key === "sk-first-keep-me", "① 首档保留 API Key（未清空）",
    JSON.stringify(first.api_key));
  check(first.base_url === "https://api.example.com/v1", "首档 base_url 正确", String(first.base_url));
  check(first.model === "gpt-4o", "首档 model 正确", String(first.model));

  const diskAfterFirst = JSON.parse(fs.readFileSync(cfgPath, "utf8"));
  const dFirst = (diskAfterFirst.profiles || [])[0] || {};
  check(
    typeof dFirst.api_key === "string" && dFirst.api_key.startsWith("enc:"),
    "首档 api_key 以 DPAPI 密文（enc:）落盘，无明文",
    String(dFirst.api_key).slice(0, 12)
  );

  // ── ② 已有档位时新增档必须清空 Key ─────────────────────────────────────
  await page.click("#newProfile");
  await page.waitForTimeout(800);
  const afterAdd = await (await fetch(base + "/api/config")).json();
  check(afterAdd.config.profiles.length === 2, "新增后 profiles 长度为 2",
    String(afterAdd.config.profiles.length));
  const added = afterAdd.config.profiles[1] || {};
  check(added.api_key === "", "② 新增档 API Key 已清空（防跨主机泄露）",
    JSON.stringify(added.api_key));
  check(afterAdd.config.profiles[0].api_key === "sk-first-keep-me",
    "② 首档 Key 未被新增动作误清空",
    JSON.stringify(afterAdd.config.profiles[0].api_key));
  check(afterAdd.config.active_index === 1, "新增后 active_index 指向新档",
    String(afterAdd.config.active_index));

  // ── ③ 删除档位：磁盘与内存同步、不错位 ────────────────────────────────
  await page.click("#delProfile");
  await page.waitForTimeout(800);
  const afterDel = await (await fetch(base + "/api/config")).json();
  check(afterDel.config.profiles.length === 1, "删除后 profiles 长度为 1",
    String(afterDel.config.profiles.length));
  check(afterDel.config.profiles[0].api_key === "sk-first-keep-me",
    "③ 删除后剩余档 Key 不错位（仍是首档的 Key）",
    JSON.stringify(afterDel.config.profiles[0].api_key));
  check(afterDel.config.active_index === 0, "③ 删除后 active_index 归 0",
    String(afterDel.config.active_index));

  const diskAfterDel = JSON.parse(fs.readFileSync(cfgPath, "utf8"));
  check((diskAfterDel.profiles || []).length === 1, "③ 磁盘 config.json 同步为 1 档",
    String((diskAfterDel.profiles || []).length));

  // ── ④ 全部删空：profiles: []（audit-server 指出的 #28 复现场景）────────
  await page.click("#delProfile");
  await page.waitForTimeout(800);
  const afterAll = await (await fetch(base + "/api/config")).json();
  check(Array.isArray(afterAll.config.profiles) && afterAll.config.profiles.length === 0,
    "④ 删光后 profiles 为 []（中性空态）",
    JSON.stringify(afterAll.config.profiles));
  check(afterAll.config.active_index === 0, "④ 删光后 active_index 为 0",
    String(afterAll.config.active_index));

  // ── ⑤ 删空后重新填表保存 → 首档再次保留 Key（往返语义）───────────────
  const overlayNow = await page.evaluate(() => !document.getElementById("settingsOverlay").hidden);
  if (!overlayNow) await page.click("#settingsBtn");
  await page.fill("#pfName", "档位B");
  await page.fill("#pfBaseUrl", "https://api.example.com/v2");
  await page.fill("#pfModel", "gpt-4o-mini");
  await page.fill("#pfApiKey", "sk-round-trip");
  await page.click("#saveSettings");
  await page.waitForFunction(() => document.getElementById("saveHint").textContent !== "");
  await page.waitForTimeout(600);
  const afterRound = await (await fetch(base + "/api/config")).json();
  check(afterRound.config.profiles.length === 1, "⑤ 删空后重新保存 → 重建 1 档",
    String(afterRound.config.profiles.length));
  check(afterRound.config.profiles[0].api_key === "sk-round-trip",
    "⑤ 重建后首档保留 API Key",
    JSON.stringify(afterRound.config.profiles[0].api_key));

  check(errs.length === 0, "无 JS 运行时错误", errs.join(" | "));

  await browser.close();
  cleanup();
  console.log(failed === 0 ? "\nRESULT: PASS —— 配置档生命周期语义全部符合预期"
                          : `\nRESULT: FAIL —— ${failed} 项不符`);
  process.exit(failed === 0 ? 0 : 1);
})().catch((e) => {
  console.error(e);
  cleanup();
  process.exit(2);
});
