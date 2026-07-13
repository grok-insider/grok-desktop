/**
 * Linux desktop packaging for Grok Desktop (Limited Mode / full GA train T1).
 *
 * Embeds the release or debug grok-daemon next to Electron resources and writes
 * a desktop entry + layout manifest. Does not claim Work isolation; the Linux
 * VM broker is packaged separately when present.
 */
import { cp, lstat, mkdir, mkdtemp, open, readFile, rm, stat, symlink, writeFile, chmod } from "node:fs/promises";
import { constants as fsConstants, readFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createHash, createPublicKey } from "node:crypto";
import { spawn } from "node:child_process";
import { packager } from "@electron/packager";
import {
  inspectDaemonAcpCatalogTrustBytes,
  parseAcpCatalogTrustedKeys,
  verifyOfficialGrokCatalogBytes,
} from "./release-utils.mjs";

const desktopRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const repositoryRoot = path.resolve(desktopRoot, "../..");
const productName = "Grok Desktop";
const executableName = "grok-desktop";
const OPEN_CLOEXEC = fsConstants.O_CLOEXEC ?? 0;
const linuxServiceTemplate = readFileSync(
  path.join(repositoryRoot, "native/linux-vm-service/packaging/grok-linux-vm-service.service.in"),
  "utf8",
);
const linuxServiceEnvironmentTemplate = readFileSync(
  path.join(repositoryRoot, "native/linux-vm-service/packaging/linux-vm-service.env.in"),
  "utf8",
);

export const LINUX_PACKAGE_ARCHITECTURES = new Set(["x64", "arm64"]);

export function parseLinuxPackageArguments(argv) {
  const values = {};
  const args = argv.filter((entry) => entry !== "--");
  for (let index = 0; index < args.length; index += 2) {
    const option = args[index];
    const value = args[index + 1];
    if (!option?.startsWith("--") || value === undefined) {
      throw new Error("linux package arguments must be option/value pairs");
    }
    if (!["--arch", "--out", "--daemon", "--acp-catalog", "--acp-component", "--appimagetool", "--appimagetool-sha256",
      "--appimageupdatetool", "--appimageupdatetool-sha256",
      "--update-trust-file", "--host-tools-helper",
      "--acp-trust-file", "--vm-service", "--daemon-uid", "--service-group"].includes(option)) {
      throw new Error(`unsupported linux package option ${option}`);
    }
    if (values[option]) throw new Error(`linux package option ${option} was repeated`);
    values[option] = value;
  }
  const architecture = values["--arch"] ?? (process.arch === "arm64" ? "arm64" : "x64");
  if (!LINUX_PACKAGE_ARCHITECTURES.has(architecture)) {
    throw new Error("--arch must be x64 or arm64");
  }
  const acpValues = [values["--acp-catalog"], values["--acp-component"], values["--acp-trust-file"]];
  if (acpValues.some(Boolean) && !acpValues.every(Boolean)) {
    throw new Error("signed ACP staging requires catalog, component, and trust file together");
  }
  const vmService = values["--vm-service"] ? path.resolve(values["--vm-service"]) : undefined;
  const appimagetool = values["--appimagetool"] ? path.resolve(values["--appimagetool"]) : undefined;
  const appimagetoolSha256 = values["--appimagetool-sha256"];
  if (appimagetool && !/^[a-f0-9]{64}$/.test(appimagetoolSha256 ?? "")) {
    throw new Error("--appimagetool-sha256 is required and must be lowercase SHA-256");
  }
  const appimageupdatetool = values["--appimageupdatetool"]
    ? path.resolve(values["--appimageupdatetool"])
    : undefined;
  const appimageupdatetoolSha256 = values["--appimageupdatetool-sha256"];
  if (appimageupdatetool && !/^[a-f0-9]{64}$/.test(appimageupdatetoolSha256 ?? "")) {
    throw new Error("--appimageupdatetool-sha256 is required and must be lowercase SHA-256");
  }
  const updateTrustFile = values["--update-trust-file"]
    ? path.resolve(values["--update-trust-file"])
    : undefined;
  if (!vmService && values["--daemon-uid"] !== undefined) {
    throw new Error("--daemon-uid is valid only with --vm-service");
  }
  if (vmService && (!/^\d{1,10}$/.test(values["--daemon-uid"] ?? "") ||
      Number(values["--daemon-uid"]) > 0xffff_ffff)) {
    throw new Error("--daemon-uid must be an explicit uint32 when staging linux-vm-service");
  }
  const serviceGroup = values["--service-group"] ?? "grok-desktop-broker";
  if (!/^[a-z_][a-z0-9_-]{0,30}$/.test(serviceGroup)) {
    throw new Error("--service-group is invalid");
  }
  return {
    architecture,
    out: values["--out"]
      ? path.resolve(values["--out"])
      : path.join(repositoryRoot, "out", "release", "linux", architecture),
    daemonBinary: values["--daemon"] ? path.resolve(values["--daemon"]) : undefined,
    hostToolsHelper: values["--host-tools-helper"]
      ? path.resolve(values["--host-tools-helper"])
      : undefined,
    acpCatalog: values["--acp-catalog"] ? path.resolve(values["--acp-catalog"]) : undefined,
    acpComponent: values["--acp-component"] ? path.resolve(values["--acp-component"]) : undefined,
    acpTrustFile: values["--acp-trust-file"] ? path.resolve(values["--acp-trust-file"]) : undefined,
    vmService,
    appimagetool,
    appimagetoolSha256,
    appimageupdatetool,
    appimageupdatetoolSha256,
    updateTrustFile,
    daemonUid: vmService ? Number(values["--daemon-uid"]) : undefined,
    serviceGroup,
  };
}

