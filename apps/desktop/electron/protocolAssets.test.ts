// @vitest-environment node
import { mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { resolveProtocolAsset } from "./protocolAssets.js";

describe("application protocol assets", () => {
  const roots: string[] = [];
  afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }); });

  it("serves only files beneath the canonical distribution root with correct MIME", () => {
    const root = fixture();
    writeFileSync(path.join(root, "index.html"), "<html></html>");
    mkdirSync(path.join(root, "assets"));
    writeFileSync(path.join(root, "assets", "app.js"), "export {};");
    expect(resolveProtocolAsset(root, "grok-desktop://app/index.html")?.contentType).toBe("text/html; charset=utf-8");
    expect(resolveProtocolAsset(root, "grok-desktop://app/assets/app.js")?.contentType).toBe("text/javascript; charset=utf-8");
  });

  it("rejects traversal, foreign hosts, missing files, and symlink escape", () => {
    const parent = fixture();
    const root = path.join(parent, "dist");
    mkdirSync(root);
    writeFileSync(path.join(parent, "secret.txt"), "secret");
    symlinkSync(path.join(parent, "secret.txt"), path.join(root, "escape.txt"));
    expect(resolveProtocolAsset(root, "grok-desktop://other/index.html")).toBeNull();
    expect(resolveProtocolAsset(root, "grok-desktop://app/..%2Fsecret.txt")).toBeNull();
    expect(resolveProtocolAsset(root, "grok-desktop://app/escape.txt")).toBeNull();
    expect(resolveProtocolAsset(root, "grok-desktop://app/missing.js")).toBeNull();
  });

  function fixture(): string {
    const root = mkdtempSync(path.join(os.tmpdir(), "grok-protocol-test-"));
    roots.push(root);
    return root;
  }
});
