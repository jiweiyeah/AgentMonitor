#!/usr/bin/env node
// Thin launcher for the agent-monitor native binary. Mirrors the shape of
// OpenAI's Codex launcher: detect platform, resolve the scoped optional dep,
// spawn the bundled binary with `stdio: "inherit"` so the TUI owns the TTY.

import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const require = createRequire(import.meta.url);

const PLATFORM_PKG_BY_TARGET = {
  "aarch64-apple-darwin":       "agentmonitor-darwin-arm64",
  "x86_64-apple-darwin":        "agentmonitor-darwin-x64",
  "aarch64-unknown-linux-gnu":  "agentmonitor-linux-arm64-gnu",
  "x86_64-unknown-linux-gnu":   "agentmonitor-linux-x64-gnu",
  "aarch64-pc-windows-msvc":    "agentmonitor-win32-arm64",
  "x86_64-pc-windows-msvc":     "agentmonitor-windows-x64",
};

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
