#!/usr/bin/env node

import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { createRequire } from "node:module";
import { chmod, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import {
  DEFAULT_CDP_HOST,
  assertLoopbackPortAvailable,
  parseDevCdpArguments,
  productionRendererBuildEnvironment,
  resolveCdpProfileDirectory,
  waitForElectronCdp,
} from "./dev-cdp-utils.mjs";
import {
  DEVELOPMENT_GRAPHICS_FALLBACK_EXIT_CODE,
  softwareFallbackArguments,
} from "./graphics-launch-utils.mjs";

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const desktopRoot = path.resolve(scriptDirectory, "..");
const repositoryRoot = path.resolve(desktopRoot, "../..");
const require = createRequire(import.meta.url);
const sessionFileName = ".dev-cdp-session.json";

let ownedChild;
let receivedSignal;
let forcedTermination;

for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
  process.on(signal, () => {
    receivedSignal ??= signal;
    terminateOwnedChild(signal);
  });
}

try {
  await main();
  if (receivedSignal) process.exitCode = signalExitCode(receivedSignal);
} catch (error) {
  const detail = error instanceof Error ? error.message : String(error);
  process.stderr.write(`dev:cdp: ${detail}\n`);
  process.exitCode = receivedSignal ? signalExitCode(receivedSignal) : 1;
} finally {
  terminateOwnedChild("SIGTERM");
}

async function main() {
  const options = parseDevCdpArguments(process.argv.slice(2));
  const profileDirectory = resolveCdpProfileDirectory(options.profile);
  const sessionPath = path.join(profileDirectory, sessionFileName);
  const sessionId = randomUUID();

  await assertLoopbackPortAvailable(options.port);
  await prepareProfile(profileDirectory, sessionPath);
  await createSessionRecord(sessionPath, {
    schemaVersion: 1,
    sessionId,
    profile: options.profile,
    port: options.port,
    launcherPid: process.pid,
    stage: "building",
    startedAt: new Date().toISOString(),
  });

  try {
    await runChecked("cargo", ["build", "--locked", "--package", "grok-daemon"], repositoryRoot);
    const buildEnvironment = productionRendererBuildEnvironment();
    await runPnpm(["--filter", "@grok-desktop/desktop", "build"], buildEnvironment);
    if (receivedSignal) return;

    // The locked builds can take time, so close the race between the first
    // preflight and launch by checking the port again immediately beforehand.
    await assertLoopbackPortAvailable(options.port);

    const executableName = process.platform === "win32" ? "grok-daemon.exe" : "grok-daemon";
    const daemonBinary = path.join(repositoryRoot, "target", "debug", executableName);
    const electronBinary = require("electron");
    if (typeof electronBinary !== "string" || !path.isAbsolute(electronBinary)) {
      throw new Error("the workspace Electron executable could not be resolved");
    }

    const environment = { ...process.env, GROK_DAEMON_BINARY: daemonBinary };
    delete environment.ELECTRON_RUN_AS_NODE;
    delete environment.VITE_BROWSER_PREVIEW;
    delete environment.VITE_DEV_SERVER_URL;

    const baseArguments = [
      `--remote-debugging-address=${DEFAULT_CDP_HOST}`,
      `--remote-debugging-port=${options.port}`,
      `--user-data-dir=${profileDirectory}`,
    ];
    let launchArguments = baseArguments;
    for (let attempt = 0; attempt < 2; attempt += 1) {
      const child = spawn(electronBinary, [...launchArguments, desktopRoot], {
        cwd: desktopRoot,
        env: environment,
        shell: false,
        stdio: "inherit",
        windowsHide: false,
      });
      ownedChild = child;
      const exit = childExit(child);
      await updateOwnedSessionRecord(sessionPath, sessionId, {
        schemaVersion: 1,
        sessionId,
        profile: options.profile,
        port: options.port,
        launcherPid: process.pid,
        electronPid: child.pid,
        stage: "running",
        startedAt: new Date().toISOString(),
      });

      const startup = await Promise.race([
        waitForElectronCdp(options.port).then((discovery) => ({ kind: "ready", discovery })),
        exit.then((outcome) => ({ kind: "exit", outcome })),
      ]);
      if (startup.kind === "exit") {
        ownedChild = undefined;
        if (
          startup.outcome.code === DEVELOPMENT_GRAPHICS_FALLBACK_EXIT_CODE
          && !startup.outcome.signal
          && attempt === 0
        ) {
          launchArguments = softwareFallbackArguments(baseArguments);
          process.stderr.write("dev:cdp: retrying once with software rendering\n");
          continue;
        }
        throw childExitError("Electron", startup.outcome);
      }

      process.stdout.write([
        `Grok Desktop CDP is listening at http://${DEFAULT_CDP_HOST}:${options.port}`,
        `Profile: ${options.profile} (${profileDirectory})`,
        `Renderer target: ${startup.discovery.targetUrl}`,
        `Electron PID: ${child.pid}`,
        "Run `pnpm test:e2e:electron -- --port " + options.port + "` in another terminal to probe the bridge.",
        "",
      ].join("\n"));

      const outcome = await exit;
      ownedChild = undefined;
      if (
        outcome.code === DEVELOPMENT_GRAPHICS_FALLBACK_EXIT_CODE
        && !outcome.signal
        && attempt === 0
      ) {
        launchArguments = softwareFallbackArguments(baseArguments);
        process.stderr.write("dev:cdp: retrying once with software rendering\n");
        continue;
      }
      if (!receivedSignal && (outcome.code !== 0 || outcome.signal)) throw childExitError("Electron", outcome);
      if (receivedSignal) process.exitCode = signalExitCode(receivedSignal);
      break;
    }
  } finally {
    terminateOwnedChild("SIGTERM");
    await removeOwnedSessionRecord(sessionPath, sessionId);
  }
}

