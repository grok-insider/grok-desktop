import { createHash } from "node:crypto";
import { spawn, type ChildProcess } from "node:child_process";
import { open, stat } from "node:fs/promises";
import path from "node:path";

const MAX_APPIMAGE_BYTES = 8 * 1024 * 1024 * 1024;

type SpawnProcess = (executable: string, arguments_: string[], options: Parameters<typeof spawn>[2]) => ChildProcess;

export class LinuxAppImageUpdateRunner {
  constructor(
    private readonly helperPath: string,
    private readonly appImagePath: string,
    private readonly spawnProcess: SpawnProcess = spawn,
  ) {}

  async download(): Promise<boolean> {
    await assertExecutable(this.helperPath, "AppImage update helper");
    await assertExecutable(this.appImagePath, "running AppImage");
    const before = await fingerprint(this.appImagePath);
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
    await assertExecutable(this.appImagePath, "updated AppImage");
    return before !== await fingerprint(this.appImagePath);
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
