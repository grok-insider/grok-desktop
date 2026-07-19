import { spawn } from "node:child_process";
import { constants as fsConstants } from "node:fs";
import {
  access,
  cp,
  lstat,
  mkdir,
  mkdtemp,
  readdir,
  realpath,
  rm,
  stat,
  writeFile,
} from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const desktopRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const repositoryRoot = path.resolve(desktopRoot, "../..");
const rustTargetByArch = {
  x64: "x86_64-pc-windows-msvc",
  arm64: "aarch64-pc-windows-msvc",
};
const hostDirectoryByArch = { x64: "Hostx64", arm64: "Hostarm64" };
const libDirectoryByArch = { x64: "x64", arm64: "arm64" };
const forbiddenCargoHomeEntries = new Set([
  "config", "config.toml", "credentials", "credentials.toml",
]);

/**
 * Resolve GitHub-hosted Windows release tools and emit GITHUB_ENV assignments
 * for the existing build-windows-daemon isolation contract.
 */
export async function resolveWindowsReleaseToolchain(options = {}) {
  const architecture = options.architecture ?? "x64";
  if (!Object.hasOwn(rustTargetByArch, architecture)) {
    throw new Error("--arch must be x64 or arm64");
  }
  if (process.platform !== "win32" && !options.allowNonWindows) {
    throw new Error("Windows release toolchain resolution requires win32");
  }

  const cargoPath = await resolveRegularFile(
    options.cargoPath ?? await resolveRustupTool("cargo", options.allowNonWindows),
    "cargo",
    options.allowNonWindows,
  );
  const rustcPath = await resolveRegularFile(
    options.rustcPath ?? await resolveRustupTool("rustc", options.allowNonWindows),
    "rustc",
    options.allowNonWindows,
  );
  const visualStudio = options.visualStudioRoot
    ? await trustedDirectory(options.visualStudioRoot, options.allowNonWindows)
    : await discoverVisualStudioRoot();
  const msvcVersion = options.msvcVersion
    ?? selectNewestMsvcVersion(await listMsvcVersions(visualStudio));
  const windowsSdkRoot = options.windowsSdkRoot
    ? await trustedDirectory(options.windowsSdkRoot, options.allowNonWindows)
    : await discoverWindowsSdkRoot();
  const windowsSdkVersion = options.windowsSdkVersion
    ?? selectNewestWindowsSdkVersion(await listWindowsSdkVersions(windowsSdkRoot));
  const pathImpl = options.pathImpl
    ?? (options.allowNonWindows ? path : path.win32);
  const layout = buildMsvcLayout({
    architecture,
    visualStudioRoot: visualStudio,
    msvcVersion,
    windowsSdkRoot,
    windowsSdkVersion,
    systemRoot: options.systemRoot
      ?? process.env.SystemRoot
      ?? (options.allowNonWindows ? path.join(os.tmpdir(), "windows-system-root") : "C:\\Windows"),
    pathImpl,
  });
  const linkerPath = await resolveRegularFile(
    layout.linkerPath, "link.exe", options.allowNonWindows,
  );
  const toolchainEnvironment = await canonicalizeExistingToolchainEnvironment(
    layout.toolchainEnvironment,
    options.allowNonWindows,
  );

  const temporaryRoot = options.temporaryRoot
    ?? await mkdtemp(path.join(os.tmpdir(), "grok-windows-release-toolchain-"));
  const cargoCache = options.cargoCache
    ?? path.join(temporaryRoot, "trusted-cargo-cache");
  if (!options.skipCargoHydration) {
    await hydrateTrustedCargoCache({
      cargoPath,
      rustcPath,
      repositoryRoot: options.repositoryRoot ?? repositoryRoot,
      target: rustTargetByArch[architecture],
      cargoCache,
      temporaryRoot,
      run: options.run ?? run,
      pathEntries: [
        path.dirname(cargoPath),
        path.dirname(rustcPath),
        path.dirname(linkerPath),
        ...toolchainEnvironment.executablePaths,
      ],
      systemRoot: toolchainEnvironment.systemRoot,
    });
  } else {
    await mkdir(cargoCache, { recursive: true });
    await mkdir(path.join(cargoCache, "registry", "cache"), { recursive: true });
    await mkdir(path.join(cargoCache, "registry", "index"), { recursive: true });
  }

  return {
    cargoPath,
    rustcPath,
    linkerPath,
    cargoCache: await trustedDirectory(cargoCache, options.allowNonWindows),
    toolchainEnvironment,
    toolchainEnvironmentJSON: JSON.stringify({
      systemRoot: toolchainEnvironment.systemRoot,
      visualCppInstallRoot: toolchainEnvironment.visualCppInstallRoot,
      executablePaths: toolchainEnvironment.executablePaths,
      includePaths: toolchainEnvironment.includePaths,
      libraryPaths: toolchainEnvironment.libraryPaths,
      librarySearchPaths: toolchainEnvironment.librarySearchPaths,
    }),
    temporaryRoot,
  };
}

