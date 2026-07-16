import { createPublicKey, verify, type KeyObject } from "node:crypto";
import { readFile } from "node:fs/promises";

const MAX_MANIFEST_BYTES = 1024 * 1024;
const VERSION_PATTERN = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$/;
const SHA256_PATTERN = /^[a-f0-9]{64}$/;
const KEY_ID_PATTERN = /^[a-z0-9][a-z0-9._-]{0,63}$/;

export interface AuthorizedUpdate {
  available: boolean;
  version: string;
  artifact: {
    kind: "appimage" | "nsis-installer";
    url: string;
    size: number;
    sha256: string;
  };
}

export interface UpdateAuthorizer {
  authorize(channel: "stable" | "beta"): Promise<AuthorizedUpdate>;
}

type FetchBytes = (url: string) => Promise<Uint8Array>;

async function discoverBetaManifest(assetName: string, fetchBytes: FetchBytes): Promise<string> {
  const bytes = await fetchBytes(
    "https://api.github.com/repos/grok-insider/grok-desktop/releases?per_page=20",
  );
  if (bytes.byteLength < 1 || bytes.byteLength > MAX_MANIFEST_BYTES) {
    throw new Error("beta release discovery is unavailable");
  }
  let releases: unknown;
  try { releases = JSON.parse(Buffer.from(bytes).toString("utf8")); } catch {
    throw new Error("beta release discovery is invalid");
  }
  if (!Array.isArray(releases) || releases.length > 20) {
    throw new Error("beta release discovery is invalid");
  }
  for (const candidate of releases) {
    if (!candidate || typeof candidate !== "object" || Array.isArray(candidate)) continue;
    const release = candidate as Record<string, unknown>;
    if (release.draft !== false || release.prerelease !== true || !Array.isArray(release.assets)
        || release.assets.length > 100) continue;
    for (const candidateAsset of release.assets) {
      if (!candidateAsset || typeof candidateAsset !== "object" || Array.isArray(candidateAsset)) continue;
      const asset = candidateAsset as Record<string, unknown>;
      if (asset.name !== assetName || typeof asset.browser_download_url !== "string") continue;
      assertReleaseUrl(asset.browser_download_url);
      if (new URL(asset.browser_download_url).pathname.split("/").at(-1) === assetName) {
        return asset.browser_download_url;
      }
    }
  }
  throw new Error("beta update manifest is unavailable");
}

export class SignedUpdateManifestAuthorizer implements UpdateAuthorizer {
  constructor(
    private readonly options: {
      platform: "linux" | "win32";
      architecture: "arm64" | "x64";
      currentVersion: string;
      protocolVersion: number;
      schemaVersion: number;
      trustedKeys: ReadonlyMap<string, KeyObject>;
    },
    private readonly fetchBytes: FetchBytes = fetchBounded,
  ) {}

  async authorize(channel: "stable" | "beta"): Promise<AuthorizedUpdate> {
    if (channel === "stable") return this.authorizeExact("stable");
    const candidates = await Promise.allSettled([
      this.authorizeExact("beta"),
      this.authorizeExact("stable"),
    ]);
    const authorized = candidates
      .filter((candidate): candidate is PromiseFulfilledResult<AuthorizedUpdate> => candidate.status === "fulfilled")
      .map((candidate) => candidate.value);
    if (authorized.length === 0) throw new Error("beta update manifests are unavailable");
    const eligible = authorized.some((candidate) => candidate.available)
      ? authorized.filter((candidate) => candidate.available)
      : authorized;
    return eligible.reduce((newest, candidate) => (
      compareVersions(candidate.version, newest.version) > 0 ? candidate : newest
    ));
  }

