import { spawn } from "node:child_process";
import { constants as fsConstants } from "node:fs";
import { cp, copyFile, lstat, mkdir, mkdtemp, readFile, readdir, realpath, rm, stat } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import {
  inspectDaemonAcpCatalogTrust,
  inspectDaemonAcpPinnedManifestBytes,
  inspectPortableExecutable,
  parseAcpCatalogTrustedKeys,
  parseStrictBoundedJSON,
  verifyOfficialGrokPinnedManifestBytes,
} from "./release-utils.mjs";

const desktopRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const repositoryRoot = path.resolve(desktopRoot, "../..");
const rustTargets = { x64: "x86_64-pc-windows-msvc", arm64: "aarch64-pc-windows-msvc" };
const targetLinkerVariables = {
  x64: "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER",
  arm64: "CARGO_TARGET_AARCH64_PC_WINDOWS_MSVC_LINKER",
};
const forbiddenCargoHomeEntries = new Set([
  "config", "config.toml", "credentials", "credentials.toml",
]);

export function parseDaemonBuildArguments(arguments_) {
  const values = {};
  for (let index = 0; index < arguments_.length; index += 2) {
    const option = arguments_[index];
    const value = arguments_[index + 1];
    if (!new Set(["--arch", "--out", "--acp-pinned-manifest"]).has(option) ||
        value === undefined || values[option]) {
      throw new Error("daemon build requires unique --arch and --out option/value pairs");
    }
    values[option] = value;
  }
  if (!Object.hasOwn(rustTargets, values["--arch"])) throw new Error("--arch must be x64 or arm64");
  if (!values["--out"] || path.basename(values["--out"]).toLowerCase() !== "grok-daemon.exe") {
    throw new Error("--out must name grok-daemon.exe");
  }
  return {
    architecture: values["--arch"],
    output: path.resolve(values["--out"]),
    ...(values["--acp-pinned-manifest"]
      ? { pinnedManifest: path.resolve(values["--acp-pinned-manifest"]) }
      : {}),
  };
}

async function main() {
  if (process.platform !== "win32") throw new Error("release daemons must be built on a trusted Windows worker");
  const options = parseDaemonBuildArguments(process.argv.slice(2));
  const cargoPath = await requiredWindowsTool("GROK_WINDOWS_CARGO_PATH");
  const rustcPath = await requiredWindowsTool("GROK_WINDOWS_RUSTC_PATH");
  const linkerPath = await requiredWindowsTool("GROK_WINDOWS_LINKER_PATH");
  const cargoCache = await requiredWindowsDirectory("GROK_WINDOWS_CARGO_CACHE");
  const toolchainEnvironment = await canonicalizeToolchainEnvironment(
    parseWindowsToolchainEnvironment(
      requiredEnvironment("GROK_WINDOWS_TOOLCHAIN_ENV_JSON", 65_536),
    ),
  );
  const trust = options.pinnedManifest
    ? verifyOfficialGrokPinnedManifestBytes(
      await readFile(options.pinnedManifest), options.architecture, "windows",
    )
    : parseAcpCatalogTrustedKeys(requiredEnvironment("GROK_ACP_CATALOG_TRUSTED_KEYS", 4096));
  const temporaryRoot = await mkdtemp(path.join(os.tmpdir(), "grok-daemon-build-"));
  try {
    const buildLayout = {
      cargoHome: path.join(temporaryRoot, "cargo-home"),
      homeDirectory: path.join(temporaryRoot, "home"),
      targetDirectory: path.join(temporaryRoot, "target"),
      temporaryDirectory: path.join(temporaryRoot, "tmp"),
      workingDirectory: path.join(temporaryRoot, "work"),
    };
    await Promise.all([
      mkdir(path.join(buildLayout.homeDirectory, "AppData", "Local"), { recursive: true }),
      mkdir(buildLayout.temporaryDirectory, { recursive: false }),
      mkdir(buildLayout.workingDirectory, { recursive: false }),
    ]);
    await prepareIsolatedCargoHome(cargoCache, buildLayout.cargoHome);
    await assertNoCargoConfigurationInAncestors(buildLayout.workingDirectory);
    const buildEnvironment = createWindowsDaemonBuildEnvironment(
      process.env,
      options.architecture,
      buildLayout,
      trust,
      { rustcPath, linkerPath, toolchainEnvironment },
    );
    await run(cargoPath, [
      "build",
      "--manifest-path", path.join(repositoryRoot, "Cargo.toml"),
      "--locked",
      "--offline",
      "--release",
      "--target", rustTargets[options.architecture],
      "--package", "grok-daemon",
      "--bin", "grok-daemon",
      "--package", "grok-host-tools-mcp",
      "--bin", "grok-host-tools-mcp",
      "--no-default-features",
    ], { cwd: buildLayout.workingDirectory, env: buildEnvironment });
    const executable = path.join(
      buildLayout.targetDirectory, rustTargets[options.architecture], "release", "grok-daemon.exe",
    );
    await inspectPortableExecutable(executable, options.architecture);
    if (options.pinnedManifest) {
      inspectDaemonAcpPinnedManifestBytes(await readFile(executable), trust);
    } else {
      await inspectDaemonAcpCatalogTrust(executable, trust);
    }
    const hostToolsHelper = path.join(
      buildLayout.targetDirectory, rustTargets[options.architecture], "release", "grok-host-tools-mcp.exe",
    );
    await inspectPortableExecutable(hostToolsHelper, options.architecture);
    await mkdir(path.dirname(options.output), { recursive: true });
    await copyFile(executable, options.output, fsConstants.COPYFILE_EXCL);
    await copyFile(
      hostToolsHelper,
      path.join(path.dirname(options.output), "grok-host-tools-mcp.exe"),
      fsConstants.COPYFILE_EXCL,
    );
  } finally {
    await rm(temporaryRoot, { recursive: true, force: true });
  }
}

