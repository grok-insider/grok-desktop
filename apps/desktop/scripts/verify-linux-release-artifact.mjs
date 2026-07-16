import { spawn } from "node:child_process";
import { constants as fsConstants } from "node:fs";
import { createHash } from "node:crypto";
import { lstat, mkdtemp, open, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { isDeepStrictEqual } from "node:util";
import { pathToFileURL } from "node:url";
import { FuseV1Options, getCurrentFuseWire } from "@electron/fuses";
import {
  ELECTRON_FUSE_POLICY,
  readVerifiedFuseState,
} from "./electron-fuse-policy.mjs";
import { inspectPortableLinuxRuntimeHandle } from "./linux-native-runtime-policy.mjs";
import { verifyOfficialGrokPinnedManifestBytes } from "./release-utils.mjs";

const SHA256_PATTERN = /^[a-f0-9]{64}$/;
const VERSION_PATTERN = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?$/;
const MAX_APPIMAGE_SIZE = 8 * 1024 * 1024 * 1024;
const MAX_ELECTRON_SIZE = 1024 * 1024 * 1024;
const MAX_NATIVE_SIZE = 1024 * 1024 * 1024;
const MAX_RECORD_SIZE = 256 * 1024;
const MAX_PIN_SIZE = 8 * 1024;
const OPEN_FLAGS = fsConstants.O_RDONLY
  | (fsConstants.O_CLOEXEC ?? 0)
  | (fsConstants.O_NOFOLLOW ?? 0);

const defaultOperations = Object.freeze({
  extractAppImage: extractAppImageWithoutFuse,
  readFuseWire: getCurrentFuseWire,
});

function exactObject(value, keys, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
  const actual = Object.keys(value).toSorted();
  const expected = [...keys].toSorted();
  if (actual.length !== expected.length
      || actual.some((key, index) => key !== expected[index])) {
    throw new Error(`${label} contains unsupported or missing fields`);
  }
}

function boundedString(value, maximum, label) {
  if (typeof value !== "string" || value.length < 1 || value.length > maximum
      || [...value].some((character) => {
        const codePoint = character.codePointAt(0);
        return codePoint < 0x20 || codePoint === 0x7f;
      })) {
    throw new Error(`${label} is invalid`);
  }
  return value;
}

function sha256(value, label) {
  if (typeof value !== "string" || !SHA256_PATTERN.test(value)) {
    throw new Error(`${label} is invalid`);
  }
  return value;
}

function sameIdentity(left, right) {
  return left.dev === right.dev
    && left.ino === right.ino
    && left.size === right.size
    && left.mode === right.mode
    && left.mtimeNs === right.mtimeNs
    && left.ctimeNs === right.ctimeNs;
}

async function openRegularFile(filePath, label, maximumSize, executable = false) {
  let handle;
  try {
    handle = await open(filePath, OPEN_FLAGS);
  } catch {
    throw new Error(`${label} is not a bounded regular${executable ? " executable" : ""} file`);
  }
  try {
    const opened = await handle.stat({ bigint: true });
    if (!opened.isFile() || opened.size < 1n || opened.size > BigInt(maximumSize)
        || (executable && (opened.mode & 0o111n) === 0n)) {
      throw new Error(`${label} is not a bounded regular${executable ? " executable" : ""} file`);
    }
    return { handle, identity: opened };
  } catch (error) {
    await handle.close();
    throw error;
  }
}

async function readRegularFile(filePath, label, maximumSize) {
  const retained = await openRegularFile(filePath, label, maximumSize);
  try {
    const contents = await retained.handle.readFile();
    const after = await retained.handle.stat({ bigint: true });
    if (!sameIdentity(retained.identity, after)
        || BigInt(contents.byteLength) !== retained.identity.size) {
      throw new Error(`${label} changed while it was read`);
    }
    return contents;
  } finally {
    await retained.handle.close();
  }
}

async function hashRegularFile(
  filePath,
  label,
  maximumSize,
  executable = false,
  includeSha1 = false,
  portableArchitecture = undefined,
) {
  const retained = await openRegularFile(filePath, label, maximumSize, executable);
  try {
    if (portableArchitecture) {
      await inspectPortableLinuxRuntimeHandle(retained.handle, portableArchitecture, label);
    }
    const digest = createHash("sha256");
    const legacyDigest = includeSha1 ? createHash("sha1") : undefined;
    const stream = retained.handle.createReadStream({ autoClose: false, start: 0 });
    for await (const chunk of stream) {
      digest.update(chunk);
      legacyDigest?.update(chunk);
    }
    const after = await retained.handle.stat({ bigint: true });
    if (!sameIdentity(retained.identity, after)) {
      throw new Error(`${label} changed while it was hashed`);
    }
    return {
      size: Number(retained.identity.size),
      sha256: digest.digest("hex"),
      ...(legacyDigest ? { sha1: legacyDigest.digest("hex") } : {}),
    };
  } finally {
    await retained.handle.close();
  }
}

function parseZsyncHeader(contents) {
  let headerEnd = contents.indexOf(Buffer.from("\n\n"));
  let separatorLength = 2;
  if (headerEnd < 0) {
    headerEnd = contents.indexOf(Buffer.from("\r\n\r\n"));
    separatorLength = 4;
  }
  if (headerEnd < 1 || headerEnd > 16_384 || headerEnd + separatorLength >= contents.length) {
    throw new Error("AppImage zsync header is missing or unbounded");
  }
  const headerBytes = contents.subarray(0, headerEnd);
  if ([...headerBytes].some((byte) => byte > 0x7f || byte === 0)) {
    throw new Error("AppImage zsync header is not bounded ASCII");
  }
  const fields = new Map();
  for (const line of headerBytes.toString("ascii").split(/\r?\n/)) {
    const match = /^([A-Za-z0-9-]{1,64}): ([\x20-\x7e]{1,4096})$/.exec(line);
    if (!match || fields.has(match[1])) {
      throw new Error("AppImage zsync header is malformed or ambiguous");
    }
    fields.set(match[1], match[2]);
  }
  for (const required of ["Filename", "URL", "Length", "SHA-1"]) {
    if (!fields.has(required)) throw new Error(`AppImage zsync header is missing ${required}`);
  }
  return fields;
}

function validatePackageRecord(value, appImagePath) {
  exactObject(value, [
    "schemaVersion", "product", "platform", "version", "architecture",
    "appDirectory", "executable", "appImage", "fuses", "appImageSha256", "zsync",
    "updateToolSha256", "daemonSha256", "hostToolsHelperSha256", "daemonSource",
    "acp", "vmService", "isolation", "notes", "builtAtUnixMs", "host",
  ], "Linux package record");
  if (value.schemaVersion !== 2 || value.product !== "grok-desktop" || value.platform !== "linux") {
    throw new Error("Linux package record identity is unsupported");
  }
  if (typeof value.version !== "string" || !VERSION_PATTERN.test(value.version)) {
    throw new Error("Linux package record version is invalid");
  }
  if (value.architecture !== "x64" && value.architecture !== "arm64") {
    throw new Error("Linux package record architecture is invalid");
  }
  for (const [name, candidate] of Object.entries({
    appDirectory: value.appDirectory,
    executable: value.executable,
    appImage: value.appImage,
    daemonSource: value.daemonSource,
  })) {
    boundedString(candidate, 4096, `Linux package record ${name}`);
    if (!path.isAbsolute(candidate)) throw new Error(`Linux package record ${name} is not absolute`);
  }
  if (value.executable !== path.join(value.appDirectory, "grok-desktop")) {
    throw new Error("Linux package record executable does not match its application directory");
  }
  const appImageName = path.basename(appImagePath);
  const expectedName = new RegExp(`^GrokDesktop-(?:stable|beta)-${value.architecture}\\.AppImage$`);
  if (!expectedName.test(appImageName) || path.basename(value.appImage) !== appImageName) {
    throw new Error("Linux package record AppImage name does not match the artifact");
  }
  sha256(value.appImageSha256, "Linux package record AppImage digest");
  exactObject(value.zsync, ["filename", "size", "sha256"], "Linux package zsync record");
  if (value.zsync.filename !== `${appImageName}.zsync`
      || !Number.isSafeInteger(value.zsync.size) || value.zsync.size < 1
      || value.zsync.size > 128 * 1024 * 1024) {
    throw new Error("Linux package zsync record is invalid");
  }
  sha256(value.zsync.sha256, "Linux package zsync digest");
  sha256(value.updateToolSha256, "Linux package record update tool digest");
  sha256(value.daemonSha256, "Linux package record daemon digest");
  sha256(value.hostToolsHelperSha256, "Linux package record Host Tools helper digest");

  const expectedFuses = expectedFuseMetadata();
  if (!isDeepStrictEqual(value.fuses, expectedFuses)) {
    throw new Error("Linux package record fuse metadata does not match the hardened policy");
  }

  if (!Number.isSafeInteger(value.builtAtUnixMs) || value.builtAtUnixMs < 1) {
    throw new Error("Linux package record build time is invalid");
  }
  if (value.isolation !== "not_embedded") {
    throw new Error("Linux package record isolation declaration is invalid");
  }
  boundedString(value.notes, 4096, "Linux package record notes");
  exactObject(value.host, ["platform", "arch", "release"], "Linux package record host");
  if (value.host.platform !== "linux" || value.host.arch !== value.architecture) {
    throw new Error("Linux package record host does not match its target");
  }
  boundedString(value.host.release, 256, "Linux package record host release");
  validateAcpRecord(value.acp);
  validateVmServiceRecord(value.vmService);
  return { ...value, expectedFuses };
}

function validateAcpRecord(value) {
  if (value?.staged === false) {
    exactObject(value, ["staged"], "Linux package ACP record");
    return;
  }
  const keys = value?.sourceUrl
    ? ["staged", "version", "sha256", "trustBinding", "sourceUrl"]
    : ["staged", "version", "sha256", "trustBinding"];
  exactObject(value, keys, "Linux package ACP record");
  if (value.staged !== true || !VERSION_PATTERN.test(value.version ?? "")) {
    throw new Error("Linux package ACP record identity is invalid");
  }
  sha256(value.sha256, "Linux package ACP digest");
  const binding = boundedString(value.trustBinding, 256, "Linux package ACP trust binding");
  if (!/^grok-acp-(?:catalog-trust|pinned-manifest)-v1:[a-f0-9]{64}$/.test(binding)) {
    throw new Error("Linux package ACP trust binding is invalid");
  }
  if (value.sourceUrl !== undefined) boundedString(value.sourceUrl, 2048, "Linux package ACP source URL");
}

function validateVmServiceRecord(value) {
  if (value?.staged === false) {
    exactObject(value, ["staged"], "Linux package VM service record");
    return;
  }
  exactObject(value, ["staged", "sha256", "serviceGroup", "daemonUid"], "Linux package VM service record");
  if (value.staged !== true || !Number.isInteger(value.daemonUid)
      || value.daemonUid < 0 || value.daemonUid > 0xffff_ffff
      || !/^[a-z_][a-z0-9_-]{0,30}$/.test(value.serviceGroup ?? "")) {
    throw new Error("Linux package VM service record is invalid");
  }
  sha256(value.sha256, "Linux package VM service digest");
}

function expectedFuseMetadata() {
  const policy = new Map(ELECTRON_FUSE_POLICY.map((entry) => [entry.option, entry.enabled]));
  const namedOptions = Object.entries(FuseV1Options)
    .filter(([name]) => Number.isNaN(Number(name)));
  if (policy.size !== ELECTRON_FUSE_POLICY.length || policy.size !== namedOptions.length
      || namedOptions.some(([, option]) => !policy.has(option))) {
    throw new Error("Electron fuse policy is incomplete or ambiguous");
  }
  return Object.fromEntries(namedOptions.map(([name, option]) => [name, policy.get(option)]));
}

async function assertDirectory(directory, label) {
  const metadata = await lstat(directory).catch(() => undefined);
  if (!metadata?.isDirectory() || metadata.isSymbolicLink()) {
    throw new Error(`${label} is unavailable`);
  }
}

async function pathExists(candidate) {
  return lstat(candidate).then(() => true, (error) => {
    if (error?.code === "ENOENT") return false;
    throw error;
  });
}

async function verifyEmbeddedAcp(binRoot, record, externalPinPath) {
  const componentRoot = path.join(binRoot, "resources", "bin", "components", "grok-acp");
  if (!record.acp.staged) {
    if (await pathExists(componentRoot)) {
      throw new Error("AppImage contains an ACP component that is absent from package metadata");
    }
    if (externalPinPath) throw new Error("an ACP pin was supplied for a package without staged ACP");
    return { staged: false };
  }

  await assertDirectory(componentRoot, "embedded ACP component root");
  const componentPath = path.join(componentRoot, "bin", "grok");
  const component = await hashRegularFile(
    componentPath, "embedded ACP component", MAX_NATIVE_SIZE, true,
  );
  if (component.sha256 !== record.acp.sha256) {
    throw new Error("embedded ACP component differs from Linux package metadata");
  }
  const pinPath = path.join(componentRoot, "pinned-component.json");
  const catalogPath = path.join(componentRoot, "catalog.json");
  const [hasPin, hasCatalog] = await Promise.all([pathExists(pinPath), pathExists(catalogPath)]);
  if (hasPin === hasCatalog) throw new Error("embedded ACP metadata is missing or ambiguous");

  if (hasPin) {
    const pinBytes = await readRegularFile(pinPath, "embedded ACP pin", MAX_PIN_SIZE);
    if (externalPinPath) {
      const externalBytes = await readRegularFile(externalPinPath, "tracked ACP pin", MAX_PIN_SIZE);
      if (!pinBytes.equals(externalBytes)) throw new Error("embedded ACP pin differs from the tracked release pin");
    }
    const pin = verifyOfficialGrokPinnedManifestBytes(pinBytes, record.architecture, "linux");
    if (pin.sha256 !== component.sha256 || pin.size !== component.size
        || pin.version !== record.acp.version || pin.binding !== record.acp.trustBinding
        || pin.sourceUrl !== record.acp.sourceUrl) {
      throw new Error("embedded ACP pin, component, and package metadata disagree");
    }
    return { staged: true, kind: "pinned", sha256: component.sha256, version: pin.version };
  }

  if (externalPinPath) throw new Error("tracked ACP pin was supplied for a catalog-backed package");
  const catalogBytes = await readRegularFile(catalogPath, "embedded ACP catalog", 512 * 1024);
  try { JSON.parse(catalogBytes.toString("utf8")); } catch {
    throw new Error("embedded ACP catalog is invalid JSON");
  }
  return { staged: true, kind: "catalog", sha256: component.sha256, version: record.acp.version };
}

export async function verifyLinuxReleaseArtifact(options, operations = defaultOperations) {
  const appImagePath = path.resolve(options?.appImagePath ?? "");
  const recordPath = path.resolve(options?.recordPath ?? "");
  const externalPinPath = options?.acpPinPath ? path.resolve(options.acpPinPath) : undefined;
  const runtime = { ...defaultOperations, ...operations };
  if (typeof runtime.extractAppImage !== "function" || typeof runtime.readFuseWire !== "function") {
    throw new Error("Linux artifact verifier operations are invalid");
  }

  const artifactBefore = await hashRegularFile(
    appImagePath, "release AppImage", MAX_APPIMAGE_SIZE, true, true,
  );
  const recordBytes = await readRegularFile(recordPath, "Linux package record", MAX_RECORD_SIZE);
  let recordValue;
  try { recordValue = JSON.parse(recordBytes.toString("utf8")); } catch {
    throw new Error("Linux package record is invalid JSON");
  }
  const record = validatePackageRecord(recordValue, appImagePath);
  if (artifactBefore.sha256 !== record.appImageSha256) {
    throw new Error("release AppImage differs from Linux package metadata");
  }
  const zsyncPath = path.join(path.dirname(appImagePath), record.zsync.filename);
  const zsyncBytes = await readRegularFile(
    zsyncPath,
    "AppImage zsync metadata",
    128 * 1024 * 1024,
  );
  const zsyncBefore = {
    size: zsyncBytes.byteLength,
    sha256: createHash("sha256").update(zsyncBytes).digest("hex"),
  };
  if (zsyncBefore.size !== record.zsync.size || zsyncBefore.sha256 !== record.zsync.sha256) {
    throw new Error("AppImage zsync bytes differ from Linux package metadata");
  }
  const zsyncHeader = parseZsyncHeader(zsyncBytes);
  const appImageName = path.basename(appImagePath);
  if (zsyncHeader.get("Filename") !== appImageName
      || zsyncHeader.get("URL") !== appImageName
      || zsyncHeader.get("Length") !== String(artifactBefore.size)
      || zsyncHeader.get("SHA-1")?.toLowerCase() !== artifactBefore.sha1) {
    throw new Error("AppImage zsync header does not describe the exact AppImage");
  }

  const temporaryRoot = await mkdtemp(path.join(os.tmpdir(), "grok-linux-release-verify-"));
  let embedded;
  try {
    const extractionRoot = await runtime.extractAppImage({
      appImagePath,
      extractionDirectory: temporaryRoot,
    });
    const expectedExtractionRoot = path.join(temporaryRoot, "squashfs-root");
    if (path.resolve(extractionRoot) !== expectedExtractionRoot) {
      throw new Error("AppImage extractor returned an unexpected root");
    }
    await assertDirectory(extractionRoot, "extracted AppImage root");
    const binRoot = path.join(extractionRoot, "usr", "bin");
    await assertDirectory(binRoot, "extracted AppImage application root");

    const electronPath = path.join(binRoot, "grok-desktop");
    await hashRegularFile(electronPath, "embedded Electron executable", MAX_ELECTRON_SIZE, true);
    const fuseMetadata = await readVerifiedFuseState(electronPath, runtime.readFuseWire);
    if (!isDeepStrictEqual(fuseMetadata, record.expectedFuses)) {
      throw new Error("embedded Electron fuse wire differs from Linux package metadata");
    }

    const daemon = await hashRegularFile(
      path.join(binRoot, "resources", "bin", "grok-daemon"),
      "embedded daemon", MAX_NATIVE_SIZE, true, false, record.architecture,
    );
    if (daemon.sha256 !== record.daemonSha256) {
      throw new Error("embedded daemon differs from Linux package metadata");
    }
    const hostToolsHelper = await hashRegularFile(
      path.join(binRoot, "resources", "bin", "grok-host-tools-mcp"),
      "embedded Host Tools helper", MAX_NATIVE_SIZE, true, false, record.architecture,
    );
    if (hostToolsHelper.sha256 !== record.hostToolsHelperSha256) {
      throw new Error("embedded Host Tools helper differs from Linux package metadata");
    }
    const updateTool = await hashRegularFile(
      path.join(binRoot, "resources", "bin", "appimageupdatetool.AppImage"),
      "embedded AppImage update tool", MAX_NATIVE_SIZE, true,
    );
    if (updateTool.sha256 !== record.updateToolSha256) {
      throw new Error("embedded update tool differs from Linux package metadata");
    }
    const acp = await verifyEmbeddedAcp(binRoot, record, externalPinPath);
    embedded = {
      fuses: fuseMetadata,
      daemonSha256: daemon.sha256,
      hostToolsHelperSha256: hostToolsHelper.sha256,
      updateToolSha256: updateTool.sha256,
      acp,
    };
  } finally {
    await rm(temporaryRoot, { recursive: true, force: true });
  }

  const artifactAfter = await hashRegularFile(
    appImagePath, "release AppImage", MAX_APPIMAGE_SIZE, true, true,
  );
  if (!isDeepStrictEqual(artifactAfter, artifactBefore)) {
    throw new Error("release AppImage changed during verification");
  }
  const zsyncAfter = await hashRegularFile(
    zsyncPath, "AppImage zsync metadata", 128 * 1024 * 1024,
  );
  if (!isDeepStrictEqual(zsyncAfter, zsyncBefore)) {
    throw new Error("AppImage zsync metadata changed during verification");
  }
  return {
    appImage: path.basename(appImagePath),
    size: artifactBefore.size,
    sha256: artifactBefore.sha256,
    zsync: { filename: record.zsync.filename, ...zsyncBefore },
    ...embedded,
  };
}

export async function extractAppImageWithoutFuse({ appImagePath, extractionDirectory }) {
  await assertDirectory(extractionDirectory, "AppImage extraction directory");
  const result = await spawnBounded(appImagePath, ["--appimage-extract"], {
    cwd: extractionDirectory,
    env: {
      PATH: process.env.PATH ?? "",
      HOME: extractionDirectory,
      TMPDIR: extractionDirectory,
      APPIMAGE_EXTRACT_AND_RUN: "1",
    },
  });
  if (result.code !== 0 || result.signal !== null) {
    throw new Error(`AppImage extraction failed: ${result.stderr.slice(0, 512)}`);
  }
  return path.join(extractionDirectory, "squashfs-root");
}

function spawnBounded(command, argumentList, options) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, argumentList, {
      ...options,
      shell: false,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk) => {
      if (stdout.length < 4096) stdout += chunk.slice(0, 4096 - stdout.length);
    });
    child.stderr.on("data", (chunk) => {
      if (stderr.length < 4096) stderr += chunk.slice(0, 4096 - stderr.length);
    });
    const timeout = setTimeout(() => child.kill("SIGKILL"), 120_000);
    child.once("error", (error) => {
      clearTimeout(timeout);
      reject(new Error(`AppImage extractor could not start: ${error.message}`));
    });
    child.once("exit", (code, signal) => {
      clearTimeout(timeout);
      resolve({ code, signal, stdout, stderr });
    });
  });
}