export function linuxAppImageUpdateInformation(architecture) {
  if (!LINUX_PACKAGE_ARCHITECTURES.has(architecture)) throw new Error("unsupported AppImage architecture");
  return `gh-releases-zsync|grok-insider|grok-desktop|latest|GrokDesktop-stable-${architecture}.AppImage.zsync`;
}

async function createLinuxAppImage(appDirectory, out, options, version) {
  if (!options.appimagetool) throw new Error("--appimagetool is required to produce the public AppImage");
  await assertExecutableFile(options.appimagetool, "appimagetool");
  if (await sha256File(options.appimagetool) !== options.appimagetoolSha256) {
    throw new Error("appimagetool does not match the pinned release digest");
  }
  const appDir = path.join(out, "AppDir");
  const bin = path.join(appDir, "usr", "bin");
  await mkdir(bin, { recursive: true, mode: 0o755 });
  await cp(appDirectory, bin, { recursive: true, dereference: false, errorOnExist: true });
  const applicationDirectory = path.join(appDir, "usr", "share", "applications");
  const iconDirectory = path.join(appDir, "usr", "share", "icons", "hicolor", "32x32", "apps");
  const metadataDirectory = path.join(appDir, "usr", "share", "metainfo");
  await mkdir(applicationDirectory, { recursive: true, mode: 0o755 });
  await mkdir(iconDirectory, { recursive: true, mode: 0o755 });
  await mkdir(metadataDirectory, { recursive: true, mode: 0o755 });
  await writeFile(
    path.join(applicationDirectory, "grok-desktop.desktop"),
    renderLinuxDesktopEntry({ name: productName, execPath: executableName, iconPath: executableName, version }),
    { encoding: "utf8", mode: 0o644, flag: "wx" },
  );
  await cp(
    path.join(appDirectory, "resources", "tray", "tray-dark-32.png"),
    path.join(iconDirectory, "grok-desktop.png"),
    { errorOnExist: true },
  );
  await writeFile(
    path.join(metadataDirectory, "grok-desktop.appdata.xml"),
    `<?xml version="1.0" encoding="UTF-8"?>\n<component type="desktop-application">\n  <id>io.grokinsider.GrokDesktop</id>\n  <name>Grok Desktop</name>\n  <summary>Official Grok and xAI desktop workspace</summary>\n  <metadata_license>CC0-1.0</metadata_license>\n  <project_license>AGPL-3.0-or-later</project_license>\n  <launchable type="desktop-id">grok-desktop.desktop</launchable>\n  <releases><release version="${version}" /></releases>\n</component>\n`,
    { encoding: "utf8", mode: 0o644, flag: "wx" },
  );
  await writeFile(
    path.join(appDir, "AppRun"),
    '#!/bin/sh\nset -eu\nHERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)\nexec "$HERE/usr/bin/grok-desktop" "$@"\n',
    { encoding: "utf8", mode: 0o755, flag: "wx" },
  );
  await symlink("usr/share/applications/grok-desktop.desktop", path.join(appDir, "grok-desktop.desktop"));
  await symlink("usr/share/icons/hicolor/32x32/apps/grok-desktop.png", path.join(appDir, "grok-desktop.png"));
  const appImage = path.join(out, `GrokDesktop-stable-${options.architecture}.AppImage`);
  await new Promise((resolve, reject) => {
    const child = spawn(options.appimagetool, ["--updateinformation", linuxAppImageUpdateInformation(options.architecture), appDir, appImage], {
      cwd: out,
      env: {
        PATH: process.env.PATH ?? "",
        ARCH: options.architecture === "x64" ? "x86_64" : "aarch64",
        APPIMAGE_EXTRACT_AND_RUN: "1",
      },
      stdio: "inherit",
    });
    child.once("error", reject);
    child.once("exit", (code, signal) => {
      if (code === 0 && signal === null) resolve();
      else reject(new Error("appimagetool failed to produce the AppImage"));
    });
  });
  await assertExecutableFile(appImage, "AppImage");
  return appImage;
}

