import assert from "node:assert/strict";
import { createHash, generateKeyPairSync } from "node:crypto";
import { execFile } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
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
  schemaVersion: 3,
  product: "grok-desktop",
  version: "1.2.3",
  nativePackageVersion: "1.2.3",
  channel: "stable",
  platform: "win32",
  architecture: "x64",
  publishedAt: 1_784_000_000,
  minimumProtocolVersion: 23,
  minimumSchemaVersion: 23,
  rolloutPercentage: 25,
  critical: false,
  artifact: {
    kind: "nsis-installer",
    url: "https://github.com/grok-insider/grok-desktop/releases/download/v1.2.3/GrokDesktop-stable-x64.exe",
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
    ...baseManifest(), artifact: { ...baseManifest().artifact, url: "https://example.com/update.exe" },
  }), /canonical GitHub Releases/);
  assert.throws(() => validateUnsignedUpdateManifest({
    ...baseManifest(), artifact: { ...baseManifest().artifact, kind: "appimage" },
  }), /target platform/);
  assert.throws(() => validateUnsignedUpdateManifest({
    ...baseManifest(), artifact: {
      ...baseManifest().artifact,
      url: "https://github.com/grok-insider/grok-desktop/releases/download/v1.2.4/GrokDesktop-stable-x64.exe",
    },
  }), /release target/);
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
      "--channel", "stable",
      "--version", "1.2.3",
    ]);
    assert.deepEqual(JSON.parse(stdout), { ok: true, version: "1.2.3", platform: "win32", architecture: "x64" });
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("release generator emits a schema-v3 manifest bound to an explicit beta NSIS target", async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), "grok-update-generate-"));
  try {
    const { privateKey, publicKey } = generateKeyPairSync("ed25519");
    const artifactPath = path.join(root, "GrokDesktop-beta-x64.exe");
    const privateKeyPath = path.join(root, "private.pem");
    const manifestPath = path.join(root, "GrokDesktop-beta-win32-x64.update.json");
    const trustPath = path.join(root, "trust.json");
    await writeFile(artifactPath, "unsigned installer fixture");
    await writeFile(privateKeyPath, privateKey.export({ format: "pem", type: "pkcs8" }));
    await writeFile(trustPath, JSON.stringify({
      "release-2026": publicKey.export({ format: "der", type: "spki" }).toString("base64"),
    }));
    await execFileAsync(process.execPath, [
      new URL("./generate-update-manifest.mjs", import.meta.url).pathname,
      "--artifact", artifactPath,
      "--artifact-kind", "nsis-installer",
      "--architecture", "x64",
      "--channel", "beta",
      "--key-id", "release-2026",
      "--native-package-version", "1.2.3",
      "--out", manifestPath,
      "--platform", "win32",
      "--private-key", privateKeyPath,
      "--release-notes-url", "https://github.com/grok-insider/grok-desktop/releases/download/v1.2.3/release-notes.md",
      "--artifact-url", "https://github.com/grok-insider/grok-desktop/releases/download/v1.2.3/GrokDesktop-beta-x64.exe",
      "--version", "1.2.3",
    ]);
    const envelope = JSON.parse(await readFile(manifestPath, "utf8"));
    const manifest = verifySignedUpdateManifest(envelope, new Map([["release-2026", publicKey]]));
    assert.equal(manifest.schemaVersion, 3);
    assert.equal(manifest.channel, "beta");
    assert.equal(manifest.artifact.kind, "nsis-installer");
    assert.equal(manifest.artifact.size, Buffer.byteLength("unsigned installer fixture"));
    assert.equal(
      manifest.artifact.sha256,
      createHash("sha256").update("unsigned installer fixture").digest("hex"),
    );
    assert.ok(manifest.publishedAt > 0);
    const { stdout } = await execFileAsync(process.execPath, [
      new URL("./verify-update-manifest.mjs", import.meta.url).pathname,
      "--manifest", manifestPath,
      "--trust-file", trustPath,
      "--platform", "win32",
      "--architecture", "x64",
      "--channel", "beta",
      "--version", "1.2.3",
    ]);
    assert.deepEqual(JSON.parse(stdout), {
      ok: true, version: "1.2.3", platform: "win32", architecture: "x64",
    });
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
