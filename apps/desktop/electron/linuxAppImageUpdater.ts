import { createHash } from "node:crypto";
import { spawn, type ChildProcess } from "node:child_process";
import { constants as fsConstants } from "node:fs";
import { copyFile, lstat, open, rename, rm, stat } from "node:fs/promises";
import path from "node:path";

const MAX_APPIMAGE_BYTES = 8 * 1024 * 1024 * 1024;

type SpawnProcess = (executable: string, arguments_: string[], options: Parameters<typeof spawn>[2]) => ChildProcess;

export class LinuxAppImageUpdateRunner {
  constructor(
    private readonly helperPath: string,
    private readonly appImagePath: string,
    private readonly spawnProcess: SpawnProcess = spawn,
  ) {}

  async download(expected?: { url?: string; size: number; sha256: string }): Promise<boolean> {
    const backupPath = `${this.appImagePath}.grok-update-backup`;
    const downloadPath = `${this.appImagePath}.grok-update-download`;
    await restoreInterruptedBackup(backupPath, this.appImagePath);
    await assertExecutable(this.helperPath, "AppImage update helper");
    await assertExecutable(this.appImagePath, "running AppImage");
    const before = await fingerprint(this.appImagePath);
    await copyFile(this.appImagePath, backupPath, fsConstants.COPYFILE_EXCL);
    try {
      if (expected?.url) {
        await downloadSignedArtifact(expected, downloadPath);
        await rename(downloadPath, this.appImagePath);
      } else {
        await new Promise<void>((resolve, reject) => {
        const child = this.spawnProcess(
          this.helperPath,
          ["--appimage-extract-and-run", "--overwrite", this.appImagePath],
          {
            cwd: path.dirname(this.appImagePath),
            env: updaterEnvironment(process.env),
            shell: false,
            stdio: "ignore",
          },
        );
        child.once("error", reject);
        child.once("exit", (code, signal) => {
          if (code === 0 && signal === null) resolve();
          else reject(new Error("AppImage update helper failed"));
        });
        });
      }
      await assertExecutable(this.appImagePath, "updated AppImage");
      const after = await fingerprint(this.appImagePath);
      if (expected) {
        const metadata = await stat(this.appImagePath);
        if (metadata.size !== expected.size || after !== expected.sha256) {
          throw new Error("updated AppImage does not match the signed manifest");
        }
      }
      await rm(backupPath);
      return before !== after;
    } catch (error) {
      await rm(downloadPath, { force: true });
      await rename(backupPath, this.appImagePath);
      throw error;
    }
  }
}

async function downloadSignedArtifact(
  expected: { url?: string; size: number; sha256: string },
  destination: string,
): Promise<void> {
  if (!expected.url || !Number.isSafeInteger(expected.size) || expected.size < 1
      || expected.size > MAX_APPIMAGE_BYTES || !/^[a-f0-9]{64}$/.test(expected.sha256)) {
    throw new Error("signed AppImage update metadata is invalid");
  }
  const url = new URL(expected.url);
  if (url.origin !== "https://github.com"
      || !url.pathname.startsWith("/grok-insider/grok-desktop/releases/download/")
      || url.search || url.hash || url.username || url.password) {
    throw new Error("signed AppImage update URL is invalid");
  }
  const response = await fetch(url, { redirect: "follow", signal: AbortSignal.timeout(120_000) });
  if (!response.ok || !response.body) throw new Error("signed AppImage update is unavailable");
  const declared = Number(response.headers.get("content-length"));
  if (Number.isFinite(declared) && declared !== expected.size) {
    throw new Error("signed AppImage update size is invalid");
  }
  const handle = await open(destination, "wx", 0o700);
  const hash = createHash("sha256");
  let written = 0;
  try {
    const reader = response.body.getReader();
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      written += value.byteLength;
      if (written > expected.size) throw new Error("signed AppImage update is too large");
      hash.update(value);
      await handle.write(value);
    }
    if (written !== expected.size || hash.digest("hex") !== expected.sha256) {
      throw new Error("signed AppImage update does not match the manifest");
    }
    await handle.sync();
    await handle.chmod(0o700);
  } finally {
    await handle.close();
  }
}

export function resolveLinuxUpdateRunner(options: {
  packaged: boolean;
  platform: NodeJS.Platform;
  resourcesPath: string;
  appImagePath?: string;
}): LinuxAppImageUpdateRunner | undefined {
  if (!options.packaged || options.platform !== "linux" || !options.appImagePath
      || !path.isAbsolute(options.appImagePath)) return undefined;
  return new LinuxAppImageUpdateRunner(
    path.join(options.resourcesPath, "bin", "appimageupdatetool.AppImage"),
    options.appImagePath,
  );
}

function updaterEnvironment(environment: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  const result: NodeJS.ProcessEnv = { APPIMAGE_EXTRACT_AND_RUN: "1" };
  for (const key of ["HOME", "PATH", "SSL_CERT_DIR", "SSL_CERT_FILE", "TMPDIR", "XDG_CACHE_HOME"]) {
    const value = environment[key];
    if (value) result[key] = value;
  }
  return result;
}

async function assertExecutable(filePath: string, label: string): Promise<void> {
  const metadata = await stat(filePath);
  if (!metadata.isFile() || metadata.size < 1 || metadata.size > MAX_APPIMAGE_BYTES
      || (metadata.mode & 0o111) === 0) throw new Error(`${label} is unavailable`);
}

async function restoreInterruptedBackup(backupPath: string, appImagePath: string): Promise<void> {
  const metadata = await lstat(backupPath).catch((error: NodeJS.ErrnoException) => {
    if (error.code === "ENOENT") return undefined;
    throw error;
  });
  if (!metadata) return;
  if (!metadata.isFile() || metadata.size < 1 || metadata.size > MAX_APPIMAGE_BYTES
      || (metadata.mode & 0o111) === 0) throw new Error("AppImage update backup is invalid");
  await rename(backupPath, appImagePath);
}

async function fingerprint(filePath: string): Promise<string> {
  const handle = await open(filePath, "r");
  try {
    const metadata = await handle.stat();
    if (!metadata.isFile() || metadata.size < 1 || metadata.size > MAX_APPIMAGE_BYTES) {
      throw new Error("AppImage is not a bounded regular file");
    }
    const hash = createHash("sha256");
    const buffer = Buffer.allocUnsafe(1024 * 1024);
    let offset = 0;
    while (offset < metadata.size) {
      const { bytesRead } = await handle.read(buffer, 0, Math.min(buffer.length, metadata.size - offset), offset);
      if (bytesRead === 0) throw new Error("AppImage changed while hashing");
      hash.update(buffer.subarray(0, bytesRead));
      offset += bytesRead;
    }
    const after = await handle.stat();
    if (after.dev !== metadata.dev || after.ino !== metadata.ino || after.size !== metadata.size) {
      throw new Error("AppImage identity changed while hashing");
    }
    return hash.digest("hex");
  } finally {
    await handle.close();
  }
}
