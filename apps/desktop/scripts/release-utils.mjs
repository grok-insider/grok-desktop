import { createHash, createPublicKey, verify as verifyCryptoSignature } from "node:crypto";
import { open, readFile, readdir, realpath, stat } from "node:fs/promises";
import path from "node:path";
import { TextDecoder } from "node:util";

export const RELEASE_ARCHITECTURES = new Set(["x64", "arm64"]);
export const RELEASE_CHANNELS = new Set(["stable", "beta", "canary"]);

const architectureMachines = { x64: 0x8664, arm64: 0xaa64 };
const packageIdentityPattern = /^[A-Za-z0-9][A-Za-z0-9.-]{1,48}[A-Za-z0-9]$/;
const windowsVersionPattern = /^(\d+)\.(\d+)\.(\d+)\.(\d+)$/;
const sha256Pattern = /^[a-f0-9]{64}$/;
const sha1Pattern = /^[A-Fa-f0-9]{40}$/;
const keyIDPattern = /^[A-Za-z0-9._:-]{1,128}$/;
const acpKeyIDPattern = /^[A-Za-z0-9._-]{1,64}$/;
const evidenceIDPattern = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;
const imageVersionPattern = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/;
const guestImageIDPattern = /^[a-z][a-z0-9.-]{0,62}$/;
const guestStagingNamePattern = /^[a-z][a-z0-9.-]{0,120}\.vhdx$/;
const integrationIDPattern = /^[a-z][a-z0-9]*(?:[.-][a-z0-9]+)+$/;
const integrationCapabilityPattern = /^[a-z][a-z0-9-]*(?:\.[a-z0-9-]+)+$/;
const integrationBundlePathPattern = /^[A-Za-z0-9._/-]+$/;
const guestCatalogTrustBindingPrefix = "grok-guest-catalog-trust-v1:";
const acpCatalogTrustBindingPrefix = "grok-acp-catalog-trust-v1:";
const acpPinnedManifestBindingPrefix = "grok-acp-pinned-manifest-v1:";
const acpPinnedManifestSchema = "grok.official-component-pin/v1";
const acpCatalogEnvelopeSchema = "grok.official-component-catalog-envelope/v1";
const acpCatalogPayloadSchema = "grok.official-component-catalog/v1";
const acpCatalogSignatureDomain = Buffer.from("grok.desktop.official-component-catalog.v1\0", "utf8");
const acpCatalogStagePath = "bin/components/grok-acp/catalog.json";
const acpPinnedManifestStagePath = "bin/components/grok-acp/pinned-component.json";
const acpComponentStagePath = "bin/components/grok-acp/bin/grok.exe";
const acpComponentRelativePath = "bin/grok.exe";
const maxAcpCatalogEnvelopeSize = 512 * 1024;
const maxAcpCatalogPayloadSize = 256 * 1024;
const maxAcpComponentSize = 1024 * 1024 * 1024;
const maxAcpPinnedManifestSize = 8 * 1024;
const maxGuestImageSize = 128 * 1024 * 1024 * 1024;
const windowsReservedNames = new Set([
  "CON", "PRN", "AUX", "NUL",
  "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
  "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
]);
const ambientSigningSecretVariables = [
  "CERTIFICATE_FILE",
  "CERTIFICATE_PASSWORD",
  "CSC_KEY_PASSWORD",
  "CSC_LINK",
  "PFX_FILE",
  "PFX_PASSWORD",
  "SIGNTOOL_CERTIFICATE_FILE",
  "SIGNTOOL_CERTIFICATE_PASSWORD",
  "WINDOWS_CERTIFICATE_FILE",
  "WINDOWS_CERTIFICATE_PASSWORD",
  "WINDOWS_SIGN_WITH_PARAMS",
  "WIN_CSC_KEY_PASSWORD",
  "WIN_CSC_LINK",
];
const ambientSigningSecretVariableSet = new Set(ambientSigningSecretVariables);
const expectedInputLimits = new Map([
  ["bin/grok-daemon.exe", 128 * 1024 * 1024],
  ["bin/grok-host-tools-mcp.exe", 128 * 1024 * 1024],
  ["service/grok-vm-service.exe", 64 * 1024 * 1024],
  ["guest/grok-guest.vhdx", 32 * 1024 * 1024 * 1024],
  ["guest/grok-guest.vhdx.sha256", 256],
  ["catalog/components.json", 1024 * 1024],
  ["catalog/integrations.json", 1024 * 1024],
  [acpCatalogStagePath, maxAcpCatalogEnvelopeSize],
  [acpComponentStagePath, maxAcpComponentSize],
]);
const coreWindowsInputLimits = new Map([
  ["bin/grok-daemon.exe", 128 * 1024 * 1024],
  ["bin/grok-host-tools-mcp.exe", 128 * 1024 * 1024],
  [acpPinnedManifestStagePath, maxAcpPinnedManifestSize],
  [acpComponentStagePath, maxAcpComponentSize],
]);

export function parseReleaseArguments(releaseArguments) {
  const result = {};
  for (let index = 0; index < releaseArguments.length; index += 2) {
    const option = releaseArguments[index];
    const value = releaseArguments[index + 1];
    if (!option?.startsWith("--") || value === undefined) throw new Error("release arguments must be option/value pairs");
    if (option !== "--arch" && option !== "--channel" && option !== "--stage" && option !== "--out") {
      throw new Error(`unsupported release option ${option}`);
    }
    if (result[option]) throw new Error(`release option ${option} was repeated`);
    result[option] = value;
  }
  if (!RELEASE_ARCHITECTURES.has(result["--arch"])) throw new Error("--arch must be x64 or arm64");
  if (!RELEASE_CHANNELS.has(result["--channel"])) throw new Error("--channel must be stable, beta, or canary");
  return { architecture: result["--arch"], channel: result["--channel"], stage: result["--stage"], out: result["--out"] };
}

export function normalizeMsixVersion(version, channel = "stable") {
  const match = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z.-]+))?$/.exec(version);
  if (!match) throw new Error("application version is not valid semantic versioning");
  const parts = match.slice(1, 4).map(Number);
  if (parts.some((part) => part > 65_535)) throw new Error("application version exceeds the MSIX component limit");
  const prerelease = match[4];
  const patchPreview = channel === "beta" && parts[0] === 0 && parts[1] === 0 && !prerelease;
  if (patchPreview) return `${parts[0]}.${parts[1]}.${parts[2]}.1`;
  if (channel === "stable") {
    if (prerelease) throw new Error("stable releases cannot use a prerelease version");
    return `${parts[0]}.${parts[1]}.${parts[2]}.65535`;
  }
  if (channel !== "beta" || !prerelease) {
    throw new Error("beta releases require a prerelease version");
  }
  const ordinal = /(?:^|\.)(\d+)$/.exec(prerelease)?.[1];
  if (!ordinal || Number(ordinal) < 1 || Number(ordinal) > 65_534) {
    throw new Error("beta prerelease must end in an ordinal from 1 through 65534");
  }
  return `${parts[0]}.${parts[1]}.${parts[2]}.${Number(ordinal)}`;
}

export function readReleaseEnvironment(environment) {
  const base = readCoreWindowsReleaseEnvironment(environment);
  const releaseMetadataKeys = parseReleaseMetadataKeys(
    boundedEnvironment(environment, "GROK_RELEASE_METADATA_PUBLIC_KEYS_JSON", 65_536),
  );
  const acpCatalogTrust = parseAcpCatalogTrustedKeys(
    boundedEnvironment(environment, "GROK_ACP_CATALOG_TRUSTED_KEYS", 4096),
  );
  return {
    ...base,
    releaseMetadataKeys,
    acpCatalogTrust,
  };
}