export function buildMsvcLayout({
  architecture,
  visualStudioRoot,
  msvcVersion,
  windowsSdkRoot,
  windowsSdkVersion,
  systemRoot,
  pathImpl = path.win32,
}) {
  if (!Object.hasOwn(rustTargetByArch, architecture)) {
    throw new Error("architecture must be x64 or arm64");
  }
  for (const [name, value] of Object.entries({
    visualStudioRoot, msvcVersion, windowsSdkRoot, windowsSdkVersion, systemRoot,
  })) {
    if (typeof value !== "string" || value.length < 1 || value.includes("\0")) {
      throw new Error(`${name} is required`);
    }
  }

  const host = hostDirectoryByArch[architecture];
  const libArch = libDirectoryByArch[architecture];
  const msvcRoot = pathImpl.join(visualStudioRoot, "VC", "Tools", "MSVC", msvcVersion);
  const vcToolsBin = pathImpl.join(msvcRoot, "bin", host, libArch);
  const vcInclude = pathImpl.join(msvcRoot, "include");
  const vcLib = pathImpl.join(msvcRoot, "lib", libArch);
  const sdkInclude = pathImpl.join(windowsSdkRoot, "Include", windowsSdkVersion);
  const sdkLib = pathImpl.join(windowsSdkRoot, "Lib", windowsSdkVersion);
  const visualCppInstallRoot = pathImpl.join(visualStudioRoot, "VC");

  return {
    linkerPath: pathImpl.join(vcToolsBin, "link.exe"),
    toolchainEnvironment: {
      systemRoot,
      visualCppInstallRoot,
      executablePaths: [vcToolsBin],
      includePaths: [
        vcInclude,
        pathImpl.join(sdkInclude, "ucrt"),
        pathImpl.join(sdkInclude, "um"),
        pathImpl.join(sdkInclude, "shared"),
      ],
      libraryPaths: [
        vcLib,
        pathImpl.join(sdkLib, "ucrt", libArch),
        pathImpl.join(sdkLib, "um", libArch),
      ],
      librarySearchPaths: [
        vcLib,
        pathImpl.join(sdkLib, "ucrt", libArch),
        pathImpl.join(sdkLib, "um", libArch),
      ],
    },
  };
}

export function selectNewestMsvcVersion(versions) {
  if (!Array.isArray(versions) || versions.length < 1) {
    throw new Error("no MSVC toolset versions found");
  }
  for (const version of versions) {
    if (typeof version !== "string" || !/^\d+\.\d+\.\d+$/.test(version)) {
      throw new Error("MSVC toolset version is invalid");
    }
  }
  return versions.toSorted(compareDottedVersions).at(-1);
}

export function selectNewestWindowsSdkVersion(versions) {
  if (!Array.isArray(versions) || versions.length < 1) {
    throw new Error("no Windows SDK versions found");
  }
  for (const version of versions) {
    if (typeof version !== "string" || !/^\d+\.\d+\.\d+\.\d+$/.test(version)) {
      throw new Error("Windows SDK version is invalid");
    }
  }
  return versions.toSorted(compareDottedVersions).at(-1);
}

