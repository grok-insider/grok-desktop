/**
 * Linux desktop packaging for Grok Desktop (Limited Mode / full GA train T1).
 *
 * Embeds the release or debug grok-daemon next to Electron resources and writes
 * a desktop entry + layout manifest. Does not claim Work isolation; the Linux
 * VM broker is packaged separately when present.
 */
import { cp, mkdir, mkdtemp, readFile, rm, stat, writeFile, chmod } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";
import { packager } from "@electron/packager";

const desktopRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const repositoryRoot = path.resolve(desktopRoot, "../..");
const productName = "Grok Desktop";
const executableName = "grok-desktop";

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
    if (option !== "--arch" && option !== "--out" && option !== "--daemon") {
      throw new Error(`unsupported linux package option ${option}`);
    }
    if (values[option]) throw new Error(`linux package option ${option} was repeated`);
    values[option] = value;
  }
  const architecture = values["--arch"] ?? (process.arch === "arm64" ? "arm64" : "x64");
  if (!LINUX_PACKAGE_ARCHITECTURES.has(architecture)) {
    throw new Error("--arch must be x64 or arm64");
  }
  return {
    architecture,
    out: values["--out"]
      ? path.resolve(values["--out"])
      : path.join(repositoryRoot, "out", "release", "linux", architecture),
    daemonBinary: values["--daemon"] ? path.resolve(values["--daemon"]) : undefined,
  };
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

async function assertExecutableFile(filePath, label) {
  const info = await stat(filePath);
  if (!info.isFile()) throw new Error(`${label} is not a file: ${filePath}`);
  // Owner-executable bit (or any exec bit) required for packaged launch.
  if ((info.mode & 0o111) === 0) throw new Error(`${label} is not executable: ${filePath}`);
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
Categories=Network;Office;
MimeType=x-scheme-handler/grok-desktop;
StartupWMClass=${name}
X-GrokDesktop-Version=${version}
`;
}

export async function verifyLinuxPackagedLayout(appDirectory, daemonRelativePath = "resources/bin/grok-daemon") {
  const daemonPath = path.join(appDirectory, daemonRelativePath);
  await assertExecutableFile(daemonPath, "packaged daemon");
  const desktopEntry = path.join(appDirectory, "grok-desktop.desktop");
  const entry = await readFile(desktopEntry, "utf8");
  if (!entry.includes("x-scheme-handler/grok-desktop")) {
    throw new Error("packaged desktop entry missing grok-desktop protocol handler");
  }
  if (!entry.includes("Exec=")) throw new Error("packaged desktop entry missing Exec");
  return { daemonPath, desktopEntry };
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

async function main() {
  if (process.platform !== "linux") {
    throw new Error("Linux packages must be assembled on a Linux host");
  }
  const options = parseLinuxPackageArguments(process.argv.slice(2));
  const packageMetadata = JSON.parse(await readFile(path.join(desktopRoot, "package.json"), "utf8"));
  await ensureDesktopBuild();
  const daemonSource = await resolveLinuxDaemonBinary(options);

  await rm(options.out, { recursive: true, force: true });
  await mkdir(options.out, { recursive: true });

  const resourcesBin = path.join(options.out, "stage-resources", "bin");
  await mkdir(resourcesBin, { recursive: true });
  const stagedDaemon = path.join(resourcesBin, "grok-daemon");
  await cp(daemonSource, stagedDaemon);
  await chmod(stagedDaemon, 0o755);

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
      extraResource: [resourcesBin, path.join(desktopRoot, "assets", "tray")],
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

    const record = {
      schemaVersion: 1,
      product: "grok-desktop",
      platform: "linux",
      version: packageMetadata.version,
      architecture: options.architecture,
      appDirectory,
      executable,
      daemonSha256: await sha256File(packagedDaemon),
      daemonSource,
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
      `${JSON.stringify({ ok: true, appDirectory, recordPath: path.join(options.out, "linux-package.json") })}\n`,
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
