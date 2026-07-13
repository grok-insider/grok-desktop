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
    await expect(authorizer.authorize("stable")).resolves.toMatchObject({ available: true, version: "1.1.0" });
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
    await expect(authorizer.authorize("stable")).rejects.toThrow();
  });

  it("discovers only an exact signed beta manifest from a GitHub prerelease", async () => {
    const { privateKey, publicKey } = generateKeyPairSync("ed25519");
    const manifest = {
      ...updateManifest(),
      version: "1.2.0-beta.3",
      nativePackageVersion: "1.2.0-beta.3",
      channel: "beta",
      artifact: {
        ...updateManifest().artifact,
        url: "https://github.com/grok-insider/grok-desktop/releases/download/v1.2.0-beta.3/GrokDesktop-beta-x64.AppImage",
      },
    };
    const envelope = {
      manifest,
      signature: {
        algorithm: "ed25519",
        keyId: "stable-2026",
        value: sign(null, Buffer.from(`${JSON.stringify(manifest)}\n`), privateKey).toString("base64"),
      },
    };
    const manifestName = "GrokDesktop-beta-linux-x64.update.json";
    const fetchBytes = async (url: string) => Buffer.from(url.includes("api.github.com")
      ? JSON.stringify([{
          draft: false,
          prerelease: true,
          assets: [{
            name: manifestName,
            browser_download_url: `https://github.com/grok-insider/grok-desktop/releases/download/v1.2.0-beta.3/${manifestName}`,
          }],
        }])
      : JSON.stringify(envelope));
    const authorizer = new SignedUpdateManifestAuthorizer({
      platform: "linux", architecture: "x64", currentVersion: "1.1.0",
      protocolVersion: 29, schemaVersion: 27,
      trustedKeys: new Map([["stable-2026", publicKey]]),
    }, fetchBytes);
    await expect(authorizer.authorize("beta")).resolves.toMatchObject({
      available: true,
      version: "1.2.0-beta.3",
    });
  });

  it("keeps beta installations eligible for a later signed stable release", async () => {
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
      platform: "linux", architecture: "x64", currentVersion: "1.0.0-beta.9",
      protocolVersion: 29, schemaVersion: 27,
      trustedKeys: new Map([["stable-2026", publicKey]]),
    }, async (url) => Buffer.from(url.includes("api.github.com") ? "[]" : JSON.stringify(envelope)));
    await expect(authorizer.authorize("beta")).resolves.toMatchObject({
      available: true,
      version: "1.1.0",
    });
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
    schemaVersion: 2,
    product: "grok-desktop",
    version: "1.1.0",
    nativePackageVersion: "1.1.0",
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
