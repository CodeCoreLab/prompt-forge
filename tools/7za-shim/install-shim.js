#!/usr/bin/env node
/*
 * Install the 7za shim into node_modules/7zip-bin.
 *
 * Why: electron-builder's app-builder unpacks the winCodeSign archive, which
 * contains macOS symlinks. Creating symlinks on Windows needs Developer Mode
 * (SeCreateSymbolicLinkPrivilege); without it 7-Zip exits with code 2 and the
 * build fails with an endless download/retry loop.
 *
 * The shim forwards all arguments to the real 7-Zip and appends an exclusion
 * for the "darwin" subtree (macOS openssl only, irrelevant for Windows builds).
 *
 * The shim binary is compiled from shim.c on demand (gcc from MinGW-w64), or
 * reused if a prebuilt binary is already present.
 */
const fs = require("fs");
const path = require("path");
const { execFileSync } = require("child_process");

const repoRoot = path.resolve(__dirname, "..", "..");
const shimDir = path.join(repoRoot, "tools", "7za-shim");
const shimSource = path.join(shimDir, "shim.c");
const shimExe = path.join(shimDir, "7za.exe");
const targetDir = path.join(repoRoot, "node_modules", "7zip-bin", "win", "x64");
const targetExe = path.join(targetDir, "7za.exe");
const backupExe = path.join(targetDir, "7za.real.exe");

function log(msg) {
  process.stdout.write(`[7za-shim] ${msg}\n`);
}

if (process.platform !== "win32") {
  log("not Windows, nothing to do");
  process.exit(0);
}

if (!fs.existsSync(targetExe)) {
  log(`7za.exe not found at ${targetDir}, skipping (run npm install first)`);
  process.exit(0);
}

if (fs.existsSync(backupExe)) {
  log("shim already installed, nothing to do");
  process.exit(0);
}

// Compile the shim if no prebuilt binary is available.
if (!fs.existsSync(shimExe)) {
  if (!fs.existsSync(shimSource)) {
    log(`shim source not found at ${shimSource}, skipping`);
    process.exit(0);
  }
  const candidates = [
    ["gcc", ["-O2", "-o", shimExe, shimSource]],
    ["x86_64-w64-mingw32-gcc", ["-O2", "-o", shimExe, shimSource]],
    ["cl", ["/nologo", "/O2", "/Fe:" + shimExe, shimSource]],
  ];
  let compiled = false;
  for (const [compiler, args] of candidates) {
    try {
      execFileSync(compiler, args, { stdio: "ignore" });
      log(`compiled shim with ${compiler}`);
      compiled = true;
      break;
    } catch {
      /* try next compiler */
    }
  }
  if (!compiled) {
    log("no C compiler found (gcc / x86_64-w64-mingw32-gcc / cl); skipping shim install");
    process.exit(0);
  }
}

// Keep a pristine copy of the real 7-Zip next to the shim, then swap them.
fs.copyFileSync(targetExe, backupExe);
fs.copyFileSync(shimExe, targetExe);
log(`installed shim -> ${targetExe} (original kept as 7za.real.exe)`);
