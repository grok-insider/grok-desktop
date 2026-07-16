import { createHash, randomUUID } from "node:crypto";
import { spawn, type ChildProcess } from "node:child_process";
import { lstat, mkdir, open, readdir, rename, rm, type FileHandle } from "node:fs/promises";
import path from "node:path";
import type { AuthorizedUpdate } from "./updateManifestVerifier.js";

const MAX_INSTALLER_BYTES = 8 * 1024 * 1024 * 1024;
const MAX_UPDATE_DIRECTORY_ENTRIES = 256;
const MAX_CLEANUP_RETRIES = 3;
const CLEANUP_RETRY_DELAY_MS = 2_000;
const SHA256_PATTERN = /^[a-f0-9]{64}$/;
const CANONICAL_INSTALLER_PATH = /^\/grok-insider\/grok-desktop\/releases\/download\/v(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?\/GrokDesktop-(?:stable|beta)-(?:x64|arm64)\.exe$/;
const OWNED_STAGED_INSTALLER = /^GrokDesktop-update-[a-f0-9-]{16,64}\.exe(?:\.download)?$/i;
const WINDOWS_INSTALLER_ENVIRONMENT_KEYS = [
  "ALLUSERSPROFILE", "APPDATA", "CommonProgramFiles", "CommonProgramFiles(x86)",
  "CommonProgramW6432", "ComSpec", "HOMEDRIVE", "HOMEPATH", "LOCALAPPDATA",
  "NUMBER_OF_PROCESSORS", "OS", "Path", "PATHEXT", "PROCESSOR_ARCHITECTURE",
  "PROCESSOR_IDENTIFIER", "PROCESSOR_LEVEL", "PROCESSOR_REVISION", "ProgramData",
  "ProgramFiles", "ProgramFiles(x86)", "ProgramW6432", "PUBLIC", "SystemDrive",
  "SystemRoot", "TEMP", "TMP", "USERDOMAIN", "USERDOMAIN_ROAMINGPROFILE",
  "USERNAME", "USERPROFILE", "WINDIR",
] as const;

type ExpectedInstaller = AuthorizedUpdate["artifact"];
type SpawnProcess = (executable: string, arguments_: readonly string[], options: {
  detached: true;
  shell: false;
  stdio: "ignore";
  windowsHide: false;
  cwd: string;
  env: NodeJS.ProcessEnv;
}) => ChildProcess;
type FetchResponse = (url: string, init: RequestInit) => Promise<Response>;
type RemoveFile = (filePath: string) => Promise<void>;

export class WindowsNsisUpdateRunner {
  private downloaded: { filePath: string; size: number; sha256: string } | undefined;
  private installStarted = false;
  private cleanupRetryTimer: ReturnType<typeof setTimeout> | undefined;
  private cleanupRetryCount = 0;
  private cleanupInFlight: Promise<number> | undefined;

  constructor(
    private readonly updateDirectory: string,
    private readonly spawnProcess: SpawnProcess = spawn,
    private readonly fetchResponse: FetchResponse = fetch,
    private readonly uniqueId: () => string = randomUUID,
    private readonly environment: NodeJS.ProcessEnv = process.env,
    private readonly removeFile: RemoveFile = (filePath) => rm(filePath),
  ) {}

  async cleanup(): Promise<void> {
    if (this.installStarted) throw new Error("Windows update installation is already starting");
    this.downloaded = undefined;
    const retained = await this.cleanupOwnedInstallers();
    this.scheduleCleanupRetry(retained);
  }

  async download(expected?: ExpectedInstaller): Promise<boolean> {
    validateExpectedInstaller(expected);
    this.cancelCleanupRetry();
    this.downloaded = undefined;
    await this.cleanupOwnedInstallers();

    const identifier = this.uniqueId();
    if (!/^[a-f0-9-]{16,64}$/i.test(identifier)) throw new Error("update staging identity is invalid");
    const destination = path.join(this.updateDirectory, `GrokDesktop-update-${identifier}.exe`);
    const temporary = `${destination}.download`;
    try {
      await downloadAuthorizedInstaller(expected, temporary, this.fetchResponse);
      await rename(temporary, destination);
      const staged = await lstat(destination);
      if (!staged.isFile() || staged.isSymbolicLink() || staged.size !== expected.size) {
        throw new Error("authorized Windows update staging failed");
      }
      this.downloaded = { filePath: destination, size: expected.size, sha256: expected.sha256 };
      return true;
    } catch (error) {
      await rm(temporary, { force: true });
      await rm(destination, { force: true });
      throw error;
    }
  }

  async install(): Promise<void> {
    const downloaded = this.downloaded;
    if (!downloaded) throw new Error("Windows update has not been downloaded");
    if (this.installStarted) throw new Error("Windows update installation is already starting");
    this.cancelCleanupRetry();
    this.installStarted = true;
    let handle: FileHandle | undefined;
    try {
      handle = await openVerifiedInstaller(downloaded);
      await assertInstallerPathIdentity(downloaded, handle);
      await launchInstaller(downloaded.filePath, this.spawnProcess, this.environment);
    } catch (error) {
      this.installStarted = false;
      throw error;
    } finally {
      await handle?.close();
    }
  }

  private cleanupOwnedInstallers(): Promise<number> {
    if (this.cleanupInFlight) return this.cleanupInFlight;
    const cleanup = preparePrivateDirectory(this.updateDirectory, this.removeFile, true);
    this.cleanupInFlight = cleanup;
    void cleanup.finally(() => {
      if (this.cleanupInFlight === cleanup) this.cleanupInFlight = undefined;
    }).catch(() => undefined);
    return cleanup;
  }

  private scheduleCleanupRetry(retained: number): void {
    if (retained === 0) {
      this.cleanupRetryCount = 0;
      return;
    }
    if (this.cleanupRetryTimer || this.cleanupRetryCount >= MAX_CLEANUP_RETRIES) return;
    this.cleanupRetryCount += 1;
    this.cleanupRetryTimer = setTimeout(() => {
      this.cleanupRetryTimer = undefined;
      if (this.installStarted || this.downloaded) return;
      void this.cleanupOwnedInstallers().then(
        (remaining) => this.scheduleCleanupRetry(remaining),
        () => undefined,
      );
    }, CLEANUP_RETRY_DELAY_MS);
    this.cleanupRetryTimer.unref?.();
  }

  private cancelCleanupRetry(): void {
    if (this.cleanupRetryTimer) clearTimeout(this.cleanupRetryTimer);
    this.cleanupRetryTimer = undefined;
    this.cleanupRetryCount = 0;
  }
}

async function assertInstallerPathIdentity(
  expected: { filePath: string; size: number },
  handle: FileHandle,
): Promise<void> {
  const [pathMetadata, handleMetadata] = await Promise.all([
    lstat(expected.filePath),
    handle.stat(),
  ]);
  if (!pathMetadata.isFile() || pathMetadata.isSymbolicLink()
      || pathMetadata.size !== expected.size || handleMetadata.size !== expected.size
      || pathMetadata.dev !== handleMetadata.dev || pathMetadata.ino !== handleMetadata.ino) {
    throw new Error("authorized Windows update changed before installation");
  }
  // Node cannot create a Windows process from an already-open file handle. The
  // verified handle remains open across CreateProcess and the pathname is
  // revalidated immediately before use. ADR 0034 records the same-user boundary:
  // a process running as this user can already replace this per-user app itself.
}

function validateExpectedInstaller(expected: ExpectedInstaller | undefined): asserts expected is ExpectedInstaller {
  if (expected?.kind !== "nsis-installer" || !Number.isSafeInteger(expected.size)
      || expected.size < 1 || expected.size > MAX_INSTALLER_BYTES
      || !SHA256_PATTERN.test(expected.sha256) || typeof expected.url !== "string") {
    throw new Error("authorized Windows update metadata is invalid");
  }
  let url: URL;
  try { url = new URL(expected.url); } catch { throw new Error("authorized Windows update URL is invalid"); }
  if (url.origin !== "https://github.com" || !CANONICAL_INSTALLER_PATH.test(url.pathname)
      || url.search || url.hash || url.username || url.password) {
    throw new Error("authorized Windows update URL is invalid");
  }
}

async function preparePrivateDirectory(
  directory: string,
  removeFile: RemoveFile,
  tolerateSharingViolation: boolean,
): Promise<number> {
  await mkdir(directory, { recursive: true, mode: 0o700 });
  const metadata = await lstat(directory);
  if (!metadata.isDirectory() || metadata.isSymbolicLink()) {
    throw new Error("Windows update directory is unavailable");
  }
  const entries = await readdir(directory, { withFileTypes: true });
  if (entries.length > MAX_UPDATE_DIRECTORY_ENTRIES) {
    throw new Error("Windows update directory contains too many entries");
  }
  let retained = 0;
  for (const entry of entries) {
    if (!OWNED_STAGED_INSTALLER.test(entry.name)) continue;
    const candidate = path.join(directory, entry.name);
    const candidateMetadata = await lstat(candidate);
    if (!entry.isFile() || !candidateMetadata.isFile() || candidateMetadata.isSymbolicLink()) {
      throw new Error("Windows update directory contains an invalid staged installer");
    }
    try {
      await removeFile(candidate);
    } catch (error) {
      if (tolerateSharingViolation && isWindowsSharingViolation(error)) {
        retained += 1;
        continue;
      }
      throw error;
    }
  }
  return retained;
}

function isWindowsSharingViolation(error: unknown): boolean {
  if (!(error instanceof Error)) return false;
  const code = (error as NodeJS.ErrnoException).code;
  return code === "EBUSY" || code === "EPERM";
}

async function downloadAuthorizedInstaller(
  expected: ExpectedInstaller,
  destination: string,
  fetchResponse: FetchResponse,
): Promise<void> {
  const response = await fetchResponse(expected.url, {
    redirect: "follow",
    signal: AbortSignal.timeout(120_000),
  });
  if (!response.ok || !response.body) throw new Error("authorized Windows update is unavailable");
  const declaredHeader = response.headers.get("content-length");
  if (declaredHeader !== null && (!/^\d+$/.test(declaredHeader)
      || Number(declaredHeader) !== expected.size)) {
    throw new Error("authorized Windows update size is invalid");
  }
  const handle = await open(destination, "wx", 0o600);
  const hash = createHash("sha256");
  let written = 0;
  try {
    const reader = response.body.getReader();
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      written += value.byteLength;
      if (written > expected.size) throw new Error("authorized Windows update is too large");
      hash.update(value);
      await writeAll(handle, value);
    }
    if (written !== expected.size || hash.digest("hex") !== expected.sha256) {
      throw new Error("authorized Windows update does not match the manifest");
    }
    await handle.sync();
  } finally {
    await handle.close();
  }
}