export function createWindowsDaemonBuildEnvironment(_environment, architecture, layout, trust, toolchain) {
  if (!Object.hasOwn(rustTargets, architecture) || !validBuildLayout(layout) ||
      !trust?.binding || (!trust.raw && !trust.sourceUrl) || !validToolchain(toolchain)) {
    throw new Error("Windows daemon build configuration is invalid");
  }
  return {
    CARGO_HOME: layout.cargoHome,
    CARGO_INCREMENTAL: "0",
    CARGO_NET_OFFLINE: "true",
    CARGO_TARGET_DIR: layout.targetDirectory,
    CARGO_TERM_COLOR: "never",
    ...(trust.raw ? {
      GROK_ACP_CATALOG_TRUSTED_KEYS: trust.raw,
      GROK_ACP_CATALOG_TRUST_BINDING: trust.binding,
    } : { GROK_ACP_PINNED_MANIFEST_BINDING: trust.binding }),
    HOME: layout.homeDirectory,
    INCLUDE: toolchain.toolchainEnvironment.includePaths.join(path.win32.delimiter),
    LIB: toolchain.toolchainEnvironment.libraryPaths.join(path.win32.delimiter),
    LIBPATH: toolchain.toolchainEnvironment.librarySearchPaths.join(path.win32.delimiter),
    LOCALAPPDATA: path.join(layout.homeDirectory, "AppData", "Local"),
    PATH: toolchain.toolchainEnvironment.executablePaths.join(path.win32.delimiter),
    RUSTC: toolchain.rustcPath,
    RUST_BACKTRACE: "0",
    SystemRoot: toolchain.toolchainEnvironment.systemRoot,
    TEMP: layout.temporaryDirectory,
    TMP: layout.temporaryDirectory,
    USERPROFILE: layout.homeDirectory,
    VCINSTALLDIR: toolchain.toolchainEnvironment.visualCppInstallRoot,
    VSCMD_ARG_TGT_ARCH: architecture === "x64" ? "x64" : "arm64",
    WINDIR: toolchain.toolchainEnvironment.systemRoot,
    [targetLinkerVariables[architecture]]: toolchain.linkerPath,
  };
}