export function readCoreWindowsReleaseEnvironment(environment) {
  rejectAmbientSigningSecrets(environment);
  const packageIdentity = boundedEnvironment(environment, "GROK_MSIX_IDENTITY", 50);
  const publisher = boundedEnvironment(environment, "GROK_MSIX_PUBLISHER", 512);
  const publisherDisplayName = boundedEnvironment(environment, "GROK_MSIX_PUBLISHER_DISPLAY_NAME", 128);
  const maxTestedVersion = boundedEnvironment(environment, "GROK_WINDOWS_MAX_TESTED_VERSION", 32);
  const signToolPath = boundedEnvironment(environment, "GROK_WINDOWS_SIGNTOOL_PATH", 1024);
  const powershellPath = boundedEnvironment(environment, "GROK_WINDOWS_POWERSHELL_PATH", 1024);
  const timestampServer = boundedEnvironment(environment, "GROK_WINDOWS_TIMESTAMP_SERVER", 2048);
  const signerThumbprint = normalizeThumbprint(boundedEnvironment(environment, "GROK_WINDOWS_SIGNER_SHA1", 40));
  const signingArguments = parseSigningArguments(
    boundedEnvironment(environment, "GROK_WINDOWS_SIGN_ARGS_JSON", 8192),
    signerThumbprint,
  );
  const updateTrustedKeysJSON = boundedEnvironment(
    environment, "GROK_UPDATE_TRUSTED_KEYS_JSON", 65_536,
  );
  const acpProvenanceEvidenceID = boundedEnvironment(
    environment, "GROK_XAI_COMPONENT_PROVENANCE_EVIDENCE_ID", 128,
  );
  const acpRedistributionEvidenceID = boundedEnvironment(
    environment, "GROK_XAI_COMPONENT_REDISTRIBUTION_EVIDENCE_ID", 128,
  );
  parseReleaseMetadataKeys(updateTrustedKeysJSON);
  if (!packageIdentityPattern.test(packageIdentity)) throw new Error("GROK_MSIX_IDENTITY is invalid");
  if (!publisher.startsWith("CN=") || hasInvalidXmlCharacters(publisher)) throw new Error("GROK_MSIX_PUBLISHER is invalid");
  if (hasInvalidXmlCharacters(publisherDisplayName)) throw new Error("GROK_MSIX_PUBLISHER_DISPLAY_NAME is invalid");
  if (!evidenceIDPattern.test(acpProvenanceEvidenceID) ||
      !evidenceIDPattern.test(acpRedistributionEvidenceID)) {
    throw new Error("official Grok component evidence identifiers are invalid");
  }
  const maxVersion = parseWindowsVersion(maxTestedVersion);
  if (compareVersions(maxVersion, [10, 0, 22_000, 0]) < 0) throw new Error("the tested Windows version predates Windows 11");
  if (!path.win32.isAbsolute(signToolPath)) throw new Error("GROK_WINDOWS_SIGNTOOL_PATH must be an absolute Windows path");
  if (!path.win32.isAbsolute(powershellPath)) throw new Error("GROK_WINDOWS_POWERSHELL_PATH must be an absolute Windows path");
  const timestampUrl = new URL(timestampServer);
  if (timestampUrl.protocol !== "https:" || timestampUrl.username || timestampUrl.password) {
    throw new Error("GROK_WINDOWS_TIMESTAMP_SERVER must be an unauthenticated HTTPS URL");
  }
  return {
    packageIdentity,
    publisher,
    publisherDisplayName,
    maxTestedVersion,
    signToolPath,
    powershellPath,
    timestampServer,
    signerThumbprint,
    signingArguments,
    updateTrustedKeysJSON,
    acpProvenanceEvidenceID,
    acpRedistributionEvidenceID,
  };
}

export function parseReleaseMetadataKeys(raw) {
  let value;
  try { value = JSON.parse(raw); } catch { throw new Error("GROK_RELEASE_METADATA_PUBLIC_KEYS_JSON must be a JSON object"); }
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("GROK_RELEASE_METADATA_PUBLIC_KEYS_JSON must be a JSON object");
  }
  const entries = Object.entries(value);
  if (entries.length < 1 || entries.length > 16) throw new Error("release metadata trust must contain 1 to 16 keys");
  const keys = new Map();
  for (const [keyID, encoded] of entries) {
    if (!keyIDPattern.test(keyID) || typeof encoded !== "string" || encoded.length > 1024) {
      throw new Error("release metadata trust contains an invalid key record");
    }
    const der = decodeCanonicalBase64(encoded, "release metadata public key");
    let publicKey;
    try {
      publicKey = createPublicKey({ key: der, format: "der", type: "spki" });
    } catch {
      throw new Error("release metadata public key is not valid SPKI");
    }
    const canonicalDER = publicKey.export({ format: "der", type: "spki" });
    if (publicKey.asymmetricKeyType !== "ed25519" || !canonicalDER.equals(der)) {
      throw new Error("release metadata public key must use canonical Ed25519 SPKI");
    }
    keys.set(keyID, publicKey);
  }
  return keys;
}

export function parseAcpCatalogTrustedKeys(raw) {
  if (typeof raw !== "string" || raw.length < 1 || raw.length > 4096 || raw.includes("\0")) {
    throw new Error("GROK_ACP_CATALOG_TRUSTED_KEYS is required and bounded");
  }
  const records = raw.split(";");
  if (records.length < 1 || records.length > 16) {
    throw new Error("ACP catalog trust must contain 1 to 16 public keys");
  }
  const keys = new Map();
  let previousKeyID = "";
  for (const record of records) {
    const separator = record.indexOf("=");
    if (separator < 1 || separator !== record.lastIndexOf("=")) {
      throw new Error("ACP catalog trust contains an invalid public key record");
    }
    const keyID = record.slice(0, separator);
    const encoded = record.slice(separator + 1);
    if (!acpKeyIDPattern.test(keyID) || !/^[a-f0-9]{64}$/.test(encoded) ||
        (previousKeyID && keyID <= previousKeyID) || keys.has(keyID)) {
      throw new Error("ACP catalog trust must be unique and ordered by key ID");
    }
    const rawKey = Buffer.from(encoded, "hex");
    const spki = Buffer.concat([Buffer.from("302a300506032b6570032100", "hex"), rawKey]);
    let publicKey;
    try { publicKey = createPublicKey({ key: spki, format: "der", type: "spki" }); } catch {
      throw new Error("ACP catalog trust contains an invalid Ed25519 public key");
    }
    if (publicKey.asymmetricKeyType !== "ed25519") {
      throw new Error("ACP catalog trust must contain only Ed25519 public keys");
    }
    keys.set(keyID, publicKey);
    previousKeyID = keyID;
  }
  const binding = acpCatalogTrustBindingPrefix + createHash("sha256").update(raw, "utf8").digest("hex");
  return { raw, keys, binding };
}

export function verifyOfficialGrokPinnedManifestBytes(contents, architecture, operatingSystem) {
  if (!Buffer.isBuffer(contents) || contents.length < 1 || contents.length > maxAcpPinnedManifestSize) {
    throw new Error("official Grok pinned manifest is not bounded");
  }
  let manifest;
  try { manifest = JSON.parse(contents.toString("utf8")); } catch {
    throw new Error("official Grok pinned manifest is invalid JSON");
  }
  if (!manifest || typeof manifest !== "object" || Array.isArray(manifest) ||
      `${JSON.stringify(manifest)}\n` !== contents.toString("utf8")) {
    throw new Error("official Grok pinned manifest must use canonical tracked JSON");
  }
  const expectedKeys = [
    "schema", "name", "publisher", "version", "os", "architecture",
    "executable", "sourceUrl", "sha256", "size",
  ];
  if (Object.keys(manifest).join("\0") !== expectedKeys.join("\0") ||
      manifest.schema !== acpPinnedManifestSchema || manifest.name !== "grok-build" ||
      manifest.publisher !== "xAI" ||
      !/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?$/.test(manifest.version) ||
      !sha256Pattern.test(manifest.sha256) || !Number.isSafeInteger(manifest.size) ||
      manifest.size < 1 || manifest.size > maxAcpComponentSize) {
    throw new Error("official Grok pinned manifest fields are invalid");
  }
  const target = {
    "linux:x64": { os: "linux", architecture: "x86_64", executable: "bin/grok", suffix: "linux-x86_64" },
    "windows:x64": { os: "windows", architecture: "x86_64", executable: "bin/grok.exe", suffix: "windows-x86_64.exe" },
  }[`${operatingSystem}:${architecture}`];
  if (!target || manifest.os !== target.os || manifest.architecture !== target.architecture ||
      manifest.executable !== target.executable) {
    throw new Error("official Grok pinned manifest target does not match the package");
  }
  let source;
  try { source = new URL(manifest.sourceUrl); } catch {
    throw new Error("official Grok pinned manifest source URL is invalid");
  }
  if (source.protocol !== "https:" || source.hostname !== "x.ai" || source.port ||
      source.username || source.password || source.search || source.hash ||
      source.pathname !== `/cli/grok-${manifest.version}-${target.suffix}`) {
    throw new Error("official Grok pinned manifest source URL is invalid");
  }
  const binding = acpPinnedManifestBindingPrefix + createHash("sha256").update(contents).digest("hex");
  return { ...manifest, binding };
}

