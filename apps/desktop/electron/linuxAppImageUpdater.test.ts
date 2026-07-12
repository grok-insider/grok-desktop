// @vitest-environment node
import { EventEmitter } from "node:events";
import { chmod, mkdtemp, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import { LinuxAppImageUpdateRunner, resolveLinuxUpdateRunner } from "./linuxAppImageUpdater.js";

const roots: string[] = [];
afterEach(async () => Promise.all(roots.splice(0).map((root) => rm(root, { recursive: true, force: true }))));

describe("LinuxAppImageUpdateRunner", () => {
  it("runs only the pinned helper contract and detects an atomic replacement", async () => {
    const root = await mkdtemp(path.join(os.tmpdir(), "grok-appimage-update-"));
    roots.push(root);
    const helper = path.join(root, "helper.AppImage");
    const appImage = path.join(root, "GrokDesktop.AppImage");
    await writeFile(helper, "helper", { mode: 0o755 });
    await writeFile(appImage, "old", { mode: 0o755 });
    const spawnProcess = vi.fn((_executable, arguments_, options) => {
      expect(arguments_).toEqual(["--appimage-extract-and-run", "--overwrite", appImage]);
      expect(options).toMatchObject({ cwd: root, shell: false, stdio: "ignore" });
      expect(options.env).not.toHaveProperty("XAI_API_KEY");
      const child = new EventEmitter();
      queueMicrotask(async () => {
        await writeFile(appImage, "new", { mode: 0o755 });
        child.emit("exit", 0, null);
      });
      return child;
    });
    const runner = new LinuxAppImageUpdateRunner(helper, appImage, spawnProcess as never);
    expect(await runner.download()).toBe(true);
    expect(spawnProcess).toHaveBeenCalledOnce();
  });

  it("fails closed for non-executable inputs and non-AppImage installs", async () => {
    const root = await mkdtemp(path.join(os.tmpdir(), "grok-appimage-update-"));
    roots.push(root);
    const helper = path.join(root, "helper.AppImage");
    const appImage = path.join(root, "GrokDesktop.AppImage");
    await writeFile(helper, "helper", { mode: 0o755 });
    await writeFile(appImage, "app", { mode: 0o755 });
    await chmod(helper, 0o644);
    await expect(new LinuxAppImageUpdateRunner(helper, appImage).download()).rejects.toThrow("unavailable");
    expect(resolveLinuxUpdateRunner({
      packaged: true, platform: "linux", resourcesPath: root,
    })).toBeUndefined();
  });
});
