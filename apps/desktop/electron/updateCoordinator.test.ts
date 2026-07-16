import { createHash } from "node:crypto";
import { describe, expect, it, vi } from "vitest";
import { UpdateCoordinator } from "./updateCoordinator.js";

const updater = () => ({ download: vi.fn(async () => true), install: vi.fn() });

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
    const platformUpdater = updater();
    const coordinator = new UpdateCoordinator({
      packaged: true, platform: "win32", architecture: "x64", version: "1.0.0",
      platformUpdater,
      authorizer: authorizer(),
    });
    coordinator.start();
    vi.advanceTimersByTime(29_999);
    expect(platformUpdater.download).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    await vi.runAllTicks();
    expect(platformUpdater.download).toHaveBeenCalledOnce();
    coordinator.stop();
    vi.useRealTimers();
  });

  it("is honest for development and unsupported platform installs", () => {
    expect(new UpdateCoordinator({
      packaged: false, platform: "linux", architecture: "x64", version: "0.1.0",
    }).getState()).toMatchObject({ phase: "unsupported", reasonCode: "development_install" });
    expect(new UpdateCoordinator({
      packaged: true, platform: "linux", architecture: "x64", version: "0.1.0",
    }).getState()).toMatchObject({ phase: "unsupported", reasonCode: "platform_unsupported" });
  });

  it("downloads only the signed authorized MSIX before offering installation", async () => {
    const platformUpdater = updater();
    const coordinator = new UpdateCoordinator({
      packaged: true, platform: "win32", architecture: "arm64", version: "1.0.0",
      platformUpdater,
      authorizer: authorizer(),
    });
    expect(platformUpdater.download).not.toHaveBeenCalled();
    coordinator.check();
    await vi.waitFor(() => expect(platformUpdater.download).toHaveBeenCalledWith(authorizedUpdate.artifact));
    await vi.waitFor(() => expect(coordinator.getState()).toMatchObject({ phase: "downloaded", targetVersion: "1.1.0" }));
    expect(coordinator.install()).toBe(true);
    expect(platformUpdater.install).toHaveBeenCalledOnce();
  });

  it("persists channel semantics through authorization and resets stale state when changed", async () => {
    const platformUpdater = updater();
    const authorize = vi.fn(async () => authorizedUpdate);
    const coordinator = new UpdateCoordinator({
      packaged: true, platform: "win32", architecture: "x64", version: "1.0.0",
      channel: "beta", authorizer: { authorize }, platformUpdater,
    });
    coordinator.check();
    await vi.waitFor(() => expect(authorize).toHaveBeenCalledWith("beta"));
    expect(coordinator.setChannel("stable")).toMatchObject({
      channel: "stable", phase: "idle", targetVersion: "",
    });
  });

  it("locks the 0.0.z preview line to beta", () => {
    const coordinator = new UpdateCoordinator({
      packaged: true, platform: "win32", architecture: "x64", version: "0.0.1",
      channel: "stable", authorizer: authorizer(), platformUpdater: updater(),
    });
    expect(coordinator.getState().channel).toBe("beta");
    expect(coordinator.setChannel("stable").channel).toBe("beta");
  });

  it("bounds failures without exposing native error details", async () => {
    const platformUpdater = updater();
    platformUpdater.download.mockRejectedValue(new Error("secret native detail"));
    const coordinator = new UpdateCoordinator({
      packaged: true, platform: "win32", architecture: "x64", version: "1.0.0",
      platformUpdater,
      authorizer: authorizer(),
    });
    expect(coordinator.check()).toMatchObject({ phase: "checking" });
    await vi.waitFor(() => expect(coordinator.getState()).toMatchObject({ phase: "failed", reasonCode: "check_failed" }));
    expect(JSON.stringify(coordinator.getState())).not.toContain("secret native detail");
  });

  it("downloads Linux AppImage updates before offering a clean restart", async () => {
    const restart = vi.fn();
    const linuxUpdater = { download: vi.fn(async () => true), install: restart };
    const coordinator = new UpdateCoordinator({
      packaged: true,
      platform: "linux",
      architecture: "x64",
      version: "1.0.0",
      platformUpdater: linuxUpdater,
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