export function formatGithubEnvAssignments(resolved) {
  const entries = {
    GROK_WINDOWS_CARGO_PATH: resolved.cargoPath,
    GROK_WINDOWS_RUSTC_PATH: resolved.rustcPath,
    GROK_WINDOWS_LINKER_PATH: resolved.linkerPath,
    GROK_WINDOWS_CARGO_CACHE: resolved.cargoCache,
    GROK_WINDOWS_TOOLCHAIN_ENV_JSON: resolved.toolchainEnvironmentJSON,
  };
  return Object.entries(entries).map(([name, value]) => {
    if (typeof value !== "string" || value.length < 1 || value.includes("\0")) {
      throw new Error(`${name} cannot be exported to GITHUB_ENV`);
    }
    if (name === "GROK_WINDOWS_TOOLCHAIN_ENV_JSON" || value.includes("\n")) {
      return `${name}<<GROK_TOOLCHAIN_EOF\n${value}\nGROK_TOOLCHAIN_EOF`;
    }
    return `${name}=${value}`;
  }).join("\n") + "\n";
}

export async function listMsvcVersions(visualStudioRoot) {
  const msvcRoot = path.join(visualStudioRoot, "VC", "Tools", "MSVC");
  const entries = await readdir(msvcRoot, { withFileTypes: true });
  const versions = [];
  for (const entry of entries) {
    if (!entry.isDirectory() || entry.isSymbolicLink()) continue;
    if (!/^\d+\.\d+\.\d+$/.test(entry.name)) continue;
    versions.push(entry.name);
  }
  return versions;
}

export async function listWindowsSdkVersions(windowsSdkRoot) {
  const includeRoot = path.join(windowsSdkRoot, "Include");
  const entries = await readdir(includeRoot, { withFileTypes: true });
  const versions = [];
  for (const entry of entries) {
    if (!entry.isDirectory() || entry.isSymbolicLink()) continue;
    if (!/^\d+\.\d+\.\d+\.\d+$/.test(entry.name)) continue;
    if (!(await pathExists(path.join(includeRoot, entry.name, "ucrt")))) continue;
    versions.push(entry.name);
  }
  return versions;
}

export async function hydrateTrustedCargoCache({
  cargoPath,
  rustcPath,
  repositoryRoot: root,
  target,
  cargoCache,
  temporaryRoot,
  run: runCommand,
  pathEntries,
  systemRoot,
}) {
  if (typeof target !== "string" || !target.includes("windows-msvc")) {
    throw new Error("cargo fetch target must be a Windows MSVC triple");
  }
  const fetchHome = path.join(temporaryRoot, "cargo-fetch-home");
  const tmpDirectory = path.join(temporaryRoot, "tmp");
  const homeDirectory = path.join(temporaryRoot, "home");
  await rm(fetchHome, { recursive: true, force: true });
  await mkdir(fetchHome, { recursive: false });
  await mkdir(tmpDirectory, { recursive: true });
  await mkdir(homeDirectory, { recursive: true });

  const fetchEnv = {
    CARGO_HOME: fetchHome,
    CARGO_TERM_COLOR: "never",
    PATH: uniquePathList(pathEntries),
    RUSTC: rustcPath,
    SystemRoot: systemRoot,
    TEMP: tmpDirectory,
    TMP: tmpDirectory,
    USERPROFILE: homeDirectory,
    HOME: homeDirectory,
  };
  // Keep rustup metadata available if cargo is still a proxy shim.
  if (process.env.RUSTUP_HOME) fetchEnv.RUSTUP_HOME = process.env.RUSTUP_HOME;
  if (process.env.RUSTUP_TOOLCHAIN) fetchEnv.RUSTUP_TOOLCHAIN = process.env.RUSTUP_TOOLCHAIN;
  await runCommand(cargoPath, [
    "fetch",
    "--locked",
    "--manifest-path", path.join(root, "Cargo.toml"),
    "--target", target,
  ], {
    cwd: root,
    env: fetchEnv,
  });

  await rm(cargoCache, { recursive: true, force: true });
  await mkdir(cargoCache, { recursive: false });
  await mkdir(path.join(cargoCache, "registry"), { recursive: false });

  const registryRoot = path.join(fetchHome, "registry");
  if (!(await pathExists(registryRoot))) {
    throw new Error("cargo fetch did not create a registry cache");
  }
  for (const name of ["cache", "index"]) {
    const source = path.join(registryRoot, name);
    if (!(await pathExists(source))) {
      throw new Error(`cargo fetch registry is missing ${name}`);
    }
    await cp(source, path.join(cargoCache, "registry", name), {
      recursive: true,
      dereference: false,
      errorOnExist: true,
    });
  }

  for (const name of forbiddenCargoHomeEntries) {
    if (await pathExists(path.join(cargoCache, name))) {
      throw new Error("trusted Cargo cache must not contain Cargo configuration or credentials");
    }
  }
  await assertRegularTree(cargoCache);
}