export function inspectDaemonAcpPinnedManifestBytes(contents, manifest) {
  if (!Buffer.isBuffer(contents) || !manifest ||
      !contents.includes(Buffer.from(manifest.binding, "utf8"))) {
    throw new Error("daemon was not built with the staged pinned ACP manifest binding");
  }
  return manifest.binding;
}

export function serviceGuestCatalogTrust(trustedKeys) {
  if (!(trustedKeys instanceof Map) || trustedKeys.size < 1 || trustedKeys.size > 16) {
    throw new Error("trusted guest catalog keys are required");
  }
  const rawKeys = {};
  for (const [keyID, publicKey] of [...trustedKeys].toSorted(([left], [right]) => left.localeCompare(right))) {
    const jwk = publicKey.export({ format: "jwk" });
    const raw = typeof jwk.x === "string" ? Buffer.from(jwk.x, "base64url") : Buffer.alloc(0);
    if (!keyIDPattern.test(keyID) || publicKey.asymmetricKeyType !== "ed25519" || raw.length !== 32) {
      throw new Error("guest catalog trust contains an invalid Ed25519 key");
    }
    rawKeys[keyID] = raw.toString("base64");
  }
  const json = JSON.stringify(rawKeys);
  const encoded = Buffer.from(json, "utf8").toString("base64");
  const binding = guestCatalogTrustBindingPrefix + createHash("sha256").update(encoded, "utf8").digest("hex");
  return { json, encoded, binding };
}

export function windowsServiceBuildMetadata(version, trustedKeys) {
  if (!validImageVersion(version)) throw new Error("VM service version must be valid semantic versioning");
  const trust = serviceGuestCatalogTrust(trustedKeys);
  const linkerFlags = [
    "-s",
    "-w",
    `-X=main.version=${version}`,
    `-X=main.guestCatalogTrust=${trust.encoded}`,
    `-X=main.guestCatalogTrustBinding=${trust.binding}`,
  ].join(" ");
  return { trust, linkerFlags };
}

export function renderManifest(template, values) {
  let output = template;
  for (const [name, value] of Object.entries(values)) {
    if (typeof value !== "string" || value.length === 0 || hasInvalidXmlCharacters(value)) throw new Error(`manifest value ${name} is invalid`);
    const token = `@@${name}@@`;
    if (!output.includes(token)) throw new Error(`manifest template does not contain ${token}`);
    output = output.replaceAll(token, escapeXml(value));
  }
  const unresolved = output.match(/@@[A-Z_]+@@/g);
  if (unresolved) throw new Error(`manifest template contains unresolved token ${unresolved[0]}`);
  return output;
}

export function renderStableAppInstaller({ architecture, packageIdentity, publisher, version }) {
  if (!RELEASE_ARCHITECTURES.has(architecture)) throw new Error("unsupported App Installer architecture");
  if (!packageIdentityPattern.test(packageIdentity) || !publisher.startsWith("CN=")
      || hasInvalidXmlCharacters(publisher)) {
    throw new Error("App Installer package identity is invalid");
  }
  if (!windowsVersionPattern.test(version)) throw new Error("App Installer version is invalid");
  const assetName = `GrokDesktop-stable-${architecture}.msix`;
  const base = "https://github.com/grok-insider/grok-desktop/releases/latest/download";
  return `<?xml version="1.0" encoding="utf-8"?>\n<AppInstaller xmlns="http://schemas.microsoft.com/appx/appinstaller/2021" Version="${escapeXml(version)}" Uri="${base}/GrokDesktop-stable-${architecture}.appinstaller">\n  <MainPackage Name="${escapeXml(packageIdentity)}" Publisher="${escapeXml(publisher)}" Version="${escapeXml(version)}" ProcessorArchitecture="${architecture}" Uri="${base}/${assetName}" />\n  <UpdateSettings>\n    <OnLaunch HoursBetweenUpdateChecks="6" ShowPrompt="true" UpdateBlocksActivation="false" />\n    <AutomaticBackgroundTask />\n  </UpdateSettings>\n</AppInstaller>\n`;
}

export function renderPreviewAppInstaller({ architecture, packageIdentity, publisher, version, releaseTag }) {
  if (!RELEASE_ARCHITECTURES.has(architecture)) throw new Error("unsupported App Installer architecture");
  if (!packageIdentityPattern.test(packageIdentity) || !publisher.startsWith("CN=")
      || hasInvalidXmlCharacters(publisher)) throw new Error("App Installer package identity is invalid");
  if (!windowsVersionPattern.test(version) || !/^v0\.0\.(0|[1-9]\d*)$/.test(releaseTag)) {
    throw new Error("preview App Installer version is invalid");
  }
  const base = `https://github.com/grok-insider/grok-desktop/releases/download/${releaseTag}`;
  return `<?xml version="1.0" encoding="utf-8"?>\n<AppInstaller xmlns="http://schemas.microsoft.com/appx/appinstaller/2021" Version="${escapeXml(version)}" Uri="${base}/GrokDesktop-beta-${architecture}.appinstaller">\n  <MainPackage Name="${escapeXml(packageIdentity)}" Publisher="${escapeXml(publisher)}" Version="${escapeXml(version)}" ProcessorArchitecture="${architecture}" Uri="${base}/GrokDesktop-beta-${architecture}.msix" />\n</AppInstaller>\n`;
}

export async function validateReleaseInputs(stageRoot, expected) {
  const { architecture, channel, desktopVersion, releaseMetadataKeys, acpCatalogTrust, nowUnixSeconds } = expected ?? {};
  if (!RELEASE_ARCHITECTURES.has(architecture)) throw new Error("unsupported release architecture");
  if (!RELEASE_CHANNELS.has(channel)) throw new Error("unsupported release channel");
  if (typeof desktopVersion !== "string" || desktopVersion.length === 0 || desktopVersion.length > 64) {
    throw new Error("expected desktop version is invalid");
  }
  if (!(releaseMetadataKeys instanceof Map) || releaseMetadataKeys.size === 0) {
    throw new Error("trusted release metadata keys are required");
  }
  assertAcpCatalogTrust(acpCatalogTrust);
  const canonicalRoot = await realpath(stageRoot);
  const manifestPath = await containedRegularFile(canonicalRoot, "release-inputs.json", 1024 * 1024);
  const rawManifest = await readFile(manifestPath, "utf8");
  const manifest = parseInputManifest(rawManifest, { architecture, channel, desktopVersion });
  verifyReleaseInputSignature(manifest, releaseMetadataKeys);
  const expectedPaths = [...expectedInputLimits.keys()].toSorted();
  if (manifest.files.length !== expectedPaths.length) throw new Error("release input manifest has an unexpected file set");
  const stagedPaths = await listStageFiles(canonicalRoot);
  const allowedPaths = ["release-inputs.json", ...expectedPaths].toSorted();
  if (stagedPaths.length !== allowedPaths.length || stagedPaths.some((candidate, index) => candidate !== allowedPaths[index])) {
    throw new Error("release staging directory contains an unexpected file or directory");
  }

  const verified = new Map();
  for (const expectedPath of expectedPaths) {
    const record = manifest.files.find((candidate) => candidate.path === expectedPath);
    if (!record) throw new Error("release input manifest is incomplete");
    const file = await containedRegularFile(canonicalRoot, expectedPath, expectedInputLimits.get(expectedPath));
    const metadata = await stat(file);
    if (metadata.size !== record.size) throw new Error("release input size does not match its manifest");
    const digest = await sha256File(file);
    if (digest !== record.sha256) throw new Error("release input digest does not match its manifest");
    verified.set(expectedPath, file);
  }

  const guestRecord = manifest.files.find((record) => record.path === manifest.guest.path);
  if (!guestRecord || guestRecord.sha256 !== manifest.guest.sha256 || guestRecord.size !== manifest.guest.size) {
    throw new Error("signed guest metadata does not match the release inventory");
  }
  await inspectPortableExecutable(verified.get("bin/grok-daemon.exe"), architecture);
  await inspectPortableExecutable(verified.get("bin/grok-host-tools-mcp.exe"), architecture);
  await inspectDaemonAcpCatalogTrust(verified.get("bin/grok-daemon.exe"), acpCatalogTrust);
  await inspectPortableExecutable(verified.get("service/grok-vm-service.exe"), architecture);
  await inspectServiceGuestCatalogTrust(verified.get("service/grok-vm-service.exe"), releaseMetadataKeys);
  await verifyVhdx(verified.get("guest/grok-guest.vhdx"), verified.get("guest/grok-guest.vhdx.sha256"));
  const guestCatalog = await verifyGuestImageCatalog(
    verified.get("catalog/components.json"), architecture, releaseMetadataKeys,
  );
  crossCheckGuestMetadata(manifest, guestCatalog);
  await verifyIntegrationCatalogJSON(verified.get("catalog/integrations.json"));
  const acpCatalog = await verifyOfficialGrokCatalog(
    verified.get(acpCatalogStagePath), architecture, acpCatalogTrust, nowUnixSeconds,
  );
  const acpRecord = manifest.files.find((record) => record.path === acpComponentStagePath);
  if (!acpRecord || acpRecord.sha256 !== acpCatalog.component.sha256 ||
      acpRecord.size !== acpCatalog.component.size) {
    throw new Error("official Grok component catalog does not match the release inventory");
  }
  await inspectPortableExecutable(verified.get(acpComponentStagePath), architecture);
  return {
    canonicalRoot,
    files: verified,
    manifest,
    guestCatalog,
    acpCatalog,
    acpCatalogTrust,
    acpComponent: { ...acpCatalog.component, stagePath: acpComponentStagePath },
  };
}

