#!/usr/bin/env node
// Thin launcher for the agent-monitor native binary. Mirrors the shape of
// OpenAI's Codex launcher: detect platform, resolve the scoped optional dep,
// spawn the bundled binary with `stdio: "inherit"` so the TUI owns the TTY.
// Also handles the `update` sub-command by invoking the detected package
// manager, avoiding a full uninstall/reinstall cycle.

import { spawn, execSync } from "node:child_process";
import { existsSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { readFileSync } from "node:fs";
import https from "node:https";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const require = createRequire(import.meta.url);

const MAIN_PKG = "@yeheboo/agentmonitor";
const REGISTRY_URL = `https://registry.npmjs.org/${encodeURIComponent(MAIN_PKG)}/latest`;

const PLATFORM_PKG_BY_TARGET = {
  "aarch64-apple-darwin":       "agentmonitor-darwin-arm64",
  "x86_64-apple-darwin":        "agentmonitor-darwin-x64",
  "aarch64-unknown-linux-gnu":  "agentmonitor-linux-arm64-gnu",
  "x86_64-unknown-linux-gnu":   "agentmonitor-linux-x64-gnu",
  "aarch64-pc-windows-msvc":    "agentmonitor-win32-arm64",
  "x86_64-pc-windows-msvc":     "agentmonitor-windows-x64",
};

// ── update command ──────────────────────────────────────────────────────────

function getCurrentVersion() {
  try {
    const pkgPath = path.join(__dirname, "..", "package.json");
    return JSON.parse(readFileSync(pkgPath, "utf8")).version ?? null;
  } catch {
    return null;
  }
}

function fetchLatestVersion() {
  return new Promise((resolve, reject) => {
    const req = https.get(REGISTRY_URL, { timeout: 10000 }, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        return fetchLatestVersion(new URL(res.headers.location)).then(resolve, reject);
      }
      if (res.statusCode !== 200) {
        reject(new Error(`registry returned ${res.statusCode}`));
        res.resume();
        return;
      }
      const chunks = [];
      res.on("data", (c) => chunks.push(c));
      res.on("end", () => {
        try {
          const data = JSON.parse(Buffer.concat(chunks).toString());
          resolve(data.version ?? null);
        } catch (e) {
          reject(e);
        }
      });
    });
    req.on("error", reject);
    req.on("timeout", () => { req.destroy(); reject(new Error("request timed out")); });
  });
}

function semverGt(a, b) {
  const pa = a.split(".").map(Number);
  const pb = b.split(".").map(Number);
  for (let i = 0; i < 3; i++) {
    if ((pa[i] ?? 0) > (pb[i] ?? 0)) return true;
    if ((pa[i] ?? 0) < (pb[i] ?? 0)) return false;
  }
  return false;
}

function getUpdateCommand(pm) {
  const cmds = {
    npm:  `npm install -g ${MAIN_PKG}@latest`,
    yarn: `yarn global add ${MAIN_PKG}@latest`,
    pnpm: `pnpm add -g ${MAIN_PKG}@latest`,
    bun:  `bun install -g ${MAIN_PKG}@latest`,
  };
  return cmds[pm] ?? cmds.npm;
}

async function handleUpdate() {
  const current = getCurrentVersion();
  if (!current) {
    console.error("[agent-monitor] cannot determine current version");
    process.exit(1);
  }

  console.log(`Current version: ${current}`);

  let latest;
  try {
    latest = await fetchLatestVersion();
  } catch (e) {
    console.error(`[agent-monitor] failed to check for updates: ${e.message}`);
    process.exit(1);
  }

  if (!latest) {
    console.error("[agent-monitor] could not determine latest version from registry");
    process.exit(1);
  }

  console.log(`Latest version:  ${latest}`);

  if (!semverGt(latest, current)) {
    console.log("Already up to date.");
    return;
  }

  const pm = detectPackageManager();
  const cmd = getUpdateCommand(pm);

  console.log(`Updating with ${pm}...`);

  try {
    execSync(cmd, { stdio: "inherit" });
    console.log(`\n✓ Updated to ${latest}`);
  } catch {
    console.error(`\n[agent-monitor] update failed. Try manually: ${cmd}`);
    process.exit(1);
  }
}