  private async authorizeExact(channel: "stable" | "beta"): Promise<AuthorizedUpdate> {
    const asset = `GrokDesktop-${channel}-${this.options.platform}-${this.options.architecture}.update.json`;
    const manifestUrl = channel === "stable"
      ? `https://github.com/grok-insider/grok-desktop/releases/latest/download/${asset}`
      : await discoverBetaManifest(asset, this.fetchBytes);
    const bytes = await this.fetchBytes(manifestUrl);
    if (bytes.byteLength < 1 || bytes.byteLength > MAX_MANIFEST_BYTES) throw new Error("update manifest is unavailable");
    const envelope = parseObject(Buffer.from(bytes).toString("utf8"), "signed update manifest");
    exactKeys(envelope, ["manifest", "signature"], "signed update manifest");
    const signature = objectValue(envelope.signature, "update signature");
    exactKeys(signature, ["algorithm", "keyId", "value"], "update signature");
    if (signature.algorithm !== "ed25519" || typeof signature.keyId !== "string"
        || !KEY_ID_PATTERN.test(signature.keyId)) throw new Error("update signature is unsupported");
    const key = this.options.trustedKeys.get(signature.keyId);
    if (!key || typeof signature.value !== "string") throw new Error("update signature is untrusted");
    const manifest = validateManifest(envelope.manifest, { ...this.options, channel });
    const signingBytes = Buffer.from(`${JSON.stringify(manifest)}\n`, "utf8");
    const signatureBytes = Buffer.from(signature.value, "base64");
    if (signatureBytes.length !== 64 || !verify(null, signingBytes, key, signatureBytes)) {
      throw new Error("update signature is invalid");
    }
    if (manifest.minimumProtocolVersion > this.options.protocolVersion
        || manifest.minimumSchemaVersion > this.options.schemaVersion) {
      throw new Error("update requires a newer local contract");
    }
    return {
      available: compareVersions(manifest.version, this.options.currentVersion) > 0,
      version: manifest.version,
      artifact: manifest.artifact,
    };
  }
}

export async function loadUpdateTrust(filePath: string): Promise<Map<string, KeyObject>> {
  const raw = await readFile(filePath);
  if (raw.byteLength < 1 || raw.byteLength > 65_536) throw new Error("update trust is unavailable");
  const value = parseObject(raw.toString("utf8"), "update trust");
  const keys = new Map<string, KeyObject>();
  for (const [keyId, encoded] of Object.entries(value)) {
    if (!KEY_ID_PATTERN.test(keyId) || typeof encoded !== "string" || encoded.length > 1024
        || !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(encoded)) {
      throw new Error("update trust contains an invalid key");
    }
    const der = Buffer.from(encoded, "base64");
    const key = createPublicKey({ key: der, format: "der", type: "spki" });
    if (key.asymmetricKeyType !== "ed25519"
        || !Buffer.from(key.export({ format: "der", type: "spki" })).equals(der)) {
      throw new Error("update trust contains an invalid key");
    }
    keys.set(keyId, key);
  }
  if (keys.size < 1 || keys.size > 8) throw new Error("update trust key count is invalid");
  return keys;
}

async function fetchBounded(url: string): Promise<Uint8Array> {
  const response = await fetch(url, { redirect: "follow", signal: AbortSignal.timeout(15_000) });
  if (!response.ok) throw new Error("update manifest request failed");
  const declared = Number(response.headers.get("content-length"));
  if (Number.isFinite(declared) && declared > MAX_MANIFEST_BYTES) throw new Error("update manifest is too large");
  const reader = response.body?.getReader();
  if (!reader) throw new Error("update manifest response is empty");
  const chunks: Uint8Array[] = [];
  let total = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.byteLength;
    if (total > MAX_MANIFEST_BYTES) throw new Error("update manifest is too large");
    chunks.push(value);
  }
  return Buffer.concat(chunks, total);
}

