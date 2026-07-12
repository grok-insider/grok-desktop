import { EventEmitter } from "node:events";
import { describe, expect, it, vi } from "vitest";
import { UpdateCoordinator, type NativeAutoUpdater } from "./updateCoordinator.js";

class FakeUpdater extends EventEmitter implements NativeAutoUpdater {
  setFeedURL = vi.fn();
  checkForUpdates = vi.fn();
  quitAndInstall = vi.fn();
}

describe("UpdateCoordinator", () => {
  it("checks the stable channel shortly after startup", () => {
    vi.useFakeTimers();
    const updater = new FakeUpdater();
    const coordinator = new UpdateCoordinator(updater, {
      packaged: true, platform: "win32", architecture: "x64", version: "1.0.0",
    });
    coordinator.start();
    vi.advanceTimersByTime(29_999);
    expect(updater.checkForUpdates).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(updater.checkForUpdates).toHaveBeenCalledOnce();
    coordinator.stop();
    vi.useRealTimers();
  });

  it("is honest for development and unsupported platform installs", () => {
    expect(new UpdateCoordinator(undefined, {
      packaged: false, platform: "linux", architecture: "x64", version: "0.1.0",
    }).getState()).toMatchObject({ phase: "unsupported", reasonCode: "development_install" });
    expect(new UpdateCoordinator(undefined, {
      packaged: true, platform: "linux", architecture: "x64", version: "0.1.0",
    }).getState()).toMatchObject({ phase: "unsupported", reasonCode: "platform_unsupported" });
  });

  it("uses only the fixed stable MSIX feed and never enables downgrade", () => {
    const updater = new FakeUpdater();
    const coordinator = new UpdateCoordinator(updater, {
      packaged: true, platform: "win32", architecture: "arm64", version: "1.0.0",
    });
    expect(updater.setFeedURL).toHaveBeenCalledWith({
      url: "https://github.com/grok-insider/grok-desktop/releases/latest/download/GrokDesktop-stable-arm64.msix",
      allowAnyVersion: false,
    });
    coordinator.check();
    expect(updater.checkForUpdates).toHaveBeenCalledOnce();
    updater.emit("update-available", { version: "1.1.0" });
    expect(coordinator.getState()).toMatchObject({ phase: "available", targetVersion: "1.1.0" });
    updater.emit("update-downloaded", {}, "1.1.0");
    expect(coordinator.install()).toBe(true);
    expect(updater.quitAndInstall).toHaveBeenCalledOnce();
  });

  it("bounds failures without exposing native error details", () => {
    const updater = new FakeUpdater();
    updater.checkForUpdates.mockImplementation(() => { throw new Error("secret native detail"); });
    const coordinator = new UpdateCoordinator(updater, {
      packaged: true, platform: "win32", architecture: "x64", version: "1.0.0",
    });
    expect(coordinator.check()).toMatchObject({ phase: "failed", reasonCode: "check_failed" });
    expect(JSON.stringify(coordinator.getState())).not.toContain("secret native detail");
  });

  it("downloads Linux AppImage updates before offering a clean restart", async () => {
    const restart = vi.fn();
    const linuxUpdater = { download: vi.fn(async () => true) };
    const coordinator = new UpdateCoordinator(undefined, {
      packaged: true,
      platform: "linux",
      architecture: "x64",
      version: "1.0.0",
      linuxUpdater,
      restart,
    });
    expect(coordinator.check()).toMatchObject({ phase: "checking" });
    await vi.waitFor(() => expect(coordinator.getState()).toMatchObject({
      phase: "downloaded", targetVersion: "latest",
    }));
    expect(coordinator.install()).toBe(true);
    expect(restart).toHaveBeenCalledOnce();
  });
});