export async function validateCoreWindowsInputs(stageRoot, { architecture } = {}) {
  if (architecture !== "x64") throw new Error("core Windows beta supports only x64");
  const canonicalRoot = await realpath(stageRoot);
  const expectedPaths = [...coreWindowsInputLimits.keys()].toSorted();
  const stagedPaths = await listStageFiles(canonicalRoot);
  if (stagedPaths.length !== expectedPaths.length ||
      stagedPaths.some((candidate, index) => candidate !== expectedPaths[index])) {
    throw new Error("core Windows staging directory contains an unexpected file or directory");
  }
  const files = new Map();
  for (const relativePath of expectedPaths) {
    files.set(relativePath, await containedRegularFile(
      canonicalRoot, relativePath, coreWindowsInputLimits.get(relativePath),
    ));
  }
  const manifestBytes = await readFile(files.get(acpPinnedManifestStagePath));
  const manifest = verifyOfficialGrokPinnedManifestBytes(manifestBytes, architecture, "windows");
  const componentMetadata = await stat(files.get(acpComponentStagePath));
  if (componentMetadata.size !== manifest.size ||
      await sha256File(files.get(acpComponentStagePath)) !== manifest.sha256) {
    throw new Error("pinned ACP manifest does not match the staged Windows component");
  }
  await inspectPortableExecutable(files.get("bin/grok-daemon.exe"), architecture);
  await inspectPortableExecutable(files.get("bin/grok-host-tools-mcp.exe"), architecture);
  await inspectPortableExecutable(files.get(acpComponentStagePath), architecture);
  inspectDaemonAcpPinnedManifestBytes(await readFile(files.get("bin/grok-daemon.exe")), manifest);
  return { canonicalRoot, files, manifest };
}

export function releaseInputSigningBytes(manifest) {
  const payload = {
    version: manifest.version,
    product: manifest.product,
    architecture: manifest.architecture,
    channel: manifest.channel,
    desktopVersion: manifest.desktopVersion,
    sequence: manifest.sequence,
    guest: {
      imageId: manifest.guest.imageId,
      imageVersion: manifest.guest.imageVersion,
      stagingName: manifest.guest.stagingName,
      path: manifest.guest.path,
      sha256: manifest.guest.sha256,
      size: manifest.guest.size,
    },
    files: manifest.files.map((record) => ({ path: record.path, sha256: record.sha256, size: record.size })),
    signature: { algorithm: manifest.signature.algorithm, keyId: manifest.signature.keyId },
  };
  return Buffer.from(`${JSON.stringify(payload)}\n`, "utf8");
}

export function guestImageCatalogSigningBytes(catalog) {
  const payload = {
    schemaVersion: catalog.schemaVersion,
    product: catalog.product,
    architecture: catalog.architecture,
    sequence: catalog.sequence,
    images: catalog.images.map((image) => ({
      id: image.id,
      version: image.version,
      stagingName: image.stagingName,
      sha256: image.sha256,
      sizeBytes: image.sizeBytes,
    })),
    signature: { algorithm: catalog.signature.algorithm, keyId: catalog.signature.keyId },
  };
  return Buffer.from(`${JSON.stringify(payload)}\n`, "utf8");
}

export function officialGrokCatalogSignatureBytes(keyID, payload) {
  if (!acpKeyIDPattern.test(keyID) || !Buffer.isBuffer(payload) ||
      payload.length < 1 || payload.length > maxAcpCatalogPayloadSize) {
    throw new Error("official Grok catalog signature input is invalid");
  }
  const keyLength = Buffer.alloc(2);
  keyLength.writeUInt16BE(Buffer.byteLength(keyID, "utf8"));
  return Buffer.concat([acpCatalogSignatureDomain, keyLength, Buffer.from(keyID, "utf8"), payload]);
}

export function validateSignerIdentity(identity, expectedSubject, expectedThumbprint) {
  if (!identity || typeof identity !== "object" || Array.isArray(identity) ||
      Object.keys(identity).toSorted().join(",") !== "subject,thumbprint" ||
      typeof identity.subject !== "string" || typeof identity.thumbprint !== "string") {
    throw new Error("signature inspection returned an invalid signer identity");
  }
  const thumbprint = normalizeThumbprint(identity.thumbprint);
  if (identity.subject !== expectedSubject) throw new Error("artifact signer subject does not match the expected publisher");
  if (thumbprint !== normalizeThumbprint(expectedThumbprint)) throw new Error("artifact signer thumbprint does not match the release certificate");
  return { subject: identity.subject, thumbprint };
}

export function createSigningToolEnvironment(environment, additions = {}) {
  const safe = {};
  for (const name of [
    "APPDATA", "LOCALAPPDATA", "ProgramData", "ProgramFiles", "ProgramFiles(x86)",
    "SystemRoot", "TEMP", "TMP", "USERPROFILE", "WINDIR",
  ]) {
    if (typeof environment[name] === "string") safe[name] = environment[name];
  }
  for (const [name, value] of Object.entries(additions)) {
    if (ambientSigningSecretVariableSet.has(name.toUpperCase()) || typeof value !== "string" || value.includes("\0")) {
      throw new Error("signing subprocess environment addition is invalid");
    }
    safe[name] = value;
  }
  return safe;
}

export async function inspectPortableExecutable(file, architecture) {
  const handle = await open(file, "r");
  try {
    const dosHeader = Buffer.alloc(64);
    if ((await handle.read(dosHeader, 0, dosHeader.length, 0)).bytesRead !== dosHeader.length || dosHeader.toString("ascii", 0, 2) !== "MZ") {
      throw new Error("release executable has an invalid DOS header");
    }
    const peOffset = dosHeader.readUInt32LE(0x3c);
    if (peOffset < 64 || peOffset > 16 * 1024 * 1024) throw new Error("release executable has an invalid PE offset");
    const peHeader = Buffer.alloc(6);
    if ((await handle.read(peHeader, 0, peHeader.length, peOffset)).bytesRead !== peHeader.length || peHeader.toString("binary", 0, 4) !== "PE\0\0") {
      throw new Error("release executable has an invalid PE signature");
    }
    if (peHeader.readUInt16LE(4) !== architectureMachines[architecture]) throw new Error("release executable architecture does not match the package");
  } finally {
    await handle.close();
  }
}

export async function inspectServiceGuestCatalogTrust(file, trustedKeys) {
  const trust = serviceGuestCatalogTrust(trustedKeys);
  const contents = await readFile(file);
  if (!contents.includes(Buffer.from(trust.encoded, "utf8")) || !contents.includes(Buffer.from(trust.binding, "utf8"))) {
    throw new Error("VM service was not built with the approved guest catalog trust binding");
  }
  return trust;
}

export async function inspectDaemonAcpCatalogTrust(file, trust) {
  return inspectDaemonAcpCatalogTrustBytes(await readFile(file), trust);
}

