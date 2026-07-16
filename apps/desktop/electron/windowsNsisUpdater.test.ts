// @vitest-environment node
import { createHash } from "node:crypto";
import { EventEmitter } from "node:events";
import { lstat, mkdtemp, readdir, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  installWindowsUpdateAfterDaemonStop,
  WindowsNsisUpdateRunner,
} from "./windowsNsisUpdater.js";

const roots: string[] = [];
const contents = Buffer.from("authorized installer bytes");
const expected = {
  kind: "nsis-installer" as const,
  url: "https://github.com/grok-insider/grok-desktop/releases/download/v1.2.3/GrokDesktop-beta-x64.exe",
  size: contents.byteLength,
  sha256: createHash("sha256").update(contents).digest("hex"),
};
const windowsEnvironment = {
  APPDATA: "C:\\Users\\Test\\AppData\\Roaming",
  LOCALAPPDATA: "C:\\Users\\Test\\AppData\\Local",
  Path: "C:\\Windows\\System32",
  SystemRoot: "C:\\Windows",
  TEMP: "C:\\Users\\Test\\AppData\\Local\\Temp",
  USERPROFILE: "C:\\Users\\Test",
  XAI_API_KEY: "must-not-reach-installer",
};

afterEach(async () => {
  await Promise.all(roots.splice(0).map((root) => rm(root, { recursive: true, force: true })));
});

describe("WindowsNsisUpdateRunner", () => {
  it("rejects a non-canonical or wrong-kind artifact before network access", async () => {
    const root = await temporaryRoot();
    const fetchResponse = vi.fn<typeof fetch>();
    const spawnProcess = vi.fn();
    const runner = new WindowsNsisUpdateRunner(root, spawnProcess, fetchResponse);
    await expect(runner.download({
      ...expected,
      url: "https://example.com/GrokDesktop-beta-x64.exe",
    })).rejects.toThrow("URL");
    await expect(runner.download({ ...expected, kind: "appimage" })).rejects.toThrow("metadata");
    expect(fetchResponse).not.toHaveBeenCalled();
    expect(spawnProcess).not.toHaveBeenCalled();
  });

  it("stages exact bytes exclusively and launches the installer directly without a shell", async () => {
    const root = await temporaryRoot();
    const fetchResponse = vi.fn(async () => new Response(contents, {
      status: 200,
      headers: { "content-length": String(contents.byteLength) },
    }));
    const child = childProcess();
    const spawnProcess = vi.fn(() => child.process);
    const runner = new WindowsNsisUpdateRunner(
      root,
      spawnProcess,
      fetchResponse,
      () => "01234567-89ab-cdef-0123-456789abcdef",
      windowsEnvironment,
    );

    await expect(runner.download(expected)).resolves.toBe(true);
    const staged = path.join(root, "GrokDesktop-update-01234567-89ab-cdef-0123-456789abcdef.exe");
    await expect(readFile(staged)).resolves.toEqual(contents);
    const install = runner.install();
    await vi.waitFor(() => expect(spawnProcess).toHaveBeenCalledOnce());
    child.events.emit("spawn");
    await install;

    expect(spawnProcess).toHaveBeenCalledWith(staged, ["--updated"], {
      detached: true,
      shell: false,
      stdio: "ignore",
      windowsHide: false,
      cwd: "C:\\Windows",
      env: {
        APPDATA: windowsEnvironment.APPDATA,
        LOCALAPPDATA: windowsEnvironment.LOCALAPPDATA,
        Path: windowsEnvironment.Path,
        SystemRoot: windowsEnvironment.SystemRoot,
        TEMP: windowsEnvironment.TEMP,
        USERPROFILE: windowsEnvironment.USERPROFILE,
      },
    });
    expect(child.process.unref).toHaveBeenCalledOnce();
    await expect(runner.install()).rejects.toThrow("already starting");
  });

  it("removes a failed partial download and never launches it", async () => {
    const root = await temporaryRoot();
    const fetchResponse = vi.fn(async () => new Response(Buffer.from("tampered"), { status: 200 }));
    const spawnProcess = vi.fn();
    const runner = new WindowsNsisUpdateRunner(root, spawnProcess, fetchResponse, () => "01234567-89ab-cdef-0123-456789abcdef");

    await expect(runner.download(expected)).rejects.toThrow("manifest");
    await expect(readdir(root)).resolves.toEqual([]);
    await expect(runner.install()).rejects.toThrow("not been downloaded");
    expect(spawnProcess).not.toHaveBeenCalled();
  });

  it("revalidates size and digest immediately before execution", async () => {
    const root = await temporaryRoot();
    const fetchResponse = vi.fn(async () => new Response(contents, { status: 200 }));
    const spawnProcess = vi.fn();
    const runner = new WindowsNsisUpdateRunner(root, spawnProcess, fetchResponse, () => "01234567-89ab-cdef-0123-456789abcdef");
    await runner.download(expected);
    const [staged] = await readdir(root);
    if (!staged) throw new Error("missing staged installer");
    await writeFile(path.join(root, staged), Buffer.alloc(contents.byteLength, 0x78));

    await expect(runner.install()).rejects.toThrow("changed before installation");
    expect(spawnProcess).not.toHaveBeenCalled();
  });

  it("does not report a launch until the operating system accepts the process", async () => {
    const root = await temporaryRoot();
    const fetchResponse = vi.fn(async () => new Response(contents, { status: 200 }));
    const child = childProcess();
    const runner = new WindowsNsisUpdateRunner(
      root,
      () => child.process,
      fetchResponse,
      () => "01234567-89ab-cdef-0123-456789abcdef",
      windowsEnvironment,
    );
    await runner.download(expected);
    const install = runner.install();
    await vi.waitFor(() => expect(child.process.listenerCount("error")).toBe(1));
    child.events.emit("error", new Error("launch denied"));
    await expect(install).rejects.toThrow("launch denied");
    expect(child.process.unref).not.toHaveBeenCalled();
  });

  it("prunes owned crash leftovers without deleting unrelated files", async () => {
    const root = await temporaryRoot();
    await writeFile(path.join(root, "GrokDesktop-update-aaaaaaaaaaaaaaaa.exe"), "stale");
    await writeFile(path.join(root, "GrokDesktop-update-bbbbbbbbbbbbbbbb.exe.download"), "partial");
    await writeFile(path.join(root, "operator-note.txt"), "keep");
    const runner = new WindowsNsisUpdateRunner(
      root,
      vi.fn(),
      vi.fn(async () => new Response(contents, { status: 200 })),
      () => "01234567-89ab-cdef-0123-456789abcdef",
    );

    await runner.cleanup();

    expect(await readdir(root)).toEqual(["operator-note.txt"]);
  });

  it("keeps updates available while NSIS temporarily locks its prior staged installer", async () => {
    vi.useFakeTimers();
    const root = await temporaryRoot();
    const stale = path.join(root, "GrokDesktop-update-aaaaaaaaaaaaaaaa.exe");
    await writeFile(stale, "stale");
    let locked = true;
    const removeFile = vi.fn(async (filePath: string) => {
      if (locked) {
        locked = false;
        throw Object.assign(new Error("sharing violation"), { code: "EPERM" });
      }
      await rm(filePath);
    });
    const runner = new WindowsNsisUpdateRunner(
      root,
      vi.fn(),
      vi.fn(async () => new Response(contents, { status: 200 })),
      () => "01234567-89ab-cdef-0123-456789abcdef",
      windowsEnvironment,
      removeFile,
    );

    await expect(runner.cleanup()).resolves.toBeUndefined();
    await expect(lstat(stale)).resolves.toMatchObject({ size: 5 });
    await vi.advanceTimersByTimeAsync(2_000);
    await vi.waitFor(() => expect(removeFile).toHaveBeenCalledTimes(2));
    await expect(lstat(stale)).rejects.toMatchObject({ code: "ENOENT" });
    vi.useRealTimers();
  });
});

