import { createHash } from "node:crypto";
import { spawn, type ChildProcess } from "node:child_process";
import { mkdir, open, readFile, rename, rm } from "node:fs/promises";
import path from "node:path";

const MAX_MSIX_BYTES = 8 * 1024 * 1024 * 1024;
const THUMBPRINT_PATTERN = /^[A-F0-9]{40}$/;

type SpawnProcess = (executable: string, arguments_: string[], options: Parameters<typeof spawn>[2]) => ChildProcess;

export interface WindowsUpdateTrust {
  packageIdentity: string;
  publisher: string;
  signerThumbprint: string;
}

export class WindowsMsixUpdateRunner {
  private downloadedPath = "";

  constructor(
    private readonly updateDirectory: string,
    private readonly trust: WindowsUpdateTrust,
    private readonly openPath: (filePath: string) => Promise<string>,
    private readonly powershellPath = path.join(process.env.SystemRoot ?? "C:\\Windows", "System32", "WindowsPowerShell", "v1.0", "powershell.exe"),
    private readonly spawnProcess: SpawnProcess = spawn,
  ) {}

  async download(expected?: { url?: string; size: number; sha256: string }): Promise<boolean> {
    validateTrust(this.trust);
    if (!expected?.url || !Number.isSafeInteger(expected.size) || expected.size < 1
        || expected.size > MAX_MSIX_BYTES || !/^[a-f0-9]{64}$/.test(expected.sha256)) {
      throw new Error("signed MSIX update metadata is invalid");
    }
    const url = new URL(expected.url);
    if (url.origin !== "https://github.com"
        || !url.pathname.startsWith("/grok-insider/grok-desktop/releases/download/")
        || url.pathname.split("/").at(-1)?.endsWith(".msix") !== true
        || url.search || url.hash || url.username || url.password) {
      throw new Error("signed MSIX update URL is invalid");
    }
    const destination = path.join(this.updateDirectory, "GrokDesktop-update.msix");
    const temporary = `${destination}.download`;
    await mkdir(this.updateDirectory, { recursive: true, mode: 0o700 });
    await rm(temporary, { force: true });
    await rm(destination, { force: true });
    try {
      await downloadSignedArtifact({ url: expected.url, size: expected.size, sha256: expected.sha256 }, temporary);
      await verifyMsix(temporary, this.trust, this.powershellPath, this.spawnProcess);
      await rename(temporary, destination);
      this.downloadedPath = destination;
      return true;
    } catch (error) {
      await rm(temporary, { force: true });
      throw error;
    }
  }

  async install(): Promise<void> {
    if (!this.downloadedPath) throw new Error("MSIX update has not been downloaded");
    const result = await this.openPath(this.downloadedPath);
    if (result) throw new Error("Windows could not open the signed MSIX installer");
  }
}

export async function loadWindowsUpdateTrust(filePath: string): Promise<WindowsUpdateTrust> {
  const raw = await readFile(filePath);
  if (raw.byteLength < 1 || raw.byteLength > 4096) throw new Error("Windows update trust is unavailable");
  let value: unknown;
  try { value = JSON.parse(raw.toString("utf8")); } catch { throw new Error("Windows update trust is invalid"); }
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Windows update trust is invalid");
  const record = value as Record<string, unknown>;
  const keys = Object.keys(record);
  if (keys.length !== 3 || !keys.includes("packageIdentity") || !keys.includes("publisher")
      || !keys.includes("signerThumbprint")
      || typeof record.packageIdentity !== "string" || typeof record.publisher !== "string"
      || typeof record.signerThumbprint !== "string") throw new Error("Windows update trust is invalid");
  const trust = {
    packageIdentity: record.packageIdentity,
    publisher: record.publisher,
    signerThumbprint: record.signerThumbprint.toUpperCase(),
  };
  validateTrust(trust);
  return trust;
}

async function downloadSignedArtifact(
  expected: { url: string; size: number; sha256: string },
  destination: string,
): Promise<void> {
  const response = await fetch(expected.url, { redirect: "follow", signal: AbortSignal.timeout(120_000) });
  if (!response.ok || !response.body) throw new Error("signed MSIX update is unavailable");
  const declared = Number(response.headers.get("content-length"));
  if (Number.isFinite(declared) && declared !== expected.size) throw new Error("signed MSIX update size is invalid");
  const handle = await open(destination, "wx", 0o600);
  const hash = createHash("sha256");
  let written = 0;
  try {
    const reader = response.body.getReader();
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      written += value.byteLength;
      if (written > expected.size) throw new Error("signed MSIX update is too large");
      hash.update(value);
      await handle.write(value);
    }
    if (written !== expected.size || hash.digest("hex") !== expected.sha256) {
      throw new Error("signed MSIX update does not match the manifest");
    }
    await handle.sync();
  } finally {
    await handle.close();
  }
}

async function verifyMsix(
  filePath: string,
  trust: WindowsUpdateTrust,
  powershellPath: string,
  spawnProcess: SpawnProcess,
): Promise<void> {
  const script = [
    "$s=Get-AuthenticodeSignature -LiteralPath $args[0]",
    "if($s.Status -ne 'Valid' -or $s.SignerCertificate.Thumbprint -ne $args[1]){exit 9}",
    "Add-Type -AssemblyName WindowsBase",
    "$p=[IO.Packaging.Package]::Open($args[0],[IO.FileMode]::Open,[IO.FileAccess]::Read)",
    "try{$part=$p.GetPart([Uri]::new('/AppxManifest.xml',[UriKind]::Relative));$r=[IO.StreamReader]::new($part.GetStream());try{[xml]$m=$r.ReadToEnd()}finally{$r.Dispose()}}finally{$p.Close()}",
    "$i=$m.Package.Identity",
    "if($i.Name -ne $args[2] -or $i.Publisher -ne $args[3]){exit 10}",
  ].join(";");
  await new Promise<void>((resolve, reject) => {
    const child = spawnProcess(powershellPath, [
      "-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "AllSigned",
      "-Command", script, filePath, trust.signerThumbprint, trust.packageIdentity, trust.publisher,
    ], { shell: false, windowsHide: true, stdio: "ignore" });
    child.once("error", reject);
    child.once("exit", (code, signal) => {
      if (code === 0 && signal === null) resolve();
      else reject(new Error("MSIX signer verification failed"));
    });
  });
}

function validateTrust(trust: WindowsUpdateTrust): void {
  if (!/^[A-Za-z0-9.-]{3,50}$/.test(trust.packageIdentity)
      || !trust.publisher.startsWith("CN=") || trust.publisher.length > 512
      || !THUMBPRINT_PATTERN.test(trust.signerThumbprint)) {
    throw new Error("Windows update trust is invalid");
  }
}