export function inspectDaemonAcpCatalogTrustBytes(contents, trust) {
  assertAcpCatalogTrust(trust);
  if (!Buffer.isBuffer(contents) || contents.length < 1 || contents.length > 128 * 1024 * 1024) {
    throw new Error("daemon trust inspection input is invalid");
  }
  if (!contents.includes(Buffer.from(trust.raw, "utf8")) ||
      !contents.includes(Buffer.from(trust.binding, "utf8"))) {
    throw new Error("daemon was not built with the approved ACP catalog trust binding");
  }
  return trust;
}

export async function verifyOfficialGrokCatalog(
  file, architecture, trust, nowUnixSeconds, operatingSystem = "windows",
) {
  return verifyOfficialGrokCatalogBytes(
    await readFile(file), architecture, trust, nowUnixSeconds, operatingSystem,
  );
}

export function verifyOfficialGrokCatalogBytes(
  envelopeBytes, architecture, trust, nowUnixSeconds, operatingSystem = "windows",
) {
  if (!RELEASE_ARCHITECTURES.has(architecture)) throw new Error("official Grok catalog target is unsupported");
  if (operatingSystem !== "windows" && operatingSystem !== "linux") {
    throw new Error("official Grok catalog operating system is unsupported");
  }
  assertAcpCatalogTrust(trust);
  const envelope = parseStrictBoundedJSON(
    envelopeBytes, maxAcpCatalogEnvelopeSize, "official Grok catalog envelope",
  );
  if (!hasExactKeys(envelope, ["schema", "keyId", "payload", "signature"]) ||
      envelope.schema !== acpCatalogEnvelopeSchema || !acpKeyIDPattern.test(envelope.keyId) ||
      typeof envelope.payload !== "string" || typeof envelope.signature !== "string") {
    throw new Error("official Grok catalog envelope is invalid");
  }
  const publicKey = trust.keys.get(envelope.keyId);
  if (!publicKey) throw new Error("official Grok catalog signing key is not trusted");
  const payloadBytes = decodeCanonicalBase64(
    envelope.payload, "official Grok catalog payload", maxAcpCatalogPayloadSize,
  );
  const signature = decodeCanonicalBase64(envelope.signature, "official Grok catalog signature", 64);
  if (signature.length !== 64 || !verifyCryptoSignature(
    null, officialGrokCatalogSignatureBytes(envelope.keyId, payloadBytes), publicKey, signature,
  )) {
    throw new Error("official Grok catalog signature is invalid");
  }

  const payload = parseStrictBoundedJSON(
    payloadBytes, maxAcpCatalogPayloadSize, "official Grok catalog payload",
  );
  const now = nowUnixSeconds ?? Math.floor(Date.now() / 1000);
  if (!Number.isSafeInteger(now) || now < 0) throw new Error("official Grok verification clock is invalid");
  if (!hasExactKeys(payload, ["schema", "sequence", "expiresAtUnixSeconds", "components"]) ||
      payload.schema !== acpCatalogPayloadSchema || !Number.isSafeInteger(payload.sequence) ||
      payload.sequence < 1 || !Number.isSafeInteger(payload.expiresAtUnixSeconds) ||
      payload.expiresAtUnixSeconds <= now || !Array.isArray(payload.components) ||
      payload.components.length < 1 || payload.components.length > 32) {
    throw new Error("official Grok catalog payload is invalid or expired");
  }
  const expectedArchitecture = architecture === "x64" ? "x86_64" : "aarch64";
  const platforms = new Set();
  let selected;
  for (const component of payload.components) {
    if (!hasExactKeys(component, [
      "name", "publisher", "version", "os", "architecture", "executable", "sha256", "size",
    ]) || component.name !== "grok-build" || component.publisher !== "xAI" ||
        !validImageVersion(component.version) ||
        (component.os !== "windows" && component.os !== "linux") ||
        (component.architecture !== "x86_64" && component.architecture !== "aarch64") ||
        !validAcpExecutablePath(component.executable, component.os) ||
        !sha256Pattern.test(component.sha256) || !Number.isSafeInteger(component.size) ||
        component.size < 1 || component.size > maxAcpComponentSize) {
      throw new Error("official Grok catalog component record is invalid");
    }
    const platform = `${component.os}/${component.architecture}`;
    if (platforms.has(platform)) throw new Error("official Grok catalog contains a duplicate platform");
    platforms.add(platform);
    if (component.os === operatingSystem && component.architecture === expectedArchitecture) selected = component;
  }
  if (!selected) throw new Error("official Grok catalog does not contain the package platform");
  const expectedExecutable = operatingSystem === "windows" ? acpComponentRelativePath : "bin/grok";
  if (selected.executable !== expectedExecutable) {
    throw new Error(`official Grok release component must use ${expectedExecutable}`);
  }
  return {
    sequence: payload.sequence,
    expiresAtUnixSeconds: payload.expiresAtUnixSeconds,
    signatureKeyId: envelope.keyId,
    component: {
      version: selected.version,
      executable: selected.executable,
      sha256: selected.sha256,
      size: selected.size,
    },
  };
}

export async function verifyPackagedNativeLayout(
  appDirectory, inputs, architecture, { firstPartyBinariesSigned = false } = {},
) {
  if (!inputs?.files || !inputs.acpComponent || inputs.acpComponent.stagePath !== acpComponentStagePath ||
      typeof firstPartyBinariesSigned !== "boolean") {
    throw new Error("verified native release inputs are required for layout validation");
  }
  const resourcesRoot = await realpath(path.join(appDirectory, "resources"));
  const expectedBinFiles = [
    "components/grok-acp/bin/grok.exe",
    "components/grok-acp/catalog.json",
    "grok-daemon.exe",
    "grok-host-tools-mcp.exe",
  ].toSorted();
  const binRoot = await realpath(path.join(resourcesRoot, "bin"));
  const actualBinFiles = await listStageFiles(binRoot);
  if (actualBinFiles.length !== expectedBinFiles.length ||
      actualBinFiles.some((candidate, index) => candidate !== expectedBinFiles[index])) {
    throw new Error("packaged native component layout is invalid");
  }
  const byteStablePaths = [acpCatalogStagePath, acpComponentStagePath];
  if (!firstPartyBinariesSigned) {
    byteStablePaths.push("bin/grok-daemon.exe");
    byteStablePaths.push("bin/grok-host-tools-mcp.exe");
  }
  for (const relativePath of byteStablePaths) {
    const expectedRecord = inputs.manifest.files.find((record) => record.path === relativePath);
    const packaged = await containedRegularFile(
      resourcesRoot, relativePath, expectedInputLimits.get(relativePath),
    );
    const metadata = await stat(packaged);
    if (!expectedRecord || metadata.size !== expectedRecord.size ||
        await sha256File(packaged) !== expectedRecord.sha256) {
      throw new Error("packaged native component bytes differ from verified release inputs");
    }
  }
  await inspectPortableExecutable(path.join(resourcesRoot, "bin", "grok-daemon.exe"), architecture);
  await inspectPortableExecutable(
    path.join(resourcesRoot, "bin", "grok-host-tools-mcp.exe"), architecture,
  );
  await inspectDaemonAcpCatalogTrust(
    path.join(resourcesRoot, "bin", "grok-daemon.exe"), inputs.acpCatalogTrust,
  );
  await inspectPortableExecutable(path.join(resourcesRoot, ...acpComponentStagePath.split("/")), architecture);
  return {
    daemon: path.join(resourcesRoot, "bin", "grok-daemon.exe"),
    hostToolsHelper: path.join(resourcesRoot, "bin", "grok-host-tools-mcp.exe"),
    catalog: path.join(resourcesRoot, ...acpCatalogStagePath.split("/")),
    component: path.join(resourcesRoot, ...acpComponentStagePath.split("/")),
  };
}

