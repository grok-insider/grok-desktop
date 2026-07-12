import assert from "node:assert/strict";
import { chmod, mkdtemp, mkdir, writeFile, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import {
  linuxDaemonCandidates,
  parseLinuxPackageArguments,
  renderLinuxDesktopEntry,
  resolveLinuxDaemonBinary,
  verifyLinuxPackagedLayout,
} from "./package-linux.mjs";

test("parseLinuxPackageArguments defaults arch from host and rejects bad options", () => {
  const parsed = parseLinuxPackageArguments([]);
  assert.ok(parsed.architecture === "x64" || parsed.architecture === "arm64");
  assert.match(parsed.out, /out[/\\]release[/\\]linux/);
  assert.throws(() => parseLinuxPackageArguments(["--arch"]), /option\/value/);
  assert.throws(() => parseLinuxPackageArguments(["--arch", "ppc64"]), /x64 or arm64/);
  assert.throws(() => parseLinuxPackageArguments(["--nope", "1"]), /unsupported/);
});

test("linuxDaemonCandidates only returns host-matching paths", () => {
  const root = "/repo";
  const candidates = linuxDaemonCandidates(root, process.arch === "arm64" ? "arm64" : "x64");
  assert.equal(candidates.length, 2);
  assert.ok(candidates[0].endsWith(path.join("target", "release", "grok-daemon")));
  const otherArch = process.arch === "arm64" ? "x64" : "arm64";
  assert.deepEqual(linuxDaemonCandidates(root, otherArch), []);
});

test("renderLinuxDesktopEntry registers protocol and exec", () => {
  const entry = renderLinuxDesktopEntry({
    name: "Grok Desktop",
    execPath: "/opt/grok/grok-desktop",
    iconPath: "/opt/grok/icon.png",
    version: "0.1.0",
  });
  assert.match(entry, /^\[Desktop Entry\]/m);
  assert.match(entry, /MimeType=x-scheme-handler\/grok-desktop;/);
  assert.match(entry, /Exec=\/opt\/grok\/grok-desktop %u/);
  assert.match(entry, /X-GrokDesktop-Version=0\.1\.0/);
});

test("resolveLinuxDaemonBinary and verifyLinuxPackagedLayout use real files", async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), "grok-linux-pkg-"));
  try {
    const daemon = path.join(root, "grok-daemon");
    await writeFile(daemon, "#!/bin/sh\necho ok\n", { mode: 0o755 });
    const resolved = await resolveLinuxDaemonBinary({
      architecture: process.arch === "arm64" ? "arm64" : "x64",
      daemonBinary: daemon,
    });
    assert.equal(resolved, daemon);

    const appDir = path.join(root, "app");
    await mkdir(path.join(appDir, "resources", "bin"), { recursive: true });
    await writeFile(path.join(appDir, "resources", "bin", "grok-daemon"), "#!/bin/sh\n", {
      mode: 0o755,
    });
    await writeFile(
      path.join(appDir, "grok-desktop.desktop"),
      renderLinuxDesktopEntry({
        name: "Grok Desktop",
        execPath: path.join(appDir, "grok-desktop"),
        iconPath: "icon.png",
        version: "0.1.0",
      }),
    );
    const layout = await verifyLinuxPackagedLayout(appDir);
    assert.ok(layout.daemonPath.endsWith(path.join("resources", "bin", "grok-daemon")));

    await chmod(path.join(appDir, "resources", "bin", "grok-daemon"), 0o644);
    await assert.rejects(() => verifyLinuxPackagedLayout(appDir), /not executable/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