async function writeAll(handle: FileHandle, value: Uint8Array): Promise<void> {
  let offset = 0;
  while (offset < value.byteLength) {
    const { bytesWritten } = await handle.write(value, offset, value.byteLength - offset);
    if (bytesWritten < 1) throw new Error("authorized Windows update staging failed");
    offset += bytesWritten;
  }
}

async function openVerifiedInstaller(
  expected: { filePath: string; size: number; sha256: string },
): Promise<FileHandle> {
  const handle = await open(expected.filePath, "r");
  try {
    const [metadata, linkMetadata] = await Promise.all([
      handle.stat(),
      lstat(expected.filePath),
    ]);
    if (!linkMetadata.isFile() || linkMetadata.isSymbolicLink()
        || !metadata.isFile() || metadata.size !== expected.size
        || linkMetadata.size !== expected.size
        || metadata.dev !== linkMetadata.dev || metadata.ino !== linkMetadata.ino) {
      throw new Error("authorized Windows update changed before installation");
    }
    const hash = createHash("sha256");
    const buffer = Buffer.allocUnsafe(1024 * 1024);
    let position = 0;
    while (position < expected.size) {
      const result = await handle.read(buffer, 0, Math.min(buffer.length, expected.size - position), position);
      if (result.bytesRead < 1) throw new Error("authorized Windows update changed before installation");
      hash.update(buffer.subarray(0, result.bytesRead));
      position += result.bytesRead;
    }
    const finalMetadata = await handle.stat();
    if (position !== expected.size || finalMetadata.size !== expected.size
        || hash.digest("hex") !== expected.sha256) {
      throw new Error("authorized Windows update changed before installation");
    }
    return handle;
  } catch (error) {
    await handle.close();
    throw error;
  }
}