export async function verifyPackagedCoreWindowsLayout(
  appDirectory, inputs, architecture, { firstPartyBinariesSigned = false } = {},
) {
  if (!inputs?.files || !inputs.manifest || architecture !== "x64" ||
      typeof firstPartyBinariesSigned !== "boolean") {
    throw new Error("verified core Windows inputs are required for layout validation");
  }
  const resourcesRoot = await realpath(path.join(appDirectory, "resources"));
  const binRoot = await realpath(path.join(resourcesRoot, "bin"));
  const expectedPaths = [
    "components/grok-acp/bin/grok.exe",
    "components/grok-acp/pinned-component.json",
    "grok-daemon.exe",
    "grok-host-tools-mcp.exe",
  ].toSorted();
  const actualPaths = await listStageFiles(binRoot);
  if (actualPaths.length !== expectedPaths.length ||
      actualPaths.some((candidate, index) => candidate !== expectedPaths[index])) {
    throw new Error("packaged core Windows native layout is invalid");
  }
  const stablePaths = [acpPinnedManifestStagePath, acpComponentStagePath];
  if (!firstPartyBinariesSigned) {
    stablePaths.push("bin/grok-daemon.exe", "bin/grok-host-tools-mcp.exe");
  }
  for (const relativePath of stablePaths) {
    const packaged = await containedRegularFile(
      resourcesRoot, relativePath, coreWindowsInputLimits.get(relativePath),
    );
    const source = inputs.files.get(relativePath);
    if (!source || await sha256File(packaged) !== await sha256File(source)) {
      throw new Error("packaged core Windows bytes differ from verified inputs");
    }
  }
  const daemon = path.join(resourcesRoot, "bin", "grok-daemon.exe");
  const helper = path.join(resourcesRoot, "bin", "grok-host-tools-mcp.exe");
  const component = path.join(resourcesRoot, ...acpComponentStagePath.split("/"));
  await inspectPortableExecutable(daemon, architecture);
  await inspectPortableExecutable(helper, architecture);
  await inspectPortableExecutable(component, architecture);
  inspectDaemonAcpPinnedManifestBytes(await readFile(daemon), inputs.manifest);
  return {
    daemon,
    hostToolsHelper: helper,
    manifest: path.join(resourcesRoot, ...acpPinnedManifestStagePath.split("/")),
    component,
  };
}

export function shouldAuthenticodeSignPackagedFile(appDirectory, file) {
  const relative = path.relative(path.resolve(appDirectory), path.resolve(file)).split(path.sep).join("/");
  if (!relative || relative.startsWith("../") || path.isAbsolute(relative)) {
    throw new Error("packaged signing candidate is outside the application directory");
  }
  return /\.(?:dll|exe|msi|node)$/i.test(relative) &&
    relative.toLowerCase() !== `resources/${acpComponentStagePath}`;
}

export async function sha256File(file) {
  const handle = await open(file, "r");
  const hash = createHash("sha256");
  const buffer = Buffer.allocUnsafe(1024 * 1024);
  try {
    let position = 0;
    while (true) {
      const { bytesRead } = await handle.read(buffer, 0, buffer.length, position);
      if (bytesRead === 0) break;
      hash.update(buffer.subarray(0, bytesRead));
      position += bytesRead;
    }
    return hash.digest("hex");
  } finally {
    await handle.close();
  }
}

function assertAcpCatalogTrust(trust) {
  if (!trust || typeof trust !== "object" || typeof trust.raw !== "string" ||
      !(trust.keys instanceof Map) || trust.keys.size < 1 || trust.keys.size > 16 ||
      typeof trust.binding !== "string" ||
      trust.binding !== acpCatalogTrustBindingPrefix + createHash("sha256").update(trust.raw, "utf8").digest("hex")) {
    throw new Error("trusted ACP catalog keys are required");
  }
}

function validAcpExecutablePath(value, operatingSystem) {
  if (typeof value !== "string" || value.length < 1 || value.length > 260 ||
      value.startsWith("/") || value.includes("\\")) return false;
  const segments = value.split("/");
  const executableName = operatingSystem === "windows" ? "grok.exe" : "grok";
  return segments.length <= 16 && segments.at(-1) === executableName && segments.every((segment) =>
    segment.length > 0 && segment !== "." && segment !== ".." &&
    /^[A-Za-z0-9._-]+$/.test(segment) && catalogPathSegmentIsSafe(segment));
}

export function parseStrictBoundedJSON(bytes, maximum, label) {
  if (!Buffer.isBuffer(bytes) || bytes.length < 1 || bytes.length > maximum) {
    throw new Error(`${label} is not bounded JSON`);
  }
  let text;
  try { text = new TextDecoder("utf-8", { fatal: true }).decode(bytes); } catch {
    throw new Error(`${label} is not valid UTF-8 JSON`);
  }
  try { return new StrictJSONParser(text).parse(); } catch {
    throw new Error(`${label} is not strict JSON`);
  }
}

class StrictJSONParser {
  constructor(text) {
    this.text = text;
    this.index = 0;
  }

  parse() {
    this.skipWhitespace();
    const value = this.parseValue(0);
    this.skipWhitespace();
    if (this.index !== this.text.length) throw new Error("trailing JSON data");
    return value;
  }

  parseValue(depth) {
    if (depth > 64) throw new Error("JSON nesting is excessive");
    const character = this.text[this.index];
    if (character === "{") return this.parseObject(depth + 1);
    if (character === "[") return this.parseArray(depth + 1);
    if (character === '"') return this.parseString();
    if (character === "t") return this.parseLiteral("true", true);
    if (character === "f") return this.parseLiteral("false", false);
    if (character === "n") return this.parseLiteral("null", null);
    return this.parseNumber();
  }

  parseObject(depth) {
    this.index += 1;
    this.skipWhitespace();
    const output = Object.create(null);
    const keys = new Set();
    if (this.text[this.index] === "}") {
      this.index += 1;
      return output;
    }
    while (true) {
      if (this.text[this.index] !== '"') throw new Error("object key is not a string");
      const key = this.parseString();
      if (keys.has(key)) throw new Error("duplicate JSON object key");
      keys.add(key);
      this.skipWhitespace();
      if (this.text[this.index] !== ":") throw new Error("object separator is missing");
      this.index += 1;
      this.skipWhitespace();
      Object.defineProperty(output, key, {
        value: this.parseValue(depth), enumerable: true, configurable: true, writable: true,
      });
      this.skipWhitespace();
      if (this.text[this.index] === "}") {
        this.index += 1;
        return output;
      }
      if (this.text[this.index] !== ",") throw new Error("object delimiter is missing");
      this.index += 1;
      this.skipWhitespace();
    }
  }

  parseArray(depth) {
    this.index += 1;
    this.skipWhitespace();
    const output = [];
    if (this.text[this.index] === "]") {
      this.index += 1;
      return output;
    }
    while (true) {
      output.push(this.parseValue(depth));
      this.skipWhitespace();
      if (this.text[this.index] === "]") {
        this.index += 1;
        return output;
      }
      if (this.text[this.index] !== ",") throw new Error("array delimiter is missing");
      this.index += 1;
      this.skipWhitespace();
    }
  }

  parseString() {
    const start = this.index;
    this.index += 1;
    let escaped = false;
    while (this.index < this.text.length) {
      const character = this.text[this.index];
      this.index += 1;
      if (escaped) {
        escaped = false;
        continue;
      }
      if (character === "\\") {
        escaped = true;
        continue;
      }
      if (character === '"') return JSON.parse(this.text.slice(start, this.index));
      if (character.codePointAt(0) < 0x20) throw new Error("control character in JSON string");
    }
    throw new Error("unterminated JSON string");
  }

  parseLiteral(literal, value) {
    if (!this.text.startsWith(literal, this.index)) throw new Error("invalid JSON literal");
    this.index += literal.length;
    return value;
  }

  parseNumber() {
    const match = /^-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?/.exec(this.text.slice(this.index));
    if (!match) throw new Error("invalid JSON value");
    this.index += match[0].length;
    const value = Number(match[0]);
    if (!Number.isFinite(value)) throw new Error("JSON number is not finite");
    return value;
  }

  skipWhitespace() {
    while ([" ", "\t", "\r", "\n"].includes(this.text[this.index])) this.index += 1;
  }
}

function boundedEnvironment(environment, name, maximum) {
  const value = environment[name];
  if (typeof value !== "string" || value.length === 0 || value.length > maximum || value.includes("\0")) {
    throw new Error(`${name} is required and bounded`);
  }
  return value;
}