async function discoverVisualStudioRoot() {
  const vswhere = await findVswhere();
  const installationPath = (await runCapture(vswhere, [
    "-latest",
    "-products", "*",
    "-requires", "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
    "-property", "installationPath",
  ])).trim();
  if (!installationPath) {
    throw new Error("vswhere did not return a Visual Studio installation with MSVC tools");
  }
  return trustedDirectory(installationPath, false);
}

async function findVswhere() {
  const candidates = [
    path.join(process.env["ProgramFiles(x86)"] ?? "C:\\Program Files (x86)",
      "Microsoft Visual Studio", "Installer", "vswhere.exe"),
    path.join(process.env.ProgramFiles ?? "C:\\Program Files",
      "Microsoft Visual Studio", "Installer", "vswhere.exe"),
  ];
  for (const candidate of candidates) {
    if (await pathExists(candidate)) {
      return resolveRegularFile(candidate, "vswhere", false);
    }
  }
  throw new Error("vswhere.exe was not found on this Windows image");
}

async function discoverWindowsSdkRoot() {
  const candidates = [
    path.join(process.env["ProgramFiles(x86)"] ?? "C:\\Program Files (x86)", "Windows Kits", "10"),
    path.join(process.env.ProgramFiles ?? "C:\\Program Files", "Windows Kits", "10"),
  ];
  for (const candidate of candidates) {
    if (await pathExists(path.join(candidate, "Include"))) {
      return trustedDirectory(candidate, false);
    }
  }
  throw new Error("Windows 10 SDK root was not found on this Windows image");
}

function uniquePathList(values) {
  const seen = new Set();
  const result = [];
  for (const value of values) {
    if (typeof value !== "string" || value.length < 1) continue;
    const key = path.win32.normalize(value).toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    result.push(value);
  }
  if (result.length < 1) throw new Error("toolchain PATH is empty");
  return result.join(path.win32.delimiter);
}

async function canonicalizeExistingToolchainEnvironment(environment, allowNonWindows) {
  const directories = async (values) => Promise.all(
    values.map((value) => trustedDirectory(value, allowNonWindows)),
  );
  return {
    systemRoot: await trustedDirectory(environment.systemRoot, allowNonWindows),
    visualCppInstallRoot: await trustedDirectory(environment.visualCppInstallRoot, allowNonWindows),
    executablePaths: await directories(environment.executablePaths),
    includePaths: await directories(environment.includePaths),
    libraryPaths: await directories(environment.libraryPaths),
    librarySearchPaths: await directories(environment.librarySearchPaths),
  };
}

/**
 * Prefer the toolchain-backed cargo/rustc binary (`rustup which`) over the
 * rustup proxy in ~/.cargo/bin. Isolated CARGO_HOME used for fetch must not
 * break rustup's default-toolchain resolution.
 */
async function resolveRustupTool(name, allowNonWindows) {
  try {
    const rustup = await whichExecutable(process.platform === "win32" ? "rustup.exe" : "rustup");
    const stdout = await runCapture(rustup, ["which", name]);
    const resolved = stdout.trim().split(/\r?\n/).filter(Boolean).at(-1);
    if (resolved && path.isAbsolute(resolved) && await pathExists(resolved)) {
      return resolved;
    }
  } catch {
    // Fall through to PATH lookup for environments without rustup.
  }
  const fileName = process.platform === "win32" ? `${name}.exe` : name;
  return whichExecutable(fileName);
}

async function whichExecutable(fileName) {
  const pathValue = process.env.PATH ?? process.env.Path ?? "";
  const extensions = process.platform === "win32"
    ? (process.env.PATHEXT ?? ".EXE;.CMD;.BAT").split(";").filter(Boolean)
    : [""];
  for (const directory of pathValue.split(path.delimiter).filter(Boolean)) {
    const candidates = [path.join(directory, fileName)];
    for (const extension of extensions) {
      if (!fileName.toLowerCase().endsWith(extension.toLowerCase())) {
        candidates.push(path.join(directory, fileName + extension));
      }
    }
    for (const candidate of candidates) {
      if (await pathExists(candidate)) return candidate;
    }
  }
  throw new Error(`${fileName} was not found on PATH`);
}

