import { spawn } from "node:child_process";
import { constants as fsConstants } from "node:fs";
import { copyFile, mkdir, mkdtemp, readFile, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import {
  inspectPortableExecutable,
  inspectServiceGuestCatalogTrust,
  parseReleaseMetadataKeys,
  windowsServiceBuildMetadata,
} from "./release-utils.mjs";
import {
  createWindowsGoBuildEnvironment,
  windowsGoArchitectures,
} from "./windows-go-build.mjs";

const desktopRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const repositoryRoot = path.resolve(desktopRoot, "../..");
const serviceRoot = path.join(repositoryRoot, "native", "windows-vm-service");

export function parseServiceBuildArguments(arguments_) {
  const values = {};
  for (let index = 0; index < arguments_.length; index += 2) {
    const option = arguments_[index];
    const value = arguments_[index + 1];
    if ((option !== "--arch" && option !== "--out") || value === undefined || values[option]) {
      throw new Error("service build requires unique --arch and --out option/value pairs");
    }
    values[option] = value;
  }
  if (!Object.hasOwn(windowsGoArchitectures, values["--arch"])) throw new Error("--arch must be x64 or arm64");
  if (!values["--out"] || path.basename(values["--out"]).toLowerCase() !== "grok-vm-service.exe") {
    throw new Error("--out must name grok-vm-service.exe");
  }
  return { architecture: values["--arch"], output: path.resolve(values["--out"]) };
}

async function main() {
  if (process.platform !== "win32") throw new Error("release VM services must be built on a trusted Windows worker");
  const options = parseServiceBuildArguments(process.argv.slice(2));
  const goPath = requiredEnvironment("GROK_WINDOWS_GO_PATH", 1024);
  if (!path.win32.isAbsolute(goPath)) throw new Error("GROK_WINDOWS_GO_PATH must be an absolute Windows path");
  const trustedKeys = parseReleaseMetadataKeys(requiredEnvironment("GROK_RELEASE_METADATA_PUBLIC_KEYS_JSON", 65_536));
  const packageMetadata = JSON.parse(await readFile(path.join(desktopRoot, "package.json"), "utf8"));
  const build = windowsServiceBuildMetadata(packageMetadata.version, trustedKeys);
  const temporaryRoot = await mkdtemp(path.join(os.tmpdir(), "grok-vm-service-build-"));
  try {
    const temporaryExecutable = path.join(temporaryRoot, "grok-vm-service.exe");
    const buildEnvironment = createWindowsServiceBuildEnvironment(process.env, options.architecture);
    await run(goPath, [
      "build",
      "-trimpath",
      "-buildvcs=false",
      "-mod=readonly",
      "-ldflags", build.linkerFlags,
      "-o", temporaryExecutable,
      "./cmd/grok-vm-service",
    ], { cwd: serviceRoot, env: buildEnvironment });
    await inspectPortableExecutable(temporaryExecutable, options.architecture);
    await inspectServiceGuestCatalogTrust(temporaryExecutable, trustedKeys);
    await mkdir(path.dirname(options.output), { recursive: true });
    await copyFile(temporaryExecutable, options.output, fsConstants.COPYFILE_EXCL);
  } finally {
    await rm(temporaryRoot, { recursive: true, force: true });
  }
}

export function createWindowsServiceBuildEnvironment(environment, architecture) {
  return createWindowsGoBuildEnvironment(environment, architecture);
}

function run(executable, arguments_, options) {
  return new Promise((resolve, reject) => {
    const child = spawn(executable, arguments_, { ...options, shell: false, stdio: "inherit", windowsHide: true });
    child.once("error", reject);
    child.once("exit", (code, signal) => {
      if (code === 0 && signal === null) resolve();
      else reject(new Error("Go service build failed"));
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
    process.stderr.write("Windows VM service release build failed\n");
    process.exitCode = 1;
  });
}