function parseSigningArguments(raw, expectedThumbprint) {
  let value;
  try { value = JSON.parse(raw); } catch { throw new Error("GROK_WINDOWS_SIGN_ARGS_JSON must be a JSON array"); }
  if (!Array.isArray(value) || value.length === 0 || value.length > 16 || value.some((item) => typeof item !== "string" || item.length === 0 || item.length > 512 || item.includes("\0"))) {
    throw new Error("GROK_WINDOWS_SIGN_ARGS_JSON is invalid");
  }
  const valueOptions = new Set(["/csp", "/kc", "/s", "/sha1"]);
  const flagOptions = new Set(["/sm"]);
  const seen = new Map();
  for (let index = 0; index < value.length; index += 1) {
    const option = value[index].toLowerCase();
    if (flagOptions.has(option)) {
      if (seen.has(option)) throw new Error("GROK_WINDOWS_SIGN_ARGS_JSON repeats a SignTool option");
      seen.set(option, true);
      continue;
    }
    if (!valueOptions.has(option)) {
      throw new Error("release signing must use only an allowlisted certificate store or hardware-backed provider");
    }
    const optionValue = value[index + 1];
    if (optionValue === undefined || optionValue.startsWith("/") || optionValue.startsWith("-")) {
      throw new Error("GROK_WINDOWS_SIGN_ARGS_JSON has a missing SignTool option value");
    }
    if (seen.has(option)) throw new Error("GROK_WINDOWS_SIGN_ARGS_JSON repeats a SignTool option");
    seen.set(option, optionValue);
    index += 1;
  }
  const selectedThumbprint = seen.get("/sha1");
  if (typeof selectedThumbprint !== "string" || normalizeThumbprint(selectedThumbprint) !== expectedThumbprint) {
    throw new Error("SignTool must select the expected release certificate by SHA-1 thumbprint");
  }
  if (seen.has("/csp") !== seen.has("/kc")) {
    throw new Error("SignTool hardware provider and key container options must be supplied together");
  }
  return value;
}

function rejectAmbientSigningSecrets(environment) {
  const present = Object.keys(environment).filter((name) => ambientSigningSecretVariableSet.has(name.toUpperCase()));
  if (present.length > 0) throw new Error("ambient certificate-file and password signing variables are forbidden");
}

function parseWindowsVersion(value) {
  const match = windowsVersionPattern.exec(value);
  if (!match) throw new Error("Windows version must contain four numeric components");
  const parts = match.slice(1).map(Number);
  if (parts.some((part) => !Number.isSafeInteger(part) || part > 4_294_967_295)) throw new Error("Windows version component is out of range");
  return parts;
}

function compareVersions(left, right) {
  for (let index = 0; index < 4; index += 1) if (left[index] !== right[index]) return left[index] - right[index];
  return 0;
}

function hasInvalidXmlCharacters(value) {
  for (const character of value) {
    const code = character.codePointAt(0);
    if ((code <= 8) || code === 11 || code === 12 || (code >= 14 && code <= 31)) return true;
  }
  return false;
}

function escapeXml(value) {
  return value.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll('"', "&quot;").replaceAll("'", "&apos;");
}

async function containedRegularFile(root, relativePath, maximumSize) {
  if (!relativePath || path.isAbsolute(relativePath) || path.posix.normalize(relativePath) !== relativePath || relativePath.startsWith("../") || relativePath.includes("\\")) {
    throw new Error("release input path is unsafe");
  }
  const candidate = await realpath(path.join(root, ...relativePath.split("/")));
  const relative = path.relative(root, candidate);
  if (relative.startsWith("..") || path.isAbsolute(relative)) throw new Error("release input escapes the staging root");
  const metadata = await stat(candidate);
  if (!metadata.isFile() || metadata.size < 1 || metadata.size > maximumSize) throw new Error("release input is not a bounded regular file");
  return candidate;
}

function parseInputManifest(raw, expected) {
  let value;
  try { value = JSON.parse(raw); } catch { throw new Error("release input manifest is invalid JSON"); }
  if (!value || typeof value !== "object" || Array.isArray(value) ||
      Object.keys(value).toSorted().join(",") !== "architecture,channel,desktopVersion,files,guest,product,sequence,signature,version" ||
      value.version !== 3 || value.product !== "grok-desktop" || value.architecture !== expected.architecture ||
      value.channel !== expected.channel || value.desktopVersion !== expected.desktopVersion ||
      !Number.isSafeInteger(value.sequence) || value.sequence < 1 || !Array.isArray(value.files)) {
    throw new Error("release input manifest has an unsupported schema");
  }
  if (!value.guest || typeof value.guest !== "object" || Array.isArray(value.guest) ||
      Object.keys(value.guest).toSorted().join(",") !== "imageId,imageVersion,path,sha256,size,stagingName" ||
      !guestImageIDPattern.test(value.guest.imageId) || !catalogPathSegmentIsSafe(value.guest.imageId) ||
      !validImageVersion(value.guest.imageVersion) || value.guest.path !== "guest/grok-guest.vhdx" ||
      !guestStagingNamePattern.test(value.guest.stagingName) || !catalogPathSegmentIsSafe(value.guest.stagingName) ||
      value.guest.stagingName !== path.posix.basename(value.guest.path) ||
      !sha256Pattern.test(value.guest.sha256) || !Number.isSafeInteger(value.guest.size) || value.guest.size < 1) {
    throw new Error("signed guest metadata is invalid");
  }
  if (!value.signature || typeof value.signature !== "object" || Array.isArray(value.signature) ||
      Object.keys(value.signature).toSorted().join(",") !== "algorithm,keyId,value" ||
      value.signature.algorithm !== "ed25519" || !keyIDPattern.test(value.signature.keyId) ||
      typeof value.signature.value !== "string") {
    throw new Error("release input signature metadata is invalid");
  }
  const paths = new Set();
  for (const record of value.files) {
    if (!record || typeof record !== "object" || Array.isArray(record) || Object.keys(record).toSorted().join(",") !== "path,sha256,size") {
      throw new Error("release input record is invalid");
    }
    if (typeof record.path !== "string" || paths.has(record.path) || typeof record.sha256 !== "string" || !sha256Pattern.test(record.sha256) || !Number.isSafeInteger(record.size) || record.size < 1) {
      throw new Error("release input record is invalid");
    }
    paths.add(record.path);
  }
  const orderedPaths = [...paths].toSorted();
  if (value.files.some((record, index) => record.path !== orderedPaths[index])) {
    throw new Error("release input records must use canonical path order");
  }
  return value;
}

function verifyReleaseInputSignature(manifest, trustedKeys) {
  const publicKey = trustedKeys.get(manifest.signature.keyId);
  if (!publicKey) throw new Error("release input signature key is not trusted");
  const signature = decodeCanonicalBase64(manifest.signature.value, "release input signature");
  if (signature.length !== 64 || !verifyCryptoSignature(null, releaseInputSigningBytes(manifest), publicKey, signature)) {
    throw new Error("release input signature is invalid");
  }
}

async function verifyGuestImageCatalog(file, architecture, trustedKeys) {
  const raw = await readFile(file, "utf8");
  let value;
  try { value = JSON.parse(raw); } catch { throw new Error("guest image catalog is invalid JSON"); }
  if (!hasExactKeys(value, ["schemaVersion", "product", "architecture", "sequence", "images", "signature"]) ||
      value.schemaVersion !== 1 || value.product !== "grok-desktop-guest" || value.architecture !== architecture ||
      !Number.isSafeInteger(value.sequence) || value.sequence < 1 || !Array.isArray(value.images) ||
      value.images.length < 1 || value.images.length > 16 ||
      !hasExactKeys(value.signature, ["algorithm", "keyId", "value"]) ||
      value.signature.algorithm !== "ed25519" || !keyIDPattern.test(value.signature.keyId) ||
      typeof value.signature.value !== "string") {
    throw new Error("guest image catalog has an unsupported schema");
  }
  if (raw !== `${JSON.stringify(value)}\n`) throw new Error("guest image catalog must use canonical JSON encoding");
  const ids = new Set();
  const stagingNames = new Set();
  let previousID = "";
  for (const image of value.images) {
    if (!hasExactKeys(image, ["id", "version", "stagingName", "sha256", "sizeBytes"]) ||
        !guestImageIDPattern.test(image.id) || !catalogPathSegmentIsSafe(image.id) || image.id <= previousID || ids.has(image.id) ||
        !validImageVersion(image.version) || !guestStagingNamePattern.test(image.stagingName) ||
        !catalogPathSegmentIsSafe(image.stagingName) || stagingNames.has(image.stagingName) ||
        !sha256Pattern.test(image.sha256) || !Number.isSafeInteger(image.sizeBytes) ||
        image.sizeBytes < 1 || image.sizeBytes > maxGuestImageSize) {
      throw new Error("guest image catalog inventory is invalid");
    }
    ids.add(image.id);
    stagingNames.add(image.stagingName);
    previousID = image.id;
  }
  const publicKey = trustedKeys.get(value.signature.keyId);
  if (!publicKey) throw new Error("guest image catalog signature key is not trusted");
  const signature = decodeCanonicalBase64(value.signature.value, "guest image catalog signature");
  if (signature.length !== 64 || !verifyCryptoSignature(null, guestImageCatalogSigningBytes(value), publicKey, signature)) {
    throw new Error("guest image catalog signature is invalid");
  }
  return value;
}

