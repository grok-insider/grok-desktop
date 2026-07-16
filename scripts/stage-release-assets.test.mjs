import assert from "node:assert/strict";
import { mkdtemp, mkdir, readFile, readdir, rm, stat, symlink, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { stageReleaseAssets } from "./stage-release-assets.mjs";

test("flattens only the exact nested platform artifact allowlist", async (t) => {
  const root = await mkdtemp(path.join(os.tmpdir(), "grok-release-stage-test-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const downloads = path.join(root, "downloads");
  const output = path.join(root, "release-assets");
  await createDownloadTree(downloads, "beta");

  await stageReleaseAssets(downloads, output, "beta");

  assert.deepEqual((await readdir(output)).toSorted(), [
    "GrokDesktop-beta-x64.AppImage",
    "GrokDesktop-beta-x64.AppImage.zsync",
    "GrokDesktop-beta-x64.exe",
    "grok-build-linux-x64.json",
    "grok-build-windows-x64.json",
    "linux-package.json",
    "windows-package.json",
  ]);
  assert.equal(await readFile(path.join(output, "GrokDesktop-beta-x64.exe"), "utf8"), "windows-installer");
  assert.equal((await stat(path.join(output, "GrokDesktop-beta-x64.exe"))).mode & 0o777, 0o700);
  assert.equal((await stat(path.join(output, "windows-package.json"))).mode & 0o777, 0o600);
});

test("rejects special filesystem entries before creating the publication directory", {
  skip: process.platform === "win32",
}, async (t) => {
  const root = await mkdtemp(path.join(os.tmpdir(), "grok-release-stage-special-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const downloads = path.join(root, "downloads");
  const output = path.join(root, "release-assets");
  await createDownloadTree(downloads, "beta");
  const socketPath = path.join(downloads, "unexpected.socket");
  const server = createServer();
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(socketPath, resolve);
  });
  try {
    await assert.rejects(stageReleaseAssets(downloads, output, "beta"), /invalid entry/);
    await assert.rejects(stat(output), { code: "ENOENT" });
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
});

test("rejects unexpected files and links before creating the publication directory", async (t) => {
  const root = await mkdtemp(path.join(os.tmpdir(), "grok-release-stage-reject-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const downloads = path.join(root, "downloads");
  const output = path.join(root, "release-assets");
  await createDownloadTree(downloads, "stable");
  await writeFile(path.join(downloads, "unexpected.txt"), "unexpected");
  await assert.rejects(stageReleaseAssets(downloads, output, "stable"), /exact allowlist/);

  await rm(path.join(downloads, "unexpected.txt"));
  const installer = path.join(downloads, "out/release/windows/stable/x64/GrokDesktop-stable-x64.exe");
  await rm(installer);
  await symlink("windows-package.json", installer);
  await assert.rejects(stageReleaseAssets(downloads, output, "stable"), /contains a link/);
  await assert.rejects(stat(output), { code: "ENOENT" });
});

async function createDownloadTree(root, channel) {
  const files = new Map([
    ["apps/desktop/release/components/grok-build/linux-x64.json", "linux-pin"],
    ["apps/desktop/release/components/grok-build/windows-x64.json", "windows-pin"],
    [`out/release/linux/x64/GrokDesktop-${channel}-x64.AppImage`, "linux-image"],
    [`out/release/linux/x64/GrokDesktop-${channel}-x64.AppImage.zsync`, "linux-zsync"],
    ["out/release/linux/x64/linux-package.json", "linux-record"],
    [`out/release/windows/${channel}/x64/GrokDesktop-${channel}-x64.exe`, "windows-installer"],
    [`out/release/windows/${channel}/x64/windows-package.json`, "windows-record"],
  ]);
  for (const [relative, contents] of files) {
    const file = path.join(root, ...relative.split("/"));
    await mkdir(path.dirname(file), { recursive: true });
    await writeFile(file, contents);
  }
}
