#!/usr/bin/env node

import { spawn } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { developmentNativeBuildArguments } from "./dev-cdp-utils.mjs";

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const desktopRoot = path.resolve(scriptDirectory, "..");
const pnpmScript = process.env.npm_execpath;
if (!pnpmScript || !path.isAbsolute(pnpmScript)) {
  throw new Error("the pnpm executable path is unavailable");
}

const repositoryRoot = path.resolve(desktopRoot, "../..");
const nativeBuild = spawn("cargo", developmentNativeBuildArguments(), {
  cwd: repositoryRoot,
  env: process.env,
  shell: false,
  stdio: "inherit",
  windowsHide: false,
});
const nativeOutcome = await childOutcome(nativeBuild, -1);
if (nativeOutcome.code !== 0) {
  throw new Error("native development binaries failed to build");
}

const nativeExtension = process.platform === "win32" ? ".exe" : "";
const developmentEnvironment = {
  ...process.env,
  GROK_DAEMON_BINARY: path.join(repositoryRoot, "target", "debug", `grok-daemon${nativeExtension}`),
};

const graphicsArguments = process.argv.slice(2);
const children = [
  spawn(process.execPath, [pnpmScript, "exec", "vite", "--host", "127.0.0.1"], childOptions()),
  spawn(process.execPath, [pnpmScript, "electron:watch"], childOptions()),
  spawn(process.execPath, [path.join(scriptDirectory, "launch-electron.mjs"), ...graphicsArguments], childOptions()),
];
let stopping = false;
let requestedSignal;

for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
  process.on(signal, () => {
    requestedSignal ??= signal;
    stopChildren(signal);
  });
}

const first = await Promise.race(children.map((child, index) => childOutcome(child, index)));
stopChildren("SIGTERM");
const forcedTermination = setTimeout(() => {
  for (const child of children) {
    if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
  }
}, 10_000);
await Promise.allSettled(children.map((child, index) => childOutcome(child, index)));
clearTimeout(forcedTermination);
if (requestedSignal) process.exitCode = 128 + signalNumber(requestedSignal);
else if (first.signal) process.exitCode = 128 + signalNumber(first.signal);
else process.exitCode = first.code ?? 1;

function childOptions() {
  return {
    cwd: desktopRoot,
    env: developmentEnvironment,
    shell: false,
    stdio: "inherit",
    windowsHide: false,
  };
}

function childOutcome(child, index) {
  if (child.exitCode !== null || child.signalCode !== null) {
    return Promise.resolve({ index, code: child.exitCode, signal: child.signalCode });
  }
  return new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("exit", (code, signal) => resolve({ index, code, signal }));
  });
}

function stopChildren(signal) {
  if (stopping) return;
  stopping = true;
  for (const child of children) {
    if (child.exitCode === null && child.signalCode === null) child.kill(signal);
  }
}

function signalNumber(signal) {
  return { SIGHUP: 1, SIGINT: 2, SIGTERM: 15 }[signal] ?? 1;
}