async function stageAppImageUpdateTool(resourcesBin, options) {
  if (!options.appimageupdatetool) {
    throw new Error("--appimageupdatetool is required to produce the public AppImage");
  }
  await assertExecutableFile(options.appimageupdatetool, "appimageupdatetool");
  if (await sha256File(options.appimageupdatetool) !== options.appimageupdatetoolSha256) {
    throw new Error("appimageupdatetool does not match the pinned release digest");
  }
  const destination = path.join(resourcesBin, "appimageupdatetool.AppImage");
  await cp(options.appimageupdatetool, destination, { errorOnExist: true });
  await chmod(destination, 0o755);
  return destination;
}

async function stageUpdateTrust(stageResources, options) {
  if (!options.updateTrustFile) throw new Error("--update-trust-file is required to produce the public AppImage");
  const raw = await readFile(options.updateTrustFile);
  if (raw.byteLength < 1 || raw.byteLength > 65_536) throw new Error("update trust file is invalid");
  let value;
  try { value = JSON.parse(raw.toString("utf8")); } catch { throw new Error("update trust file is invalid"); }
  const entries = value && typeof value === "object" && !Array.isArray(value) ? Object.entries(value) : [];
  if (entries.length < 1 || entries.length > 8) throw new Error("update trust file is invalid");
  for (const [keyId, encoded] of entries) {
    if (!/^[a-z0-9][a-z0-9._-]{0,63}$/.test(keyId) || typeof encoded !== "string") {
      throw new Error("update trust file is invalid");
    }
    const der = Buffer.from(encoded, "base64");
    const key = createPublicKey({ key: der, format: "der", type: "spki" });
    if (key.asymmetricKeyType !== "ed25519"
        || !Buffer.from(key.export({ format: "der", type: "spki" })).equals(der)) {
      throw new Error("update trust file is invalid");
    }
  }
  const destination = path.join(stageResources, "update-trusted-keys.json");
  await cp(options.updateTrustFile, destination, { errorOnExist: true });
  return destination;
}

/** Resolves the daemon binary candidates used by development and package embeds. */
export function linuxDaemonCandidates(repositoryRootPath, architecture) {
  const hostMatches =
    (architecture === "x64" && process.arch === "x64") ||
    (architecture === "arm64" && process.arch === "arm64");
  if (!hostMatches) return [];
  return [
    path.join(repositoryRootPath, "target", "release", "grok-daemon"),
    path.join(repositoryRootPath, "target", "debug", "grok-daemon"),
  ];
}

export async function resolveLinuxDaemonBinary(options) {
  if (options.daemonBinary) {
    await assertExecutableFile(options.daemonBinary, "daemon override");
    return options.daemonBinary;
  }
  for (const candidate of linuxDaemonCandidates(repositoryRoot, options.architecture)) {
    try {
      await assertExecutableFile(candidate, "daemon candidate");
      return candidate;
    } catch {
      // try next
    }
  }
  throw new Error(
    "grok-daemon binary not found; build with `cargo build -p grok-daemon --release` or pass --daemon",
  );
}

