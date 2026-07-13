import { createPublicKey, sign, verify } from "node:crypto";

export const UPDATE_MANIFEST_SCHEMA_VERSION = 2;
export const UPDATE_CHANNELS = new Set(["beta", "stable"]);
export const UPDATE_PLATFORMS = new Set(["linux", "win32"]);
export const UPDATE_ARCHITECTURES = new Set(["arm64", "x64"]);
const RELEASE_ORIGIN = "https://github.com";
const RELEASE_PATH_PREFIX = "/grok-insider/grok-desktop/releases/download/";
const SHA256_PATTERN = /^[a-f0-9]{64}$/;
const ED25519_SIGNATURE_PATTERN = /^[A-Za-z0-9+/]{86}==$/;
const KEY_ID_PATTERN = /^[a-z0-9][a-z0-9._-]{0,63}$/;
const VERSION_PATTERN = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$/;
const WINDOWS_VERSION_PATTERN = /^(?:0|[1-9]\d*)(?:\.(?:0|[1-9]\d*)){3}$/;

function exactObject(value, keys, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
  const actual = Object.keys(value).toSorted();
  const expected = [...keys].toSorted();
  if (actual.length !== expected.length || actual.some((key, index) => key !== expected[index])) {
    throw new Error(`${label} contains unsupported or missing fields`);
  }
}

function boundedString(value, maximum, label) {
  if (typeof value !== "string" || value.length < 1 || value.length > maximum
      || [...value].some((character) => character.codePointAt(0) < 0x20 || character.codePointAt(0) === 0x7f)) {
    throw new Error(`${label} is invalid`);
  }
  return value;
}

function releaseUrl(value, label) {
  const raw = boundedString(value, 2_048, label);
  const url = new URL(raw);
  if (url.origin !== RELEASE_ORIGIN || !url.pathname.startsWith(RELEASE_PATH_PREFIX) || url.username || url.password || url.search || url.hash) {
    throw new Error(`${label} must use the canonical GitHub Releases origin`);
  }
  return url.toString();
}

export function canonicalUpdateManifestBytes(manifest) {
  const parsed = validateUnsignedUpdateManifest(manifest);
  return Buffer.from(`${JSON.stringify(parsed)}\n`, "utf8");
}

export function validateUnsignedUpdateManifest(value) {
  exactObject(value, [
    "schemaVersion", "product", "version", "nativePackageVersion", "channel", "platform", "architecture",
    "publishedAt", "minimumProtocolVersion", "minimumSchemaVersion", "rolloutPercentage",
    "critical", "artifact", "releaseNotesUrl",
  ], "update manifest");
  if (value.schemaVersion !== UPDATE_MANIFEST_SCHEMA_VERSION || value.product !== "grok-desktop") {
    throw new Error("update manifest identity is unsupported");
  }
  const version = boundedString(value.version, 64, "version");
  if (!VERSION_PATTERN.test(version)) throw new Error("version is not canonical semantic versioning");
  const nativePackageVersion = boundedString(value.nativePackageVersion, 64, "native package version");
  if (!UPDATE_CHANNELS.has(value.channel)) throw new Error("update channel is unsupported");
  if (value.channel === "stable" && version.includes("-")) throw new Error("stable updates cannot be prereleases");
  if (!UPDATE_PLATFORMS.has(value.platform)) throw new Error("update platform is unsupported");
  if ((value.platform === "linux" && nativePackageVersion !== version)
      || (value.platform === "win32" && !WINDOWS_VERSION_PATTERN.test(nativePackageVersion))) {
    throw new Error("native package version is invalid");
  }
  if (!UPDATE_ARCHITECTURES.has(value.architecture)) throw new Error("update architecture is unsupported");
  if (!Number.isSafeInteger(value.publishedAt) || value.publishedAt <= 0) throw new Error("publication time is invalid");
  for (const field of ["minimumProtocolVersion", "minimumSchemaVersion"]) {
    if (!Number.isSafeInteger(value[field]) || value[field] < 1 || value[field] > 1_000_000) {
      throw new Error(`${field} is invalid`);
    }
  }
  if (!Number.isInteger(value.rolloutPercentage) || value.rolloutPercentage < 0 || value.rolloutPercentage > 100) {
    throw new Error("rollout percentage is invalid");
  }
  if (typeof value.critical !== "boolean") throw new Error("critical flag is invalid");
  exactObject(value.artifact, ["url", "size", "sha256"], "update artifact");
  if (!Number.isSafeInteger(value.artifact.size) || value.artifact.size < 1 || value.artifact.size > 8 * 1024 * 1024 * 1024) {
    throw new Error("update artifact size is invalid");
  }
  if (typeof value.artifact.sha256 !== "string" || !SHA256_PATTERN.test(value.artifact.sha256)) {
    throw new Error("update artifact digest is invalid");
  }
  return {
    schemaVersion: value.schemaVersion,
    product: value.product,
    version,
    nativePackageVersion,
    channel: value.channel,
    platform: value.platform,
    architecture: value.architecture,
    publishedAt: value.publishedAt,
    minimumProtocolVersion: value.minimumProtocolVersion,
    minimumSchemaVersion: value.minimumSchemaVersion,
    rolloutPercentage: value.rolloutPercentage,
    critical: value.critical,
    artifact: {
      url: releaseUrl(value.artifact.url, "artifact URL"),
      size: value.artifact.size,
      sha256: value.artifact.sha256,
    },
    releaseNotesUrl: releaseUrl(value.releaseNotesUrl, "release notes URL"),
  };
}

export function signUpdateManifest(manifest, privateKey, keyId) {
  if (typeof keyId !== "string" || !KEY_ID_PATTERN.test(keyId)) throw new Error("update signing key ID is invalid");
  const payload = canonicalUpdateManifestBytes(manifest);
  return {
    manifest: JSON.parse(payload.toString("utf8")),
    signature: { algorithm: "ed25519", keyId, value: sign(null, payload, privateKey).toString("base64") },
  };
}

export function verifySignedUpdateManifest(value, trustedKeys) {
  exactObject(value, ["manifest", "signature"], "signed update manifest");
  exactObject(value.signature, ["algorithm", "keyId", "value"], "update signature");
  if (value.signature.algorithm !== "ed25519" || !KEY_ID_PATTERN.test(value.signature.keyId)) {
    throw new Error("update signature metadata is unsupported");
  }
  const key = trustedKeys.get(value.signature.keyId);
  if (!key) throw new Error("update signature key is not trusted");
  if (typeof value.signature.value !== "string" || !ED25519_SIGNATURE_PATTERN.test(value.signature.value)) {
    throw new Error("update manifest signature is invalid");
  }
  const signature = Buffer.from(value.signature.value, "base64");
  const publicKey = key?.type === "public" ? key : createPublicKey(key);
  if (signature.length !== 64 || !verify(null, canonicalUpdateManifestBytes(value.manifest), publicKey, signature)) {
    throw new Error("update manifest signature is invalid");
  }
  return validateUnsignedUpdateManifest(value.manifest);
}
