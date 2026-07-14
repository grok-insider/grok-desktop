import { mkdtemp, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import { loadWindowsUpdateTrust, WindowsMsixUpdateRunner } from "./windowsMsixUpdater.js";

const roots: string[] = [];
const trust = {
  packageIdentity: "GrokInsider.GrokDesktop.Preview",
  publisher: "CN=Grok Desktop Preview",
  signerThumbprint: "A".repeat(40),
};

afterEach(async () => {
  await Promise.all(roots.splice(0).map((root) => rm(root, { recursive: true, force: true })));
});

describe("WindowsMsixUpdateRunner", () => {
  it("loads only exact bounded public Windows update trust", async () => {
    const root = await mkdtemp(path.join(os.tmpdir(), "grok-windows-update-trust-"));
    roots.push(root);
    const file = path.join(root, "trust.json");
    await writeFile(file, JSON.stringify(trust));
    await expect(loadWindowsUpdateTrust(file)).resolves.toEqual(trust);
    await writeFile(file, JSON.stringify({ ...trust, privateKey: "forbidden" }));
    await expect(loadWindowsUpdateTrust(file)).rejects.toThrow("invalid");
  });

  it("rejects an unauthorized artifact before network or installer access", async () => {
    const root = await mkdtemp(path.join(os.tmpdir(), "grok-windows-update-"));
    roots.push(root);
    const openPath = vi.fn(async () => "");
    const runner = new WindowsMsixUpdateRunner(root, trust, openPath);
    await expect(runner.download({
      url: "https://example.com/GrokDesktop-beta-x64.msix",
      size: 1,
      sha256: "a".repeat(64),
    })).rejects.toThrow("URL");
    expect(openPath).not.toHaveBeenCalled();
  });

  it("does not invoke the OS installer without a verified download", async () => {
    const root = await mkdtemp(path.join(os.tmpdir(), "grok-windows-update-"));
    roots.push(root);
    const openPath = vi.fn(async () => "");
    const runner = new WindowsMsixUpdateRunner(root, trust, openPath);
    await expect(runner.install()).rejects.toThrow("not been downloaded");
    expect(openPath).not.toHaveBeenCalled();
  });
});
