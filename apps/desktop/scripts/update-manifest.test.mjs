import assert from "node:assert/strict";
import { generateKeyPairSync } from "node:crypto";
import { execFile } from "node:child_process";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import test from "node:test";
import {
  canonicalUpdateManifestBytes,
  signUpdateManifest,
  validateUnsignedUpdateManifest,
  verifySignedUpdateManifest,
} from "./update-manifest.mjs";

const baseManifest = () => ({
  schemaVersion: 2,
  product: "grok-desktop",
  version: "1.2.3",
  nativePackageVersion: "1.2.3.65535",
  channel: "stable",
  platform: "win32",
  architecture: "x64",
  publishedAt: 1_784_000_000,
  minimumProtocolVersion: 23,
  minimumSchemaVersion: 23,
  rolloutPercentage: 25,
  critical: false,
  artifact: {
    url: "https://github.com/grok-insider/grok-desktop/releases/download/v1.2.3/GrokDesktop.msix",
    size: 123_456,
    sha256: "a".repeat(64),
  },
  releaseNotesUrl: "https://github.com/grok-insider/grok-desktop/releases/download/v1.2.3/release-notes.md",
});
const execFileAsync = promisify(execFile);

test("canonical update manifests are signed and verified with pinned Ed25519 keys", () => {
  const { privateKey, publicKey } = generateKeyPairSync("ed25519");
  const signed = signUpdateManifest(baseManifest(), privateKey, "release-2026");
  assert.deepEqual(
    verifySignedUpdateManifest(signed, new Map([["release-2026", publicKey]])),
    validateUnsignedUpdateManifest(baseManifest()),
  );
  assert.equal(canonicalUpdateManifestBytes(signed.manifest).at(-1), 0x0a);
});

test("update manifests reject channel confusion, unknown fields, and non-release origins", () => {
  assert.throws(() => validateUnsignedUpdateManifest({ ...baseManifest(), version: "1.2.3-beta.1" }), /stable/);
  assert.throws(() => validateUnsignedUpdateManifest({ ...baseManifest(), surprise: true }), /fields/);
  assert.throws(() => validateUnsignedUpdateManifest({
    ...baseManifest(), artifact: { ...baseManifest().artifact, url: "https://example.com/update.msix" },
  }), /canonical GitHub Releases/);
});

test("update signatures fail closed for tamper and unknown keys", () => {
  const { privateKey, publicKey } = generateKeyPairSync("ed25519");
  const signed = signUpdateManifest(baseManifest(), privateKey, "release-2026");
  assert.throws(() => verifySignedUpdateManifest(signed, new Map()), /not trusted/);
  signed.manifest.rolloutPercentage = 100;
  assert.throws(
    () => verifySignedUpdateManifest(signed, new Map([["release-2026", publicKey]])),
    /signature is invalid/,
  );
});

test("release verifier CLI binds a signed envelope to the requested target", async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), "grok-update-verify-"));
  try {
    const { privateKey, publicKey } = generateKeyPairSync("ed25519");
    const signed = signUpdateManifest(baseManifest(), privateKey, "release-2026");
    const manifestPath = path.join(root, "update.json");
    const trustPath = path.join(root, "trust.json");
    await writeFile(manifestPath, JSON.stringify(signed));
    await writeFile(trustPath, JSON.stringify({
      "release-2026": publicKey.export({ format: "der", type: "spki" }).toString("base64"),
    }));
    const { stdout } = await execFileAsync(process.execPath, [
      new URL("./verify-update-manifest.mjs", import.meta.url).pathname,
      "--manifest", manifestPath,
      "--trust-file", trustPath,
      "--platform", "win32",
      "--architecture", "x64",
      "--version", "1.2.3",
    ]);
    assert.deepEqual(JSON.parse(stdout), { ok: true, version: "1.2.3", platform: "win32", architecture: "x64" });
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