export function parseLinuxArtifactVerifierArguments(argv) {
  const values = {};
  const argumentList = argv.filter((value) => value !== "--");
  for (let index = 0; index < argumentList.length; index += 2) {
    const option = argumentList[index];
    const value = argumentList[index + 1];
    if (!["--appimage", "--record", "--acp-pin"].includes(option) || value === undefined) {
      throw new Error("Linux artifact verifier arguments are invalid");
    }
    if (values[option]) throw new Error(`Linux artifact verifier option ${option} was repeated`);
    values[option] = value;
  }
  if (!values["--appimage"] || !values["--record"]) {
    throw new Error("--appimage and --record are required");
  }
  return {
    appImagePath: path.resolve(values["--appimage"]),
    recordPath: path.resolve(values["--record"]),
    acpPinPath: values["--acp-pin"] ? path.resolve(values["--acp-pin"]) : undefined,
  };
}

const isMain = process.argv[1]
  && pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url;
if (isMain) {
  if (process.platform !== "linux") throw new Error("Linux artifacts must be verified on Linux");
  const result = await verifyLinuxReleaseArtifact(
    parseLinuxArtifactVerifierArguments(process.argv.slice(2)),
  );
  process.stdout.write(`${JSON.stringify({ ok: true, ...result })}\n`);
}
