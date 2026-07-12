// @vitest-environment node
import { generateKeyPairSync, sign } from "node:crypto";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { loadUpdateTrust, SignedUpdateManifestAuthorizer } from "./updateManifestVerifier.js";

const roots: string[] = [];
afterEach(async () => Promise.all(roots.splice(0).map((root) => rm(root, { recursive: true, force: true }))));

describe("SignedUpdateManifestAuthorizer", () => {
  it("accepts only a signed exact stable target newer than the running app", async () => {
    const { privateKey, publicKey } = generateKeyPairSync("ed25519");
    const manifest = updateManifest();
    const envelope = {
      manifest,
      signature: {
        algorithm: "ed25519",
        keyId: "stable-2026",
        value: sign(null, Buffer.from(`${JSON.stringify(manifest)}\n`), privateKey).toString("base64"),
      },
    };
    const authorizer = new SignedUpdateManifestAuthorizer({
      platform: "linux",
      architecture: "x64",
      currentVersion: "1.0.0",
      protocolVersion: 23,
      schemaVersion: 23,
      trustedKeys: new Map([["stable-2026", publicKey]]),
    }, async () => Buffer.from(JSON.stringify(envelope)));
    await expect(authorizer.authorize()).resolves.toMatchObject({ available: true, version: "1.1.0" });
  });

  it("fails closed for tamper, target confusion, and newer contract requirements", async () => {
    const { privateKey, publicKey } = generateKeyPairSync("ed25519");
    const manifest = updateManifest();
    const envelope = {
      manifest: { ...manifest, architecture: "arm64", minimumProtocolVersion: 24 },
      signature: {
        algorithm: "ed25519",
        keyId: "stable-2026",
        value: sign(null, Buffer.from(`${JSON.stringify(manifest)}\n`), privateKey).toString("base64"),
      },
    };
    const authorizer = new SignedUpdateManifestAuthorizer({
      platform: "linux", architecture: "x64", currentVersion: "1.0.0",
      protocolVersion: 23, schemaVersion: 23,
      trustedKeys: new Map([["stable-2026", publicKey]]),
    }, async () => Buffer.from(JSON.stringify(envelope)));
    await expect(authorizer.authorize()).rejects.toThrow();
  });

  it("loads a bounded canonical Ed25519 SPKI trust set", async () => {
    const root = await mkdtemp(path.join(os.tmpdir(), "grok-update-trust-"));
    roots.push(root);
    const { publicKey } = generateKeyPairSync("ed25519");
    const trust = path.join(root, "trust.json");
    await writeFile(trust, JSON.stringify({
      "stable-2026": publicKey.export({ format: "der", type: "spki" }).toString("base64"),
    }));
    await expect(loadUpdateTrust(trust)).resolves.toHaveProperty("size", 1);
    await writeFile(trust, JSON.stringify({ "stable-2026": "not-a-key" }));
    await expect(loadUpdateTrust(trust)).rejects.toThrow();
  });
});

function updateManifest() {
  return {
    schemaVersion: 1,
    product: "grok-desktop",
    version: "1.1.0",
    channel: "stable",
    platform: "linux",
    architecture: "x64",
    publishedAt: 1_800_000_000,
    minimumProtocolVersion: 23,
    minimumSchemaVersion: 23,
    rolloutPercentage: 100,
    critical: false,
    artifact: {
      url: "https://github.com/grok-insider/grok-desktop/releases/download/v1.1.0/GrokDesktop-stable-x64.AppImage",
      size: 123,
      sha256: "a".repeat(64),
    },
    releaseNotesUrl: "https://github.com/grok-insider/grok-desktop/releases/download/v1.1.0/release-notes.md",
  };
}