function validateManifest(value: unknown, target: {
  platform: "linux" | "win32"; architecture: "arm64" | "x64"; channel: "stable" | "beta";
}) {
  const manifest = objectValue(value, "update manifest");
  exactKeys(manifest, [
    "schemaVersion", "product", "version", "nativePackageVersion", "channel", "platform", "architecture", "publishedAt",
    "minimumProtocolVersion", "minimumSchemaVersion", "rolloutPercentage", "critical", "artifact",
    "releaseNotesUrl",
  ], "update manifest");
  if (manifest.schemaVersion !== 3 || manifest.product !== "grok-desktop" || manifest.channel !== target.channel
      || manifest.platform !== target.platform || manifest.architecture !== target.architecture
      || typeof manifest.version !== "string" || !VERSION_PATTERN.test(manifest.version)
      || typeof manifest.nativePackageVersion !== "string"
      || manifest.nativePackageVersion !== manifest.version) {
    throw new Error("update manifest target is invalid");
  }
  if (!positiveInteger(manifest.publishedAt) || !positiveInteger(manifest.minimumProtocolVersion)
      || !positiveInteger(manifest.minimumSchemaVersion) || manifest.rolloutPercentage !== 100
      || typeof manifest.critical !== "boolean") throw new Error("update manifest policy is invalid");
  const artifact = objectValue(manifest.artifact, "update artifact");
  exactKeys(artifact, ["kind", "url", "size", "sha256"], "update artifact");
  const expectedKind: AuthorizedUpdate["artifact"]["kind"] = target.platform === "linux"
    ? "appimage"
    : "nsis-installer";
  if (artifact.kind !== expectedKind || typeof artifact.url !== "string" || typeof artifact.sha256 !== "string"
      || !SHA256_PATTERN.test(artifact.sha256) || !positiveInteger(artifact.size)
      || artifact.size > 8 * 1024 * 1024 * 1024) throw new Error("update artifact is invalid");
  assertReleaseUrl(artifact.url);
  if (target.channel === "stable" && manifest.version.includes("-")) {
    throw new Error("stable update manifest cannot be a prerelease");
  }
  const expectedArtifact = target.platform === "linux"
    ? `GrokDesktop-${target.channel}-${target.architecture}.AppImage`
    : `GrokDesktop-${target.channel}-${target.architecture}.exe`;
  if (new URL(artifact.url).pathname
      !== `/grok-insider/grok-desktop/releases/download/v${manifest.version}/${expectedArtifact}`) {
    throw new Error("update artifact target is invalid");
  }
  if (typeof manifest.releaseNotesUrl !== "string") throw new Error("release notes URL is invalid");
  assertReleaseUrl(manifest.releaseNotesUrl);
  return {
    schemaVersion: 3,
    product: "grok-desktop",
    version: manifest.version,
    nativePackageVersion: manifest.nativePackageVersion,
    channel: target.channel,
    platform: target.platform,
    architecture: target.architecture,
    publishedAt: manifest.publishedAt,
    minimumProtocolVersion: manifest.minimumProtocolVersion,
    minimumSchemaVersion: manifest.minimumSchemaVersion,
    rolloutPercentage: 100,
    critical: manifest.critical,
    artifact: {
      kind: expectedKind,
      url: artifact.url,
      size: artifact.size,
      sha256: artifact.sha256,
    },
    releaseNotesUrl: manifest.releaseNotesUrl,
  };
}

function assertReleaseUrl(raw: string): void {
  const url = new URL(raw);
  if (url.origin !== "https://github.com" || !url.pathname.startsWith("/grok-insider/grok-desktop/releases/download/")
      || url.search || url.hash || url.username || url.password) throw new Error("update URL is invalid");
}

function compareVersions(left: string, right: string): number {
  const leftMatch = VERSION_PATTERN.exec(left);
  const rightMatch = VERSION_PATTERN.exec(right);
  if (!leftMatch || !rightMatch) throw new Error("current version is invalid");
  const leftParts = leftMatch.slice(1, 4).map(Number);
  const rightParts = rightMatch.slice(1, 4).map(Number);
  for (let index = 0; index < 3; index += 1) {
    if (leftParts[index] !== rightParts[index]) return leftParts[index] - rightParts[index];
  }
  const leftPrerelease = leftMatch[4]?.split(".");
  const rightPrerelease = rightMatch[4]?.split(".");
  if (!leftPrerelease && !rightPrerelease) return 0;
  if (!leftPrerelease) return 1;
  if (!rightPrerelease) return -1;
  const length = Math.max(leftPrerelease.length, rightPrerelease.length);
  for (let index = 0; index < length; index += 1) {
    const leftPart = leftPrerelease[index];
    const rightPart = rightPrerelease[index];
    if (leftPart === undefined) return -1;
    if (rightPart === undefined) return 1;
    if (leftPart === rightPart) continue;
    const leftNumber = /^\d+$/.test(leftPart) ? Number(leftPart) : undefined;
    const rightNumber = /^\d+$/.test(rightPart) ? Number(rightPart) : undefined;
    if (leftNumber !== undefined && rightNumber !== undefined) return leftNumber - rightNumber;
    if (leftNumber !== undefined) return -1;
    if (rightNumber !== undefined) return 1;
    return leftPart < rightPart ? -1 : 1;
  }
  return 0;
}

function parseObject(raw: string, label: string): Record<string, unknown> {
  try { return objectValue(JSON.parse(raw), label); } catch { throw new Error(`${label} is invalid`); }
}

function objectValue(value: unknown, label: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`${label} is invalid`);
  return value as Record<string, unknown>;
}

function exactKeys(value: Record<string, unknown>, expected: string[], label: string): void {
  const actual = Object.keys(value);
  if (actual.length !== expected.length
      || expected.some((key) => !Object.prototype.hasOwnProperty.call(value, key))) {
    throw new Error(`${label} fields are invalid`);
  }
}

function positiveInteger(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) > 0;
}
