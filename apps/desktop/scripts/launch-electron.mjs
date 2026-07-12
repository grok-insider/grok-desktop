#!/usr/bin/env node

import { spawn } from "node:child_process";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";
import waitOn from "wait-on";
import {
  DEVELOPMENT_GRAPHICS_FALLBACK_EXIT_CODE,
  softwareFallbackArguments,
} from "./graphics-launch-utils.mjs";

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const desktopRoot = path.resolve(scriptDirectory, "..");
const electronBinary = createRequire(import.meta.url)("electron");
const forwardedArguments = process.argv.slice(2);
let child;
let stopping = false;

for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
  process.on(signal, () => {
    stopping = true;
    child?.kill(signal);
  });
}

await waitOn({
  resources: ["tcp:127.0.0.1:5173", path.join(desktopRoot, "dist-electron/electron/main.js")],
  timeout: 30_000,
});

let launchArguments = forwardedArguments;
let fallbackUsed = false;
for (let attempt = 0; attempt < 2; attempt += 1) {
  if (stopping) break;
  const outcome = await launch(launchArguments);
  if (
    outcome.code === DEVELOPMENT_GRAPHICS_FALLBACK_EXIT_CODE
    && !outcome.signal
    && !fallbackUsed
  ) {
    fallbackUsed = true;
    launchArguments = softwareFallbackArguments(launchArguments);
    process.stderr.write("electron: retrying once with software rendering\n");
    continue;
  }
  if (outcome.signal) process.exitCode = 128 + signalNumber(outcome.signal);
  else process.exitCode = outcome.code ?? 1;
  break;
}

function launch(arguments_) {
  const environment = { ...process.env, VITE_DEV_SERVER_URL: "http://127.0.0.1:5173" };
  delete environment.ELECTRON_RUN_AS_NODE;
  child = spawn(electronBinary, [...arguments_, desktopRoot], {
    cwd: desktopRoot,
    env: environment,
    shell: false,
    stdio: "inherit",
    windowsHide: false,
  });
  return new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("exit", (code, signal) => {
      child = undefined;
      resolve({ code, signal });
    });
  });
}

function signalNumber(signal) {
  return { SIGHUP: 1, SIGINT: 2, SIGTERM: 15 }[signal] ?? 1;
}