function crossCheckGuestMetadata(manifest, catalog) {
  if (catalog.sequence !== manifest.sequence || catalog.images.length !== 1) {
    throw new Error("guest image catalog sequence or inventory does not match the signed release");
  }
  const image = catalog.images[0];
  if (image.id !== manifest.guest.imageId || image.version !== manifest.guest.imageVersion ||
      image.stagingName !== manifest.guest.stagingName || image.sha256 !== manifest.guest.sha256 ||
      image.sizeBytes !== manifest.guest.size) {
    throw new Error("guest image catalog metadata does not match the signed release");
  }
}

async function verifyIntegrationCatalogJSON(file) {
  return parseIntegrationCatalog(await readFile(file));
}

export function parseIntegrationCatalog(bytes) {
  const value = parseStrictBoundedJSON(bytes, 1024 * 1024, "integration catalog");
  if (!hasExactKeys(value, ["version", "revision", "bundles"]) || value.version !== 1 ||
      !Number.isSafeInteger(value.revision) || value.revision < 1 ||
      !Array.isArray(value.bundles) || value.bundles.length > 64) {
    throw new Error("integration catalog header is invalid");
  }

  const identities = new Set();
  const bundleLocations = new Set();
  let previousID = "";
  for (const entry of value.bundles) {
    if (!hasExactKeys(entry, [
      "id", "version", "rootIndex", "bundlePath", "manifestPath", "manifestSha256",
      "allowedCapabilities", "files",
    ]) || typeof entry.id !== "string" || entry.id.length > 128 ||
        !integrationIDPattern.test(entry.id) || (previousID && entry.id <= previousID) ||
        typeof entry.version !== "string" || entry.version.length > 64 || !validImageVersion(entry.version) ||
        !Number.isSafeInteger(entry.rootIndex) || entry.rootIndex < 0 || entry.rootIndex > 63 ||
        !validIntegrationBundlePath(entry.bundlePath) || !validIntegrationBundlePath(entry.manifestPath) ||
        typeof entry.manifestSha256 !== "string" || !sha256Pattern.test(entry.manifestSha256) ||
        !Array.isArray(entry.allowedCapabilities) || entry.allowedCapabilities.length > 64 ||
        !Array.isArray(entry.files) || entry.files.length < 1 || entry.files.length > 1024) {
      throw new Error("integration catalog entry is invalid");
    }
    const location = `${entry.rootIndex}:${entry.bundlePath.toLowerCase()}`;
    if (identities.has(entry.id) || bundleLocations.has(location)) {
      throw new Error("integration catalog contains a duplicate identity or bundle location");
    }
    identities.add(entry.id);
    bundleLocations.add(location);
    previousID = entry.id;

    let previousCapability = "";
    for (const capability of entry.allowedCapabilities) {
      if (typeof capability !== "string" || capability.length > 96 ||
          !integrationCapabilityPattern.test(capability) ||
          (previousCapability && capability <= previousCapability)) {
        throw new Error("integration catalog capabilities are invalid or non-canonical");
      }
      previousCapability = capability;
    }

    const files = new Map();
    const caseFoldedPaths = new Set();
    let previousPath = "";
    let totalBytes = 0;
    for (const file of entry.files) {
      if (!hasExactKeys(file, ["path", "sha256", "size", "executable"]) ||
          !validIntegrationBundlePath(file.path) || file.path.split("/").length - 1 >= 16 ||
          typeof file.sha256 !== "string" || !sha256Pattern.test(file.sha256) ||
          !Number.isSafeInteger(file.size) || file.size < 1 || file.size > 64 * 1024 * 1024 ||
          typeof file.executable !== "boolean" || (previousPath && file.path <= previousPath)) {
        throw new Error("integration catalog file inventory is invalid or non-canonical");
      }
      const foldedPath = file.path.toLowerCase();
      if (files.has(file.path) || caseFoldedPaths.has(foldedPath)) {
        throw new Error("integration catalog contains a duplicate file path");
      }
      totalBytes += file.size;
      if (!Number.isSafeInteger(totalBytes) || totalBytes > 512 * 1024 * 1024) {
        throw new Error("integration catalog bundle exceeds the size limit");
      }
      files.set(file.path, file);
      caseFoldedPaths.add(foldedPath);
      previousPath = file.path;
    }
    const manifest = files.get(entry.manifestPath);
    if (!manifest || manifest.executable || manifest.sha256 !== entry.manifestSha256) {
      throw new Error("integration catalog manifest binding is invalid");
    }
  }
  return value;
}

function validIntegrationBundlePath(value) {
  if (typeof value !== "string" || value.length < 1 || value.length > 260 ||
      !integrationBundlePathPattern.test(value) || value.startsWith("/") ||
      /^[A-Za-z]:/.test(value) || path.posix.normalize(value) !== value) return false;
  return value.split("/").every((segment) => segment.length > 0 && segment !== "." && segment !== ".." &&
    !segment.endsWith(".") && !segment.endsWith(" ") && catalogPathSegmentIsSafe(segment));
}

function validImageVersion(value) {
  if (typeof value !== "string" || value.length < 1 || value.length > 128 || !imageVersionPattern.test(value)) return false;
  const [withoutBuild, build] = value.split("+", 2);
  if (build !== undefined && build.split(".").some((identifier) => identifier.length === 0)) return false;
  const separator = withoutBuild.indexOf("-");
  if (separator < 0) return true;
  return withoutBuild.slice(separator + 1).split(".").every((identifier) =>
    identifier.length > 0 && (!/^\d+$/.test(identifier) || identifier.length === 1 || identifier[0] !== "0"));
}

function catalogPathSegmentIsSafe(value) {
  return typeof value === "string" && !windowsReservedNames.has(value.split(".", 1)[0].toUpperCase());
}

function hasExactKeys(value, expected) {
  return value !== null && typeof value === "object" && !Array.isArray(value) &&
    Object.keys(value).toSorted().join(",") === expected.toSorted().join(",");
}

async function verifyVhdx(vhdx, sidecar) {
  const handle = await open(vhdx, "r");
  try {
    const signature = Buffer.alloc(8);
    if ((await handle.read(signature, 0, 8, 0)).bytesRead !== 8 || signature.toString("ascii") !== "vhdxfile") throw new Error("guest disk is not a VHDX image");
  } finally {
    await handle.close();
  }
  const sidecarText = await readFile(sidecar, "utf8");
  const match = /^([a-f0-9]{64})  grok-guest\.vhdx\r?\n$/.exec(sidecarText);
  if (!match || match[1] !== await sha256File(vhdx)) throw new Error("guest VHDX sidecar digest does not match");
}

function normalizeThumbprint(value) {
  if (typeof value !== "string" || !sha1Pattern.test(value)) throw new Error("release certificate thumbprint must contain 40 hexadecimal characters");
  return value.toUpperCase();
}

function decodeCanonicalBase64(value, name, maximumDecoded = 3072) {
  const maximumEncoded = Math.ceil(maximumDecoded / 3) * 4;
  if (typeof value !== "string" || value.length === 0 || value.length > maximumEncoded ||
      !/^[A-Za-z0-9+/]+={0,2}$/.test(value)) {
    throw new Error(`${name} is not canonical base64`);
  }
  const decoded = Buffer.from(value, "base64");
  if (decoded.length < 1 || decoded.length > maximumDecoded || decoded.toString("base64") !== value) {
    throw new Error(`${name} is not canonical base64`);
  }
  return decoded;
}

async function listStageFiles(root, current = root) {
  const files = [];
  for (const entry of await readdir(current, { withFileTypes: true })) {
    const candidate = path.join(current, entry.name);
    if (entry.isSymbolicLink()) throw new Error("release staging directory contains a symbolic link");
    if (entry.isDirectory()) files.push(...await listStageFiles(root, candidate));
    else if (entry.isFile()) files.push(path.relative(root, candidate).split(path.sep).join("/"));
    else throw new Error("release staging directory contains an unsupported file type");
  }
  return files.toSorted();
}