describe("Windows update shutdown sequencing", () => {
  it("stops the daemon before launching and always finishes shutdown", async () => {
    const calls: string[] = [];
    await installWindowsUpdateAfterDaemonStop({
      stopDaemon: async () => { calls.push("stop"); },
      install: async () => { calls.push("install"); },
      recoverAfterInstallFailure: () => { calls.push("recover"); },
      finishShutdown: () => { calls.push("finish"); },
    });
    expect(calls).toEqual(["stop", "install", "finish"]);
  });

  it("does not launch or finish shutdown after a failed daemon stop", async () => {
    const install = vi.fn(async () => undefined);
    const finishShutdown = vi.fn();
    await expect(installWindowsUpdateAfterDaemonStop({
      stopDaemon: async () => { throw new Error("stop failed"); },
      install,
      recoverAfterInstallFailure: vi.fn(),
      finishShutdown,
    })).rejects.toThrow("stop failed");
    expect(install).not.toHaveBeenCalled();
    expect(finishShutdown).not.toHaveBeenCalled();
  });

  it("requests a clean app recovery when installer launch fails after daemon stop", async () => {
    const calls: string[] = [];
    await expect(installWindowsUpdateAfterDaemonStop({
      stopDaemon: async () => { calls.push("stop"); },
      install: async () => {
        calls.push("install");
        throw new Error("launch failed");
      },
      recoverAfterInstallFailure: () => { calls.push("recover"); },
      finishShutdown: () => { calls.push("finish"); },
    })).rejects.toThrow("launch failed");
    expect(calls).toEqual(["stop", "install", "recover"]);
  });
});

async function temporaryRoot(): Promise<string> {
  const root = await mkdtemp(path.join(os.tmpdir(), "grok-windows-update-"));
  roots.push(root);
  return root;
}

function childProcess() {
  const events = new EventEmitter();
  const process = Object.assign(events, { unref: vi.fn() });
  return { events, process: process as unknown as import("node:child_process").ChildProcess };
}