/** Resolves the policy-free Host Tools MCP helper for a Linux package. */
export async function resolveLinuxHostToolsHelper(options) {
  if (options.hostToolsHelper) {
    await assertExecutableFile(options.hostToolsHelper, "Host Tools helper override");
    return options.hostToolsHelper;
  }
  const hostMatches =
    (options.architecture === "x64" && process.arch === "x64") ||
    (options.architecture === "arm64" && process.arch === "arm64");
  if (hostMatches) {
    for (const profile of ["release", "debug"]) {
      const candidate = path.join(repositoryRoot, "target", profile, "grok-host-tools-mcp");
      try {
        await assertExecutableFile(candidate, "Host Tools helper candidate");
        return candidate;
      } catch {
        // try next
      }
    }
  }
  throw new Error(
    "grok-host-tools-mcp binary not found; build with `cargo build -p grok-host-tools-mcp --release` or pass --host-tools-helper",
  );
}

async function assertExecutableFile(filePath, label) {
  const link = await lstat(filePath);
  if (link.isSymbolicLink()) throw new Error(`${label} must not be a symbolic link: ${filePath}`);
  const info = await stat(filePath);
  if (!info.isFile()) throw new Error(`${label} is not a file: ${filePath}`);
  // Owner-executable bit (or any exec bit) required for packaged launch.
  if ((info.mode & 0o111) === 0) throw new Error(`${label} is not executable: ${filePath}`);
}

export async function inspectElfExecutable(filePath, architecture) {
  await assertExecutableFile(filePath, "ELF executable");
  const header = (await readFile(filePath)).subarray(0, 20);
  inspectElfHeader(header, architecture);
}

function inspectElfHeader(header, architecture) {
  const expectedMachine = architecture === "x64" ? 62 : 183;
  if (header.length < 20 || !header.subarray(0, 4).equals(Buffer.from([0x7f, 0x45, 0x4c, 0x46])) ||
      header[4] !== 2 || header[5] !== 1 || header.readUInt16LE(18) !== expectedMachine) {
    throw new Error("ELF executable architecture does not match the package");
  }
}

async function openRetainedSource(filePath, label, maximumBytes, executable = false) {
  const handle = await open(filePath, fsConstants.O_RDONLY | OPEN_CLOEXEC | fsConstants.O_NOFOLLOW);
  try {
    const identity = await handle.stat({ bigint: true });
    if (!identity.isFile() || identity.size < 1n || identity.size > BigInt(maximumBytes) ||
        (executable && (identity.mode & 0o111n) === 0n)) {
      throw new Error(`${label} is not a bounded regular file`);
    }
    return { handle, identity, label };
  } catch (error) {
    await handle.close();
    throw error;
  }
}

async function openRetainedSources(specifications) {
  const sources = [];
  try {
    for (const specification of specifications) {
      sources.push(await openRetainedSource(...specification));
    }
    return sources;
  } catch (error) {
    await Promise.all(sources.map((source) => source.handle.close()));
    throw error;
  }
}

async function assertRetainedIdentity(source) {
  const current = await source.handle.stat({ bigint: true });
  for (const field of ["dev", "ino", "size", "mode"]) {
    if (current[field] !== source.identity[field]) throw new Error(`${source.label} identity changed during staging`);
  }
}

async function readRetainedSource(source) {
  const size = Number(source.identity.size);
  const bytes = Buffer.alloc(size);
  let position = 0;
  while (position < size) {
    const { bytesRead } = await source.handle.read(bytes, position, size - position, position);
    if (bytesRead === 0) throw new Error(`${source.label} was truncated during staging`);
    position += bytesRead;
  }
  await assertRetainedIdentity(source);
  return bytes;
}

async function hashRetainedSource(source) {
  const hash = createHash("sha256");
  const buffer = Buffer.allocUnsafe(1024 * 1024);
  let position = 0;
  while (position < Number(source.identity.size)) {
    const length = Math.min(buffer.length, Number(source.identity.size) - position);
    const { bytesRead } = await source.handle.read(buffer, 0, length, position);
    if (bytesRead === 0) throw new Error(`${source.label} was truncated during staging`);
    hash.update(buffer.subarray(0, bytesRead));
    position += bytesRead;
  }
  await assertRetainedIdentity(source);
  return hash.digest("hex");
}