export function parseWindowsToolchainEnvironment(raw) {
  const value = parseStrictBoundedJSON(
    Buffer.from(raw, "utf8"),
    65_536,
    "GROK_WINDOWS_TOOLCHAIN_ENV_JSON",
  );
  const expectedKeys = [
    "executablePaths",
    "includePaths",
    "libraryPaths",
    "librarySearchPaths",
    "systemRoot",
    "visualCppInstallRoot",
  ];
  if (!value || typeof value !== "object" || Array.isArray(value) ||
      Object.keys(value).toSorted().join(",") !== expectedKeys.toSorted().join(",") ||
      typeof value.systemRoot !== "string" || !validWindowsAbsolutePath(value.systemRoot) ||
      typeof value.visualCppInstallRoot !== "string" || !validWindowsAbsolutePath(value.visualCppInstallRoot)) {
    throw new Error("GROK_WINDOWS_TOOLCHAIN_ENV_JSON has an invalid schema");
  }
  return {
    systemRoot: value.systemRoot,
    visualCppInstallRoot: value.visualCppInstallRoot,
    executablePaths: parseWindowsPathList(value.executablePaths),
    includePaths: parseWindowsPathList(value.includePaths),
    libraryPaths: parseWindowsPathList(value.libraryPaths),
    librarySearchPaths: parseWindowsPathList(value.librarySearchPaths),
  };
}

export async function prepareIsolatedCargoHome(source, destination) {
  const canonicalSource = await realpath(source);
  if (!path.isAbsolute(canonicalSource) || !path.isAbsolute(destination) ||
      path.resolve(canonicalSource) === path.resolve(destination) || !(await stat(canonicalSource)).isDirectory()) {
    throw new Error("trusted Cargo cache paths are invalid");
  }
  const rootEntries = await readdir(canonicalSource, { withFileTypes: true });
  if (rootEntries.some((entry) => forbiddenCargoHomeEntries.has(entry.name.toLowerCase()))) {
    throw new Error("trusted Cargo cache must not contain Cargo configuration or credentials");
  }
  const registry = rootEntries.find((entry) => entry.name === "registry");
  if (!registry?.isDirectory() || registry.isSymbolicLink()) {
    throw new Error("trusted Cargo cache does not contain a regular registry cache");
  }
  await mkdir(destination, { recursive: false });
  await mkdir(path.join(destination, "registry"), { recursive: false });
  const registryEntries = await readdir(path.join(canonicalSource, "registry"), { withFileTypes: true });
  for (const name of ["cache", "index"]) {
    const entry = registryEntries.find((candidate) => candidate.name === name);
    if (!entry?.isDirectory() || entry.isSymbolicLink()) {
      throw new Error("trusted Cargo cache must contain regular registry index and archive directories");
    }
    await cp(path.join(canonicalSource, "registry", name), path.join(destination, "registry", name), {
      recursive: true,
      dereference: false,
      errorOnExist: true,
    });
  }
  await assertRegularTree(destination);
}

export async function assertNoCargoConfigurationInAncestors(workingDirectory) {
  let current = await realpath(workingDirectory);
  while (true) {
    const cargoDirectory = path.join(current, ".cargo");
    const directoryMetadata = await lstat(cargoDirectory).catch((error) => {
      if (error?.code === "ENOENT") return undefined;
      throw error;
    });
    if (directoryMetadata?.isSymbolicLink()) {
      throw new Error("Cargo configuration search path contains a symbolic link");
    }
    for (const name of ["config", "config.toml"]) {
      const configuration = await lstat(path.join(cargoDirectory, name)).catch((error) => {
        if (error?.code === "ENOENT") return undefined;
        throw error;
      });
      if (configuration) throw new Error("Cargo configuration is forbidden in build path ancestors");
    }
    const parent = path.dirname(current);
    if (parent === current) return;
    current = parent;
  }
}

function parseWindowsPathList(value) {
  if (!Array.isArray(value) || value.length < 1 || value.length > 64) {
    throw new Error("trusted Windows toolchain path lists must contain 1 to 64 entries");
  }
  const seen = new Set();
  return value.map((entry) => {
    if (typeof entry !== "string" || !validWindowsAbsolutePath(entry) || entry.includes(path.win32.delimiter)) {
      throw new Error("trusted Windows toolchain paths must be absolute directories");
    }
    const key = path.win32.normalize(entry).toLowerCase();
    if (seen.has(key)) throw new Error("trusted Windows toolchain paths must be unique");
    seen.add(key);
    return entry;
  });
}

function validWindowsAbsolutePath(value) {
  return value.length > 2 && value.length <= 32_000 && !value.includes("\0") &&
    path.win32.isAbsolute(value) && !value.startsWith("\\\\");
}