async function resolveRegularFile(value, label, allowNonWindows) {
  if (typeof value !== "string" || value.length < 1) {
    throw new Error(`${label} path is required`);
  }
  if (!allowNonWindows && process.platform === "win32" && !validWindowsAbsolutePath(value)) {
    throw new Error(`${label} must be an absolute local Windows path`);
  }
  if (!path.isAbsolute(value)) throw new Error(`${label} path must be absolute`);
  const metadata = await lstat(value);
  const canonical = await realpath(value);
  if (metadata.isSymbolicLink() || !(await stat(canonical)).isFile()) {
    throw new Error(`${label} must identify a regular file`);
  }
  await access(canonical, fsConstants.R_OK);
  return canonical;
}

async function trustedDirectory(value, allowNonWindows) {
  if (typeof value !== "string" || value.length < 1) {
    throw new Error("trusted directory path is required");
  }
  if (!allowNonWindows && process.platform === "win32" && !validWindowsAbsolutePath(value)) {
    throw new Error("trusted directory must be an absolute local Windows path");
  }
  if (!path.isAbsolute(value)) throw new Error("trusted directory must be absolute");
  const metadata = await lstat(value);
  const canonical = await realpath(value);
  if (metadata.isSymbolicLink() || !(await stat(canonical)).isDirectory()) {
    throw new Error("trusted path is not a regular directory");
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

function validWindowsAbsolutePath(value) {
  return typeof value === "string" && value.length > 2 && value.length <= 32_000 &&
    !value.includes("\0") && path.win32.isAbsolute(value) && !value.startsWith("\\\\");
}

function compareDottedVersions(left, right) {
  const leftParts = left.split(".").map(Number);
  const rightParts = right.split(".").map(Number);
  const length = Math.max(leftParts.length, rightParts.length);
  for (let index = 0; index < length; index += 1) {
    const delta = (leftParts[index] ?? 0) - (rightParts[index] ?? 0);
    if (delta !== 0) return delta;
  }
  return 0;
}

async function pathExists(value) {
  try {
    await lstat(value);
    return true;
  } catch (error) {
    if (error?.code === "ENOENT") return false;
    throw error;
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
      else reject(new Error(`command failed: ${path.basename(executable)}`));
    });
  });
}

function runCapture(executable, arguments_) {
  return new Promise((resolve, reject) => {
    const child = spawn(executable, arguments_, {
      shell: false, stdio: ["ignore", "pipe", "pipe"], windowsHide: true,
    });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk) => { stdout += chunk; });
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.once("error", reject);
    child.once("exit", (code, signal) => {
      if (code === 0 && signal === null) resolve(stdout);
      else reject(new Error(`command failed: ${path.basename(executable)}: ${stderr || stdout}`));
    });
  });
}

async function main() {
  const architecture = process.argv.includes("--arch")
    ? process.argv[process.argv.indexOf("--arch") + 1]
    : "x64";
  const skipCargoHydration = process.argv.includes("--skip-cargo-hydration");
  const resolved = await resolveWindowsReleaseToolchain({ architecture, skipCargoHydration });
  const githubEnv = process.env.GITHUB_ENV;
  if (githubEnv) {
    await writeFile(githubEnv, formatGithubEnvAssignments(resolved), { encoding: "utf8", flag: "a" });
  } else if (!skipCargoHydration) {
    throw new Error("GITHUB_ENV is required to export Windows release toolchain paths");
  }
  process.stdout.write([
    `cargo=${resolved.cargoPath}`,
    `rustc=${resolved.rustcPath}`,
    `linker=${resolved.linkerPath}`,
    `cargo_cache=${resolved.cargoCache}`,
  ].join("\n") + "\n");
}

if (process.argv[1] && pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url) {
  main().catch((error) => {
    process.stderr.write(`${error?.message ?? "Windows release toolchain resolution failed"}\n`);
    process.exitCode = 1;
  });
}