async function copyRetainedSource(source, destination, mode, expectedDigest) {
  const output = await open(
    destination,
    fsConstants.O_WRONLY | fsConstants.O_CREAT | fsConstants.O_EXCL | OPEN_CLOEXEC | fsConstants.O_NOFOLLOW,
    mode,
  );
  const hash = createHash("sha256");
  const buffer = Buffer.allocUnsafe(1024 * 1024);
  let position = 0;
  try {
    while (position < Number(source.identity.size)) {
      const length = Math.min(buffer.length, Number(source.identity.size) - position);
      const { bytesRead } = await source.handle.read(buffer, 0, length, position);
      if (bytesRead === 0) throw new Error(`${source.label} was truncated during staging`);
      let written = 0;
      while (written < bytesRead) {
        const result = await output.write(buffer, written, bytesRead - written, position + written);
        if (result.bytesWritten === 0) throw new Error(`failed to stage ${source.label}`);
        written += result.bytesWritten;
      }
      hash.update(buffer.subarray(0, bytesRead));
      position += bytesRead;
    }
    await output.sync();
  } finally {
    await output.close();
  }
  await assertRetainedIdentity(source);
  const digest = hash.digest("hex");
  if (expectedDigest && digest !== expectedDigest) throw new Error(`${source.label} changed during staging`);
  return digest;
}

export function renderLinuxVmServiceUnit({ serviceGroup }) {
  if (!/^[a-z_][a-z0-9_-]{0,30}$/.test(serviceGroup)) throw new Error("service group is invalid");
  return linuxServiceTemplate.replace("@@SERVICE_GROUP@@", serviceGroup);
}

export function renderLinuxVmServiceEnvironment({ daemonUid }) {
  if (!Number.isInteger(daemonUid) || daemonUid < 0 || daemonUid > 0xffff_ffff) {
    throw new Error("daemon uid is invalid");
  }
  return linuxServiceEnvironmentTemplate.replace("@@DAEMON_UID@@", String(daemonUid));
}

export function renderLinuxDesktopEntry({ name, execPath, iconPath, version }) {
  return `[Desktop Entry]
Type=Application
Version=1.5
Name=${name}
Comment=Official Grok and xAI desktop workspace
Exec=${execPath} %u
Icon=${iconPath}
Terminal=false
Categories=Office;
MimeType=x-scheme-handler/grok-desktop;
StartupWMClass=${name}
X-GrokDesktop-Version=${version}
`;
}

export async function verifyLinuxPackagedLayout(appDirectory, daemonRelativePath = "resources/bin/grok-daemon") {
  const daemonPath = path.join(appDirectory, daemonRelativePath);
  await assertExecutableFile(daemonPath, "packaged daemon");
  const hostToolsHelperPath = path.join(appDirectory, "resources", "bin", "grok-host-tools-mcp");
  await assertExecutableFile(hostToolsHelperPath, "packaged Host Tools helper");
  await assertExecutableFile(
    path.join(appDirectory, "resources", "bin", "appimageupdatetool.AppImage"),
    "packaged AppImage update helper",
  );
  const updateTrust = await stat(path.join(appDirectory, "resources", "update-trusted-keys.json"));
  if (!updateTrust.isFile() || updateTrust.size < 1 || updateTrust.size > 65_536) {
    throw new Error("packaged update trust is unavailable");
  }
  const desktopEntry = path.join(appDirectory, "grok-desktop.desktop");
  const entry = await readFile(desktopEntry, "utf8");
  if (!entry.includes("x-scheme-handler/grok-desktop")) {
    throw new Error("packaged desktop entry missing grok-desktop protocol handler");
  }
  if (!entry.includes("Exec=")) throw new Error("packaged desktop entry missing Exec");
  return { daemonPath, hostToolsHelperPath, desktopEntry };
}

export async function sha256File(filePath) {
  const data = await readFile(filePath);
  return createHash("sha256").update(data).digest("hex");
}

