/**
 * Development-only resolution of the official Grok Build ACP component.
 *
 * Packaged launches never call this path. The Electron supervisor injects the
 * resulting GROK_ACP_* variables only when allowDevelopmentBinary is true, and
 * the daemon accepts them only when built with debug-acp-descriptor + debug
 * assertions. Production components come from the signed managed catalog.
 */
import { createHash } from "node:crypto";
import { readFileSync, realpathSync, statSync } from "node:fs";
import { spawnSync } from "node:child_process";
import path from "node:path";

const MAX_EXECUTABLE_PATH_BYTES = 4_096;
const MAX_VERSION_BYTES = 128;
const VERSION_OUTPUT_TIMEOUT_MS = 5_000;
const SEMVER_PATTERN = /^(\d+\.\d+\.\d+)(?:[-+][\w.-]+)?$/;
const VERSION_FROM_OUTPUT = /\b(\d+\.\d+\.\d+(?:[-+][\w.-]+)?)\b/;
const SHA256_PATTERN = /^[0-9a-fA-F]{64}$/;

export interface DevelopmentAcpDescriptor {
  executable: string;
  version: string;
  sha256: string;
}

export interface DevelopmentAcpResolveOptions {
  platform: NodeJS.Platform;
  env: NodeJS.ProcessEnv;
  findOnPath?: (name: string, pathEnv: string | undefined) => string | undefined;
  resolveRealPath?: (filePath: string) => string | undefined;
  hashFile?: (filePath: string) => string | undefined;
  readVersion?: (executable: string) => string | undefined;
}

/** Resolves a complete official-component descriptor for development launches. */
export function resolveDevelopmentAcpDescriptor(
  options: DevelopmentAcpResolveOptions,
): DevelopmentAcpDescriptor | undefined {
  const findOnPath = options.findOnPath ?? ((name, pathEnv) => defaultFindOnPath(name, pathEnv, options.platform));
  const resolveRealPath = options.resolveRealPath ?? defaultResolveRealPath;
  const hashFile = options.hashFile ?? defaultHashFile;
  const readVersion = options.readVersion ?? defaultReadVersion;

  const hasExplicitDescriptor = [
    options.env.GROK_ACP_EXECUTABLE,
    options.env.GROK_ACP_VERSION,
    options.env.GROK_ACP_SHA256,
  ].some((value) => value !== undefined);
  const envExecutable = validDevelopmentExecutable(options.env.GROK_ACP_EXECUTABLE, options.platform);
  const envVersion = validDevelopmentVersion(options.env.GROK_ACP_VERSION);
  const envSha = validDevelopmentSha256(options.env.GROK_ACP_SHA256);
  if (hasExplicitDescriptor && (!envExecutable || !envVersion || !envSha)) return undefined;

  const pathExecutable = envExecutable
    ?? findOnPath("grok", options.env.PATH ?? options.env.Path);

  if (!pathExecutable) return undefined;

  const executable = resolveRealPath(pathExecutable);
  if (!executable || !officialGrokBasename(executable, options.platform)) return undefined;

  const version = envVersion ?? normalizeVersion(readVersion(executable));
  if (!version) return undefined;

  const sha256 = envSha ?? hashFile(executable);
  if (!sha256) return undefined;

  // Never pair a caller-supplied digest with a different auto-detected path
  // when only the digest was set; require a coherent triple.
  if (options.env.GROK_ACP_WORKSPACE_ROOTS) return undefined;

  return { executable, version, sha256: sha256.toLowerCase() };
}

/** Applies a resolved descriptor onto a daemon child environment map. */
export function applyDevelopmentAcpDescriptor(
  environment: NodeJS.ProcessEnv,
  descriptor: DevelopmentAcpDescriptor,
): void {
  environment.GROK_ACP_EXECUTABLE = descriptor.executable;
  environment.GROK_ACP_VERSION = descriptor.version;
  environment.GROK_ACP_SHA256 = descriptor.sha256;
}

export function validDevelopmentExecutable(
  value: string | undefined,
  platform: NodeJS.Platform,
): string | undefined {
  if (!value || Buffer.byteLength(value, "utf8") > MAX_EXECUTABLE_PATH_BYTES) return undefined;
  const platformPath = platform === "win32" ? path.win32 : path.posix;
  if (!platformPath.isAbsolute(value)) return undefined;
  if (Array.from(value).some((character) => {
    const point = character.codePointAt(0) ?? 0;
    return point <= 0x1f || (point >= 0x7f && point <= 0x9f);
  })) {
    return undefined;
  }
  return value;
}

export function validDevelopmentVersion(value: string | undefined): string | undefined {
  if (!value || Buffer.byteLength(value, "utf8") > MAX_VERSION_BYTES) return undefined;
  const match = value.trim().match(SEMVER_PATTERN);
  return match?.[1];
}

export function validDevelopmentSha256(value: string | undefined): string | undefined {
  if (!value || !SHA256_PATTERN.test(value)) return undefined;
  return value.toLowerCase();
}

function officialGrokBasename(filePath: string, platform: NodeJS.Platform): boolean {
  const base = (platform === "win32" ? path.win32 : path.posix).basename(filePath);
  return base === "grok" || base === "grok.exe";
}

function normalizeVersion(value: string | undefined): string | undefined {
  if (!value) return undefined;
  const direct = validDevelopmentVersion(value);
  if (direct) return direct;
  const match = value.match(VERSION_FROM_OUTPUT);
  return match ? validDevelopmentVersion(match[1]) : undefined;
}

function defaultFindOnPath(
  name: string,
  pathEnv: string | undefined,
  platform: NodeJS.Platform,
): string | undefined {
  const delimiter = platform === "win32" ? ";" : ":";
  const names = platform === "win32" ? [`${name}.exe`, name] : [name];
  for (const directory of (pathEnv ?? "").split(delimiter)) {
    if (!directory) continue;
    for (const candidateName of names) {
      const candidate = path.join(directory, candidateName);
      try {
        const metadata = statSync(candidate);
        if (metadata.isFile()) return candidate;
      } catch {
        // try next
      }
    }
  }
  return undefined;
}

function defaultResolveRealPath(filePath: string): string | undefined {
  try {
    return realpathSync(filePath);
  } catch {
    return undefined;
  }
}

function defaultHashFile(filePath: string): string | undefined {
  try {
    const bytes = readFileSync(filePath);
    if (bytes.length === 0) return undefined;
    return createHash("sha256").update(bytes).digest("hex");
  } catch {
    return undefined;
  }
}

function defaultReadVersion(executable: string): string | undefined {
  try {
    const result = spawnSync(executable, ["--version"], {
      encoding: "utf8",
      timeout: VERSION_OUTPUT_TIMEOUT_MS,
      windowsHide: true,
      shell: false,
      env: {
        PATH: process.env.PATH,
        Path: process.env.Path,
        SystemRoot: process.env.SystemRoot,
        WINDIR: process.env.WINDIR,
      },
    });
    if (result.error) return undefined;
    return `${result.stdout ?? ""}\n${result.stderr ?? ""}`;
  } catch {
    return undefined;
  }
}