async function launchInstaller(
  filePath: string,
  spawnProcess: SpawnProcess,
  environment: NodeJS.ProcessEnv,
): Promise<void> {
  const { cwd, env } = windowsInstallerProcessEnvironment(environment);
  const child = spawnProcess(filePath, ["--updated"], {
    detached: true,
    shell: false,
    stdio: "ignore",
    windowsHide: false,
    cwd,
    env,
  });
  await new Promise<void>((resolve, reject) => {
    child.once("error", reject);
    child.once("spawn", resolve);
  });
  child.unref();
}

function windowsInstallerProcessEnvironment(environment: NodeJS.ProcessEnv): {
  cwd: string;
  env: NodeJS.ProcessEnv;
} {
  const byUppercaseName = new Map(
    Object.entries(environment).map(([name, value]) => [name.toUpperCase(), value]),
  );
  const result: NodeJS.ProcessEnv = {};
  for (const name of WINDOWS_INSTALLER_ENVIRONMENT_KEYS) {
    const value = byUppercaseName.get(name.toUpperCase());
    if (value === undefined) continue;
    if (value.length < 1 || value.length > 32_767 || value.includes("\0")) {
      throw new Error("Windows installer environment is invalid");
    }
    result[name] = value;
  }
  const systemRoot = result.SystemRoot ?? result.WINDIR;
  if (!systemRoot || !path.win32.isAbsolute(systemRoot)) {
    throw new Error("Windows installer environment is unavailable");
  }
  return { cwd: systemRoot, env: result };
}

export async function installWindowsUpdateAfterDaemonStop(options: {
  stopDaemon(): Promise<void>;
  install(): Promise<void>;
  recoverAfterInstallFailure(): void;
  finishShutdown(): void;
}): Promise<void> {
  await options.stopDaemon();
  try {
    await options.install();
  } catch (error) {
    options.recoverAfterInstallFailure();
    throw error;
  }
  options.finishShutdown();
}