async function ensureDesktopBuild() {
  for (const relativePath of [
    "dist/index.html",
    "dist-electron/electron/main.js",
    "dist-electron/electron/preload.cjs",
  ]) {
    const metadata = await stat(path.join(desktopRoot, relativePath)).catch(() => undefined);
    if (!metadata?.isFile()) {
      throw new Error("desktop build missing; run `pnpm --filter @grok-desktop/desktop build` first");
    }
  }
}

/** Minimal package source: production assets only (matches Windows packager source). */
export async function prepareLinuxPackagingSource(sourceRoot, packageMetadata) {
  await mkdir(path.join(sourceRoot, "node_modules", "@bufbuild"), { recursive: true });
  await cp(path.join(desktopRoot, "dist"), path.join(sourceRoot, "dist"), {
    recursive: true,
    dereference: false,
    errorOnExist: true,
  });
  await cp(path.join(desktopRoot, "dist-electron"), path.join(sourceRoot, "dist-electron"), {
    recursive: true,
    dereference: false,
    errorOnExist: true,
  });
  await cp(
    path.join(desktopRoot, "node_modules", "@bufbuild", "protobuf"),
    path.join(sourceRoot, "node_modules", "@bufbuild", "protobuf"),
    { recursive: true, dereference: true, errorOnExist: true },
  );
  const packagedMetadata = {
    name: "grok-desktop",
    productName,
    version: packageMetadata.version,
    description: packageMetadata.description,
    private: true,
    type: "module",
    main: "dist-electron/electron/main.js",
    license: "AGPL-3.0-or-later",
    dependencies: {
      "@bufbuild/protobuf": packageMetadata.dependencies["@bufbuild/protobuf"],
    },
  };
  await writeFile(
    path.join(sourceRoot, "package.json"),
    `${JSON.stringify(packagedMetadata, null, 2)}\n`,
    { encoding: "utf8", mode: 0o600 },
  );
}

export async function stageVerifiedLinuxAcp(resourcesBin, options, daemonSource, nowUnixSeconds) {
  if (!options.acpCatalog) return undefined;
  const sources = await openRetainedSources([
    [options.acpTrustFile, "ACP trust file", 4096],
    [options.acpCatalog, "ACP signed catalog", 512 * 1024],
    [options.acpComponent, "ACP component", 1024 * 1024 * 1024, true],
    [daemonSource, "daemon", 128 * 1024 * 1024, true],
  ]);
  const [trustSource, catalogSource, componentSource, daemonSourceHandle] = sources;
  try {
    const trustRaw = (await readRetainedSource(trustSource)).toString("utf8").trim();
    const trust = parseAcpCatalogTrustedKeys(trustRaw);
    const catalogBytes = await readRetainedSource(catalogSource);
    const catalog = verifyOfficialGrokCatalogBytes(
      catalogBytes, options.architecture, trust, nowUnixSeconds, "linux",
    );
    inspectDaemonAcpCatalogTrustBytes(await readRetainedSource(daemonSourceHandle), trust);
    const componentHeader = Buffer.alloc(20);
    if ((await componentSource.handle.read(componentHeader, 0, 20, 0)).bytesRead !== 20) {
      throw new Error("ACP component ELF header is truncated");
    }
    inspectElfHeader(componentHeader, options.architecture);
    const componentRoot = path.join(resourcesBin, "components", "grok-acp");
    const stagedCatalog = path.join(componentRoot, "catalog.json");
    const stagedComponent = path.join(componentRoot, "bin", "grok");
    await mkdir(path.dirname(stagedComponent), { recursive: true, mode: 0o755 });
    const catalogDigest = createHash("sha256").update(catalogBytes).digest("hex");
    await copyRetainedSource(catalogSource, stagedCatalog, 0o644, catalogDigest);
    const sourceDigest = await copyRetainedSource(
      componentSource, stagedComponent, 0o755, catalog.component.sha256,
    );
    if (componentSource.identity.size !== BigInt(catalog.component.size)) {
      throw new Error("signed ACP catalog does not match the selected Linux component size");
    }
    await inspectElfExecutable(stagedComponent, options.architecture);
    const stagedCatalogBytes = await readFile(stagedCatalog);
    if (createHash("sha256").update(stagedCatalogBytes).digest("hex") !== catalogDigest) {
      throw new Error("staged ACP catalog differs from verified bytes");
    }
    verifyOfficialGrokCatalogBytes(
      stagedCatalogBytes, options.architecture, trust, nowUnixSeconds, "linux",
    );
    return {
      catalog: stagedCatalog,
      component: stagedComponent,
      version: catalog.component.version,
      sha256: sourceDigest,
      trustBinding: trust.binding,
    };
  } finally {
    await Promise.all(sources.map((source) => source.handle.close()));
  }
}

