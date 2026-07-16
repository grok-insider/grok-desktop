import { createHash } from "node:crypto";
import { describe, expect, it, vi } from "vitest";
import { UpdateCoordinator } from "./updateCoordinator.js";

const updater = () => ({ download: vi.fn(async () => true), install: vi.fn() });

const authorizedUpdate = {
  available: true,
  version: "1.1.0",
  artifact: {
    kind: "nsis-installer" as const,
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
    await Promise.resolve();
    await Promise.resolve();
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

  it("downloads only the signed authorized NSIS installer before offering installation", async () => {
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

  it("accepts an update installation only once while shutdown is in flight", async () => {
    const installation = deferred<void>();
    const platformUpdater = {
      download: vi.fn(async () => true),
      install: vi.fn(() => installation.promise),
    };
    const coordinator = new UpdateCoordinator({
      packaged: true, platform: "win32", architecture: "x64", version: "1.0.0",
      platformUpdater,
      authorizer: authorizer(),
    });
    coordinator.check();
    await vi.waitFor(() => expect(coordinator.getState().phase).toBe("downloaded"));

    expect(coordinator.install()).toBe(true);
    expect(coordinator.install()).toBe(false);
    expect(platformUpdater.install).toHaveBeenCalledOnce();
    expect(coordinator.check().phase).toBe("downloaded");

    installation.resolve();
    await installation.promise;
    expect(coordinator.install()).toBe(false);
    expect(platformUpdater.install).toHaveBeenCalledOnce();
  });

  it("serializes channel downloads and binds installation to the latest generation", async () => {
    const stableDownload = deferred<boolean>();
    const betaDownload = deferred<boolean>();
    let stagedChannel = "";
    let installedChannel = "";
    const stableUpdate = {
      ...authorizedUpdate,
      artifact: { ...authorizedUpdate.artifact, url: "https://example.invalid/stable" },
    };
    const betaUpdate = {
      ...authorizedUpdate,
      version: "1.2.0-beta.1",
      artifact: { ...authorizedUpdate.artifact, url: "https://example.invalid/beta" },
    };
    const platformUpdater = {
      download: vi.fn(async (artifact: typeof authorizedUpdate.artifact) => {
        const channel = artifact.url.endsWith("/stable") ? "stable" : "beta";
        const changed = await (channel === "stable" ? stableDownload.promise : betaDownload.promise);
        stagedChannel = channel;
        return changed;
      }),
      install: vi.fn(() => { installedChannel = stagedChannel; }),
    };
    const authorize = vi.fn(async (channel: "stable" | "beta") => (
      channel === "stable" ? stableUpdate : betaUpdate
    ));
    const coordinator = new UpdateCoordinator({
      packaged: true, platform: "win32", architecture: "x64", version: "1.0.0",
      platformUpdater,
      authorizer: { authorize },
    });

    coordinator.check();
    await vi.waitFor(() => expect(platformUpdater.download).toHaveBeenCalledTimes(1));
    expect(coordinator.setChannel("beta")).toMatchObject({ channel: "beta", phase: "idle" });
    coordinator.check();
    await vi.waitFor(() => expect(authorize).toHaveBeenCalledWith("beta"));
    expect(platformUpdater.download).toHaveBeenCalledTimes(1);

    stableDownload.resolve(true);
    await vi.waitFor(() => expect(platformUpdater.download).toHaveBeenCalledTimes(2));
    expect(platformUpdater.download.mock.calls[1]?.[0]).toEqual(betaUpdate.artifact);
    expect(coordinator.install()).toBe(false);

    betaDownload.resolve(true);
    await vi.waitFor(() => expect(coordinator.getState()).toMatchObject({
      channel: "beta", phase: "downloaded", targetVersion: betaUpdate.version,
    }));
    expect(coordinator.install()).toBe(true);
    expect(installedChannel).toBe("beta");
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

function deferred<T>(): { promise: Promise<T>; resolve(value: T): void } {
  let resolvePromise: ((value: T) => void) | undefined;
  const promise = new Promise<T>((resolve) => { resolvePromise = resolve; });
  return {
    promise,
    resolve: (value) => resolvePromise?.(value),
  };
}