function validBuildLayout(layout) {
  const values = layout && [
    layout.cargoHome,
    layout.homeDirectory,
    layout.targetDirectory,
    layout.temporaryDirectory,
    layout.workingDirectory,
  ];
  return Array.isArray(values) && values.every((value) => typeof value === "string" && path.isAbsolute(value)) &&
    new Set(values.map((value) => path.resolve(value))).size === values.length;
}

function validToolchain(toolchain) {
  const environment = toolchain?.toolchainEnvironment;
  return typeof toolchain?.rustcPath === "string" && validWindowsAbsolutePath(toolchain.rustcPath) &&
    typeof toolchain.linkerPath === "string" && validWindowsAbsolutePath(toolchain.linkerPath) &&
    environment && validWindowsAbsolutePath(environment.systemRoot) &&
    validWindowsAbsolutePath(environment.visualCppInstallRoot) &&
    [environment.executablePaths, environment.includePaths, environment.libraryPaths, environment.librarySearchPaths]
      .every((values) => Array.isArray(values) && values.length > 0 && values.every(validWindowsAbsolutePath));
}

async function requiredWindowsTool(name) {
  const value = requiredEnvironment(name, 1024);
  if (!validWindowsAbsolutePath(value)) throw new Error(`${name} must be an absolute local Windows path`);
  const metadata = await lstat(value);
  const canonical = await realpath(value);
  if (metadata.isSymbolicLink() || !(await stat(canonical)).isFile()) {
    throw new Error(`${name} must identify a regular trusted tool`);
  }
  return canonical;
}

async function requiredWindowsDirectory(name) {
  const value = requiredEnvironment(name, 1024);
  if (!validWindowsAbsolutePath(value)) throw new Error(`${name} must be an absolute local Windows path`);
  const metadata = await lstat(value);
  const canonical = await realpath(value);
  if (metadata.isSymbolicLink() || !(await stat(canonical)).isDirectory()) {
    throw new Error(`${name} must identify a regular trusted directory`);
  }
  return canonical;
}

async function canonicalizeToolchainEnvironment(environment) {
  const canonicalizeDirectories = (values) => Promise.all(values.map(trustedDirectory));
  return {
    systemRoot: await trustedDirectory(environment.systemRoot),
    visualCppInstallRoot: await trustedDirectory(environment.visualCppInstallRoot),
    executablePaths: await canonicalizeDirectories(environment.executablePaths),
    includePaths: await canonicalizeDirectories(environment.includePaths),
    libraryPaths: await canonicalizeDirectories(environment.libraryPaths),
    librarySearchPaths: await canonicalizeDirectories(environment.librarySearchPaths),
  };
}

async function trustedDirectory(value) {
  const metadata = await lstat(value);
  const canonical = await realpath(value);
  if (metadata.isSymbolicLink() || !(await stat(canonical)).isDirectory()) {
    throw new Error("trusted Windows toolchain path is not a regular directory");
  }
  return canonical;
}

async function assertRegularTree(root) {
  for (const entry of await readdir(root, { withFileTypes: true })) {
    if (entry.isSymbolicLink()) throw new Error("trusted Cargo cache contains a symbolic link");
    const candidate = path.join(root, entry.name);
    if (entry.isDirectory()) await assertRegularTree(candidate);
    else if (!entry.isFile()) throw new Error("trusted Cargo cache contains an unsupported filesystem object");
  }
}

function run(executable, arguments_, options) {
  return new Promise((resolve, reject) => {
    const child = spawn(executable, arguments_, {
      ...options, shell: false, stdio: "inherit", windowsHide: true,
    });
    child.once("error", reject);
    child.once("exit", (code, signal) => {
      if (code === 0 && signal === null) resolve();
      else reject(new Error("trusted Windows daemon build failed"));
    });
  });
}

function requiredEnvironment(name, maximum) {
  const value = process.env[name];
  if (typeof value !== "string" || value.length < 1 || value.length > maximum || value.includes("\0")) {
    throw new Error(`${name} is required and bounded`);
  }
  return value;
}

if (process.argv[1] && pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url) {
  main().catch(() => {
    process.stderr.write("Windows daemon release build failed\n");
    process.exitCode = 1;
  });
}