export async function stageLinuxVmServiceBundle(out, options, daemonSource) {
  if (!options.vmService) return undefined;
  const [serviceSource, daemonSourceHandle] = await openRetainedSources([
    [options.vmService, "linux-vm-service", 128 * 1024 * 1024, true],
    [daemonSource, "daemon", 128 * 1024 * 1024, true],
  ]);
  try {
    const serviceHeader = Buffer.alloc(20);
    if ((await serviceSource.handle.read(serviceHeader, 0, 20, 0)).bytesRead !== 20) {
      throw new Error("linux-vm-service ELF header is truncated");
    }
    inspectElfHeader(serviceHeader, options.architecture);
    const digest = await hashRetainedSource(serviceSource);
    const binding = `grok-linux-vm-service-trust-v1:${createHash("sha256").update(digest).digest("hex")}`;
    const daemonBytes = await readRetainedSource(daemonSourceHandle);
    if (!daemonBytes.includes(Buffer.from(digest)) || !daemonBytes.includes(Buffer.from(binding))) {
      throw new Error("daemon was not built with the staged linux-vm-service trust binding");
    }
  const serviceRoot = path.join(out, "linux-service");
  const binary = path.join(serviceRoot, "usr", "libexec", "grok-desktop", "grok-linux-vm-service");
  const unit = path.join(serviceRoot, "usr", "lib", "systemd", "system", "grok-linux-vm-service.service");
  const environment = path.join(serviceRoot, "etc", "grok-desktop", "linux-vm-service.env");
  await mkdir(path.dirname(binary), { recursive: true, mode: 0o755 });
  await mkdir(path.dirname(unit), { recursive: true, mode: 0o755 });
  await mkdir(path.dirname(environment), { recursive: true, mode: 0o750 });
  await copyRetainedSource(serviceSource, binary, 0o755, digest);
  await writeFile(unit, renderLinuxVmServiceUnit(options), { encoding: "utf8", mode: 0o644 });
  await writeFile(environment, renderLinuxVmServiceEnvironment(options), { encoding: "utf8", mode: 0o640 });
  await inspectElfExecutable(binary, options.architecture);
  if (await sha256File(binary) !== digest) {
    throw new Error("staged linux-vm-service differs from its verified source bytes");
  }
    return { serviceRoot, binary, unit, environment, sha256: digest, trustBinding: binding };
  } finally {
    await Promise.all([serviceSource.handle.close(), daemonSourceHandle.handle.close()]);
  }
}