async function runPnpm(arguments_, environment) {
  const pnpmScript = process.env.npm_execpath;
  if (pnpmScript && path.isAbsolute(pnpmScript)) {
    await runChecked(process.execPath, [pnpmScript, ...arguments_], repositoryRoot, environment);
    return;
  }
  const corepack = process.platform === "win32" ? "corepack.cmd" : "corepack";
  await runChecked(corepack, ["pnpm", ...arguments_], repositoryRoot, environment);
}

async function runChecked(command, arguments_, workingDirectory, environment = process.env) {
  if (receivedSignal) return;
  const child = spawn(command, arguments_, {
    cwd: workingDirectory,
    env: environment,
    shell: false,
    stdio: "inherit",
    windowsHide: false,
  });
  ownedChild = child;
  const outcome = await childExit(child);
  if (ownedChild === child) ownedChild = undefined;
  if (receivedSignal) return;
  if (outcome.code !== 0 || outcome.signal) throw childExitError(command, outcome);
}

function childExit(child) {
  return new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("exit", (code, signal) => resolve({ code, signal }));
  });
}

function childExitError(label, outcome) {
  if (outcome.signal) return new Error(`${label} exited after signal ${outcome.signal}`);
  return new Error(`${label} exited with code ${outcome.code ?? "unknown"}`);
}

function terminateOwnedChild(signal) {
  const child = ownedChild;
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  try {
    child.kill(signal);
    if (signal !== "SIGKILL" && !forcedTermination) {
      forcedTermination = setTimeout(() => {
        if (ownedChild === child && child.exitCode === null && child.signalCode === null) {
          try {
            child.kill("SIGKILL");
          } catch {
            // The exact owned process exited while the fallback was pending.
          }
        }
      }, 10_000);
      forcedTermination.unref();
      child.once("exit", () => {
        if (forcedTermination) clearTimeout(forcedTermination);
        forcedTermination = undefined;
      });
    }
  } catch {
    // The exact owned process may have exited between the state check and kill.
  }
}

async function prepareProfile(profileDirectory, sessionPath) {
  await mkdir(profileDirectory, { recursive: true, mode: 0o700 });
  if (process.platform !== "win32") await chmod(profileDirectory, 0o700);
  let record;
  try {
    record = JSON.parse(await readFile(sessionPath, "utf8"));
  } catch (error) {
    if (error && typeof error === "object" && "code" in error && error.code === "ENOENT") return;
    throw new Error(`profile session metadata is invalid: ${sessionPath}`, { cause: error });
  }

  const livePids = [record?.launcherPid, record?.electronPid]
    .filter((pid) => Number.isSafeInteger(pid) && pid > 0)
    .filter(processExists);
  if (livePids.length > 0) {
    throw new Error(`profile ${profileDirectory} is already owned by a live launcher session`);
  }
  await rm(sessionPath, { force: true });
}

async function createSessionRecord(sessionPath, record) {
  try {
    await writeFile(sessionPath, `${JSON.stringify(record, null, 2)}\n`, { encoding: "utf8", mode: 0o600, flag: "wx" });
  } catch (error) {
    if (error && typeof error === "object" && "code" in error && error.code === "EEXIST") {
      throw new Error("the named profile was claimed by another launcher during startup", { cause: error });
    }
    throw error;
  }
}

async function updateOwnedSessionRecord(sessionPath, sessionId, record) {
  const existing = JSON.parse(await readFile(sessionPath, "utf8"));
  if (existing?.sessionId !== sessionId) throw new Error("the named profile session changed ownership during startup");
  await writeFile(sessionPath, `${JSON.stringify(record, null, 2)}\n`, { encoding: "utf8", mode: 0o600, flag: "w" });
  if (process.platform !== "win32") await chmod(sessionPath, 0o600);
}

async function removeOwnedSessionRecord(sessionPath, sessionId) {
  try {
    const record = JSON.parse(await readFile(sessionPath, "utf8"));
    if (record?.sessionId === sessionId) await rm(sessionPath, { force: true });
  } catch (error) {
    if (!error || typeof error !== "object" || !("code" in error) || error.code !== "ENOENT") {
      process.stderr.write(`dev:cdp: could not clean owned session metadata: ${String(error)}\n`);
    }
  }
}

function processExists(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return Boolean(error && typeof error === "object" && "code" in error && error.code === "EPERM");
  }
}

function signalExitCode(signal) {
  return 128 + ({ SIGHUP: 1, SIGINT: 2, SIGTERM: 15 }[signal] ?? 1);
}
