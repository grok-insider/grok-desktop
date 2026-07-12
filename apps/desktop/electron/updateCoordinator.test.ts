import { EventEmitter } from "node:events";
import { createHash } from "node:crypto";
import { describe, expect, it, vi } from "vitest";
import { UpdateCoordinator, type NativeAutoUpdater } from "./updateCoordinator.js";

class FakeUpdater extends EventEmitter implements NativeAutoUpdater {
  setFeedURL = vi.fn();
  checkForUpdates = vi.fn();
  quitAndInstall = vi.fn();
}

const authorizedUpdate = {
  available: true,
  version: "1.1.0",
  artifact: {
    url: "https://github.com/grok-insider/grok-desktop/releases/download/v1.1.0/app",
    size: 3,
    sha256: createHash("sha256").update("new").digest("hex"),
  },
};
const authorizer = () => ({ authorize: vi.fn(async () => authorizedUpdate) });

describe("UpdateCoordinator", () => {
  it("checks the stable channel shortly after startup", async () => {
    vi.useFakeTimers();
    const updater = new FakeUpdater();
    const coordinator = new UpdateCoordinator(updater, {
      packaged: true, platform: "win32", architecture: "x64", version: "1.0.0",
      authorizer: authorizer(),
    });
    coordinator.start();
    vi.advanceTimersByTime(29_999);
    expect(updater.checkForUpdates).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    await vi.runAllTicks();
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

  it("uses only the fixed stable MSIX feed and never enables downgrade", async () => {
    const updater = new FakeUpdater();
    const coordinator = new UpdateCoordinator(updater, {
      packaged: true, platform: "win32", architecture: "arm64", version: "1.0.0",
      authorizer: authorizer(),
    });
    expect(updater.setFeedURL).toHaveBeenCalledWith({
      url: "https://github.com/grok-insider/grok-desktop/releases/latest/download/GrokDesktop-stable-arm64.msix",
      allowAnyVersion: false,
    });
    coordinator.check();
    await vi.waitFor(() => expect(updater.checkForUpdates).toHaveBeenCalledOnce());
    updater.emit("update-available", { version: "1.1.0" });
    expect(coordinator.getState()).toMatchObject({ phase: "available", targetVersion: "1.1.0" });
    updater.emit("update-downloaded", {}, "1.1.0");
    expect(coordinator.install()).toBe(true);
    expect(updater.quitAndInstall).toHaveBeenCalledOnce();
  });

  it("bounds failures without exposing native error details", async () => {
    const updater = new FakeUpdater();
    updater.checkForUpdates.mockImplementation(() => { throw new Error("secret native detail"); });
    const coordinator = new UpdateCoordinator(updater, {
      packaged: true, platform: "win32", architecture: "x64", version: "1.0.0",
      authorizer: authorizer(),
    });
    expect(coordinator.check()).toMatchObject({ phase: "checking" });
    await vi.waitFor(() => expect(coordinator.getState()).toMatchObject({ phase: "failed", reasonCode: "check_failed" }));
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
      authorizer: authorizer(),
    });
    expect(coordinator.check()).toMatchObject({ phase: "checking" });
    await vi.waitFor(() => expect(coordinator.getState()).toMatchObject({
      phase: "downloaded", targetVersion: "1.1.0",
    }));
    expect(coordinator.install()).toBe(true);
    expect(restart).toHaveBeenCalledOnce();
  });
});