async function main() {
  if (process.platform !== "linux") {
    throw new Error("Linux packages must be assembled on a Linux host");
  }
  const options = parseLinuxPackageArguments(process.argv.slice(2));
  const packageMetadata = JSON.parse(await readFile(path.join(desktopRoot, "package.json"), "utf8"));
  await ensureDesktopBuild();
  const daemonSource = await resolveLinuxDaemonBinary(options);
  const hostToolsHelperSource = await resolveLinuxHostToolsHelper(options);

  await rm(options.out, { recursive: true, force: true });
  await mkdir(options.out, { recursive: true, mode: 0o700 });
  await chmod(options.out, 0o700);

  const resourcesBin = path.join(options.out, "stage-resources", "bin");
  await mkdir(resourcesBin, { recursive: true });
  const stagedDaemon = path.join(resourcesBin, "grok-daemon");
  await cp(daemonSource, stagedDaemon);
  await chmod(stagedDaemon, 0o755);
  const stagedHostToolsHelper = path.join(resourcesBin, "grok-host-tools-mcp");
  await cp(hostToolsHelperSource, stagedHostToolsHelper);
  await chmod(stagedHostToolsHelper, 0o755);
  const stagedUpdateTool = await stageAppImageUpdateTool(resourcesBin, options);
  const stagedUpdateTrust = await stageUpdateTrust(path.dirname(resourcesBin), options);
  const stagedAcp = await stageVerifiedLinuxAcp(
    resourcesBin, options, daemonSource, Math.floor(Date.now() / 1000),
  );
  const stagedVmService = await stageLinuxVmServiceBundle(options.out, options, daemonSource);

  const temporaryRoot = await mkdtemp(path.join(os.tmpdir(), "grok-desktop-linux-pkg-"));
  try {
    const sourceRoot = path.join(temporaryRoot, "source");
    await prepareLinuxPackagingSource(sourceRoot, packageMetadata);

    const electronVersion = (
      packageMetadata.devDependencies?.electron ?? packageMetadata.dependencies?.electron ?? ""
    ).replace(/^[^\d]*/, "");
    if (!/^\d+\.\d+\.\d+/.test(electronVersion)) {
      throw new Error("package.json must pin electron x.y.z for linux packaging");
    }
    const packagedDirectories = await packager({
      dir: sourceRoot,
      out: path.join(options.out, "unpacked"),
      overwrite: true,
      platform: "linux",
      arch: options.architecture,
      name: productName,
      executableName,
      appVersion: packageMetadata.version,
      appCopyright: "Copyright (c) 2026 Grok Insider",
      asar: true,
      prune: true,
      electronVersion,
      extraResource: [resourcesBin, stagedUpdateTrust, path.join(desktopRoot, "assets", "tray")],
    });
    if (packagedDirectories.length !== 1) {
      throw new Error("Electron Packager returned an unexpected target set");
    }
    const appDirectory = packagedDirectories[0];
    const executable = path.join(appDirectory, executableName);
    await assertExecutableFile(executable, "packaged electron executable");

    const packagedDaemon = path.join(appDirectory, "resources", "bin", "grok-daemon");
    await assertExecutableFile(packagedDaemon, "packaged daemon");

    const desktopEntry = renderLinuxDesktopEntry({
      name: productName,
      execPath: executable,
      iconPath: path.join(appDirectory, "resources", "tray", "tray-dark-32.png"),
      version: packageMetadata.version,
    });
    await writeFile(path.join(appDirectory, "grok-desktop.desktop"), desktopEntry, {
      encoding: "utf8",
      mode: 0o644,
    });

    await verifyLinuxPackagedLayout(appDirectory);
    const appImage = await createLinuxAppImage(appDirectory, options.out, options, packageMetadata.version);

    const record = {
      schemaVersion: 1,
      product: "grok-desktop",
      platform: "linux",
      version: packageMetadata.version,
      architecture: options.architecture,
      appDirectory,
      executable,
      appImage,
      appImageSha256: await sha256File(appImage),
      updateToolSha256: await sha256File(stagedUpdateTool),
      daemonSha256: await sha256File(packagedDaemon),
      daemonSource,
      acp: stagedAcp ? {
        staged: true,
        version: stagedAcp.version,
        sha256: stagedAcp.sha256,
        trustBinding: stagedAcp.trustBinding,
      } : { staged: false },
      vmService: stagedVmService ? {
        staged: true,
        sha256: stagedVmService.sha256,
        serviceGroup: options.serviceGroup,
        daemonUid: options.daemonUid,
      } : { staged: false },
      isolation: "not_embedded",
      notes:
        "Work isolation requires the separate linux-vm-service and signed guest image; this package is fail-closed Limited Mode for Work.",
      builtAtUnixMs: Date.now(),
      host: { platform: process.platform, arch: process.arch, release: os.release() },
    };
    await writeFile(
      path.join(options.out, "linux-package.json"),
      `${JSON.stringify(record, null, 2)}\n`,
      "utf8",
    );
    process.stdout.write(
      `${JSON.stringify({ ok: true, appDirectory, appImage, recordPath: path.join(options.out, "linux-package.json") })}\n`,
    );
  } finally {
    await rm(temporaryRoot, { recursive: true, force: true });
  }
}

const isMain = process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  main().catch((error) => {
    process.stderr.write(`${error instanceof Error ? error.stack ?? error.message : String(error)}\n`);
    process.exitCode = 1;
  });
}