// ── original launcher logic ─────────────────────────────────────────────────

function resolveTargetTriple() {
  const { platform, arch } = process;
  if (platform === "darwin") {
    if (arch === "arm64") return "aarch64-apple-darwin";
    if (arch === "x64")   return "x86_64-apple-darwin";
  } else if (platform === "linux") {
    if (arch === "arm64") return "aarch64-unknown-linux-gnu";
    if (arch === "x64")   return "x86_64-unknown-linux-gnu";
  } else if (platform === "win32") {
    if (arch === "arm64") return "aarch64-pc-windows-msvc";
    if (arch === "x64")   return "x86_64-pc-windows-msvc";
  }
  return null;
}

function detectPackageManager() {
  const ua = process.env.npm_config_user_agent || "";
  if (ua.startsWith("bun"))  return "bun";
  if (ua.startsWith("pnpm")) return "pnpm";
  if (ua.startsWith("yarn")) return "yarn";
  return "npm";
}

function reinstallHint(pkg) {
  const pm = detectPackageManager();
  const reinstall = {
    bun:  "bun install -g @yeheboo/agentmonitor@latest",
    pnpm: "pnpm add -g @yeheboo/agentmonitor@latest",
    yarn: "yarn global add @yeheboo/agentmonitor@latest",
    npm:  "npm install -g @yeheboo/agentmonitor@latest",
  }[pm];
  return `Missing optional dependency ${pkg}. Reinstall with: ${reinstall}`;
}

// ── main ────────────────────────────────────────────────────────────────────

if (process.argv[2] === "update") {
  handleUpdate().catch((e) => {
    console.error(`[agent-monitor] update error: ${e.message}`);
    process.exit(1);
  });
} else {
  launchBinary();
}

function launchBinary() {
  const triple = resolveTargetTriple();
  if (!triple) {
    console.error(`[agent-monitor] unsupported platform: ${process.platform}-${process.arch}`);
    process.exit(1);
  }

  const platformPkg = PLATFORM_PKG_BY_TARGET[triple];
  const binaryName = process.platform === "win32" ? "agent-monitor.exe" : "agent-monitor";

  // 1. Try to resolve via the scoped optional-dep package.
  let binaryPath = null;
  try {
    const pkgJson = require.resolve(`${platformPkg}/package.json`);
    binaryPath = path.join(path.dirname(pkgJson), "bin", binaryName);
    if (!existsSync(binaryPath)) binaryPath = null;
  } catch {
    // fall through
  }

  // 2. Fallback: look in ../vendor for dev / local builds before publish.
  if (!binaryPath) {
    const devPath = path.join(__dirname, "..", "vendor", triple, binaryName);
    if (existsSync(devPath)) binaryPath = devPath;
  }

  if (!binaryPath) {
    console.error(`[agent-monitor] ${reinstallHint(platformPkg)}`);
    process.exit(1);
  }

  const child = spawn(binaryPath, process.argv.slice(2), {
    stdio: "inherit",
    windowsHide: true,
  });

  child.on("error", (err) => {
    console.error(`[agent-monitor] failed to launch: ${err.message}`);
    process.exit(1);
  });
  child.on("exit", (code, signal) => {
    if (signal) {
      process.kill(process.pid, signal);
    } else {
      process.exit(code ?? 0);
    }
  });

  // Forward common termination signals so we don't orphan the child on Ctrl+C.
  for (const sig of ["SIGINT", "SIGTERM", "SIGHUP"]) {
    process.on(sig, () => {
      try { child.kill(sig); } catch {}
    });
  }
}
