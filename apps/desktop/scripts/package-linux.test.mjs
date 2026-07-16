import assert from "node:assert/strict";
import { chmod, mkdtemp, mkdir, readFile, stat, symlink, writeFile, rm } from "node:fs/promises";
import { createHash, generateKeyPairSync, sign } from "node:crypto";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import {
  linuxDaemonCandidates,
  linuxAppImageUpdateInformation,
  parseLinuxPackageArguments,
  renderLinuxVmServiceEnvironment,
  renderLinuxVmServiceUnit,
  renderLinuxDesktopEntry,
  renderLinuxAppStreamMetadata,
  resolveLinuxDaemonBinary,
  resolveLinuxHostToolsHelper,
  stageLinuxVmServiceBundle,
  stageVerifiedLinuxAcp,
  verifyLinuxPackagedLayout,
} from "./package-linux.mjs";

test("parseLinuxPackageArguments defaults arch from host and rejects bad options", () => {
  const parsed = parseLinuxPackageArguments([]);
  assert.ok(parsed.architecture === "x64" || parsed.architecture === "arm64");
  assert.match(parsed.out, /out[/\\]release[/\\]linux/);
  assert.throws(() => parseLinuxPackageArguments(["--arch"]), /option\/value/);
  assert.throws(() => parseLinuxPackageArguments(["--arch", "ppc64"]), /x64 or arm64/);
  assert.throws(() => parseLinuxPackageArguments(["--nope", "1"]), /unsupported/);
  assert.throws(() => parseLinuxPackageArguments(["--vm-service", "/bin/true"]), /daemon-uid/);
  assert.throws(() => parseLinuxPackageArguments(["--acp-catalog", "/tmp/catalog"]), /requires/);
  assert.throws(() => parseLinuxPackageArguments(["--acp-pinned-manifest", "/tmp/pin"]), /requires/);
  assert.throws(() => parseLinuxPackageArguments([
    "--acp-catalog", "/tmp/catalog", "--acp-trust-file", "/tmp/trust",
    "--acp-pinned-manifest", "/tmp/pin", "--acp-component", "/tmp/grok",
  ]), /mutually exclusive/);
  assert.throws(() => parseLinuxPackageArguments(["--appimagetool", "/tmp/tool"]), /sha256/);
  assert.throws(() => parseLinuxPackageArguments(["--appimageupdatetool", "/tmp/update-tool"]), /sha256/);
  assert.equal(parseLinuxPackageArguments([
    "--appimagetool", "/tmp/tool", "--appimagetool-sha256", "a".repeat(64),
    "--appimageupdatetool", "/tmp/update-tool", "--appimageupdatetool-sha256", "b".repeat(64),
    "--update-trust-file", "/tmp/update-trust.json",
    "--release-date", "2026-07-16",
  ]).appimageupdatetoolSha256, "b".repeat(64));
  assert.throws(() => parseLinuxPackageArguments([
    "--appimagetool", "/tmp/tool", "--appimagetool-sha256", "a".repeat(64),
  ]), /release-date/);
  assert.throws(() => parseLinuxPackageArguments(["--release-date", "2026-02-30"]), /release-date/);
});

test("renders valid release metadata from deterministic inputs", () => {
  const metadata = renderLinuxAppStreamMetadata({ version: "0.0.4", releaseDate: "2026-07-16" });
  assert.match(metadata, /<description>\s*<p>[^<]+<\/p>\s*<\/description>/);
  assert.match(metadata, /<url type="homepage">https:\/\/github\.com\/grok-insider\/grok-desktop<\/url>/);
  assert.match(metadata, /<release version="0\.0\.4" date="2026-07-16" \/>/);
  assert.throws(
    () => renderLinuxAppStreamMetadata({ version: "next", releaseDate: "2026-07-16" }),
    /exact release version/,
  );
  assert.throws(
    () => renderLinuxAppStreamMetadata({ version: "0.0.4", releaseDate: "2026-02-30" }),
    /canonical release date/,
  );
});

test("pins AppImage updates to the canonical stable GitHub release asset", () => {
  assert.equal(
    linuxAppImageUpdateInformation("x64"),
    "gh-releases-zsync|grok-insider|grok-desktop|latest|GrokDesktop-stable-x64.AppImage.zsync",
  );
  assert.equal(
    linuxAppImageUpdateInformation("x64", "beta", "0.0.1"),
    "gh-releases-zsync|grok-insider|grok-desktop|v0.0.1|GrokDesktop-beta-x64.AppImage.zsync",
  );
  assert.throws(() => linuxAppImageUpdateInformation("x64", "beta"), /exact release version/);
  assert.throws(() => linuxAppImageUpdateInformation("ia32"), /architecture/);
});

test("renders a fixed private systemd broker policy with explicit daemon uid", () => {
  const unit = renderLinuxVmServiceUnit({ serviceGroup: "grok-desktop-broker" });
  assert.match(unit, /User=root\nGroup=grok-desktop-broker/);
  assert.match(unit, /RuntimeDirectoryMode=0750/);
  assert.match(unit, /DeviceAllow=\/dev\/kvm rw/);
  assert.match(unit, /ProtectSystem=strict/);
  assert.doesNotMatch(unit, /GROK_LINUX_VM_ALLOWED_UID/);
  assert.equal(renderLinuxVmServiceEnvironment({ daemonUid: 1000 }),
    "GROK_LINUX_VM_ALLOWED_UID=1000\nGROK_LINUX_VM_ALLOWED_DAEMON=/opt/grok-desktop/resources/bin/grok-daemon\n");
  assert.throws(() => renderLinuxVmServiceEnvironment({ daemonUid: -1 }), /invalid/);
});

test("stages byte-identical Linux broker service policy", async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), "grok-linux-service-"));
  try {
    const binary = path.join(root, "grok-linux-vm-service");
    const binaryBytes = elfFixture(process.arch === "arm64" ? "arm64" : "x64");
    await writeFile(binary, binaryBytes, { mode: 0o755 });
    const digest = createHash("sha256").update(binaryBytes).digest("hex");
    const binding = `grok-linux-vm-service-trust-v1:${createHash("sha256").update(digest).digest("hex")}`;
    const daemon = path.join(root, "grok-daemon");
    await writeFile(daemon, `${digest}${binding}`, { mode: 0o755 });
    const out = path.join(root, "out");
    await mkdir(out);
    const result = await stageLinuxVmServiceBundle(out, {
      vmService: binary,
      architecture: process.arch === "arm64" ? "arm64" : "x64",
      daemonUid: 1000,
      serviceGroup: "grok-desktop-broker",
    }, daemon);
    assert.deepEqual(await readFile(result.binary), await readFile(binary));
    assert.equal((await stat(result.environment)).mode & 0o777, 0o640);
    assert.match(await readFile(result.unit, "utf8"), /EnvironmentFile=\/etc\/grok-desktop/);
    const linked = path.join(root, "linked-service");
    await symlink(binary, linked);
    await assert.rejects(() => stageLinuxVmServiceBundle(path.join(root, "linked-out"), {
      vmService: linked,
      architecture: process.arch === "arm64" ? "arm64" : "x64",
      daemonUid: 1000,
      serviceGroup: "grok-desktop-broker",
    }, daemon), /ELOOP|symbolic|regular/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("stages only a signed Linux ACP component and preserves vendor bytes", async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), "grok-linux-acp-"));
  try {
    const architecture = process.arch === "arm64" ? "arm64" : "x64";
    const component = path.join(root, "grok");
    const componentBytes = elfFixture(architecture);
    await writeFile(component, componentBytes, { mode: 0o755 });
    const { publicKey, privateKey } = generateKeyPairSync("ed25519");
    const rawKey = Buffer.from(publicKey.export({ format: "jwk" }).x, "base64url");
    const keyID = "linux-test";
    const trustRaw = `${keyID}=${rawKey.toString("hex")}`;
    const trustBinding = `grok-acp-catalog-trust-v1:${createHash("sha256").update(trustRaw).digest("hex")}`;
    const trustFile = path.join(root, "trust.txt");
    await writeFile(trustFile, `${trustRaw}\n`, { mode: 0o600 });
    const payload = Buffer.from(JSON.stringify({
      schema: "grok.official-component-catalog/v1",
      sequence: 1,
      expiresAtUnixSeconds: 2_000_000_000,
      components: [{
        name: "grok-build", publisher: "xAI", version: "1.2.3", os: "linux",
        architecture: architecture === "x64" ? "x86_64" : "aarch64",
        executable: "bin/grok",
        sha256: createHash("sha256").update(componentBytes).digest("hex"),
        size: componentBytes.length,
      }],
    }));
    const keyLength = Buffer.alloc(2);
    keyLength.writeUInt16BE(Buffer.byteLength(keyID));
    const signingBytes = Buffer.concat([
      Buffer.from("grok.desktop.official-component-catalog.v1\0"), keyLength,
      Buffer.from(keyID), payload,
    ]);
    const catalog = path.join(root, "catalog.json");
    await writeFile(catalog, JSON.stringify({
      schema: "grok.official-component-catalog-envelope/v1",
      keyId: keyID,
      payload: payload.toString("base64"),
      signature: sign(null, signingBytes, privateKey).toString("base64"),
    }));
    const daemon = path.join(root, "grok-daemon");
    await writeFile(daemon, Buffer.concat([Buffer.from(trustRaw), Buffer.from(trustBinding)]), { mode: 0o755 });
    const resources = path.join(root, "resources-bin");
    await mkdir(resources);
    const staged = await stageVerifiedLinuxAcp(resources, {
      architecture, acpCatalog: catalog, acpComponent: component, acpTrustFile: trustFile,
    }, daemon, 1_900_000_000);
    assert.deepEqual(await readFile(staged.component), componentBytes);
    await writeFile(component, Buffer.concat([componentBytes, Buffer.from("tamper")]), { mode: 0o755 });
    await assert.rejects(() => stageVerifiedLinuxAcp(path.join(root, "second"), {
      architecture, acpCatalog: catalog, acpComponent: component, acpTrustFile: trustFile,
    }, daemon, 1_900_000_000), /changed during staging|does not match/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("stages an exact source-pinned Linux ACP component", async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), "grok-linux-pinned-acp-"));
  try {
    const architecture = process.arch === "arm64" ? "arm64" : "x64";
    if (architecture !== "x64") return;
    const component = path.join(root, "grok");
    const componentBytes = elfFixture(architecture);
    await writeFile(component, componentBytes, { mode: 0o755 });
    const manifestValue = {
      schema: "grok.official-component-pin/v1", name: "grok-build", publisher: "xAI",
      version: "0.2.99", os: "linux", architecture: "x86_64", executable: "bin/grok",
      sourceUrl: "https://x.ai/cli/grok-0.2.99-linux-x86_64",
      sha256: createHash("sha256").update(componentBytes).digest("hex"), size: componentBytes.length,
    };
    const manifestBytes = Buffer.from(`${JSON.stringify(manifestValue)}\n`);
    const manifest = path.join(root, "linux-x64.json");
    await writeFile(manifest, manifestBytes);
    const binding = `grok-acp-pinned-manifest-v1:${createHash("sha256").update(manifestBytes).digest("hex")}`;
    const daemon = path.join(root, "grok-daemon");
    await writeFile(daemon, binding, { mode: 0o755 });
    const resources = path.join(root, "resources-bin");
    await mkdir(resources);
    const staged = await stageVerifiedLinuxAcp(resources, {
      architecture, acpPinnedManifest: manifest, acpComponent: component,
    }, daemon, 1_900_000_000);
    assert.deepEqual(await readFile(staged.component), componentBytes);
    assert.deepEqual(await readFile(staged.manifest), manifestBytes);
    assert.equal(staged.trustBinding, binding);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

function elfFixture(architecture) {
  const bytes = Buffer.alloc(64);
  bytes.set([0x7f, 0x45, 0x4c, 0x46, 2, 1], 0);
  bytes.writeUInt16LE(architecture === "x64" ? 62 : 183, 18);
  return bytes;
}

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

test("native helper resolution and verifyLinuxPackagedLayout use real files", async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), "grok-linux-pkg-"));
  try {
    const daemon = path.join(root, "grok-daemon");
    await writeFile(daemon, "#!/bin/sh\necho ok\n", { mode: 0o755 });
    const resolved = await resolveLinuxDaemonBinary({
      architecture: process.arch === "arm64" ? "arm64" : "x64",
      daemonBinary: daemon,
    });
    assert.equal(resolved, daemon);
    const helper = path.join(root, "grok-host-tools-mcp");
    await writeFile(helper, "#!/bin/sh\necho ok\n", { mode: 0o755 });
    const resolvedHelper = await resolveLinuxHostToolsHelper({
      architecture: process.arch === "arm64" ? "arm64" : "x64",
      hostToolsHelper: helper,
    });
    assert.equal(resolvedHelper, helper);

    const appDir = path.join(root, "app");
    await mkdir(path.join(appDir, "resources", "bin"), { recursive: true });
    await writeFile(path.join(appDir, "resources", "bin", "grok-daemon"), "#!/bin/sh\n", {
      mode: 0o755,
    });
    await writeFile(
      path.join(appDir, "resources", "bin", "grok-host-tools-mcp"),
      "#!/bin/sh\n",
      { mode: 0o755 },
    );
    await writeFile(
      path.join(appDir, "resources", "bin", "appimageupdatetool.AppImage"),
      "#!/bin/sh\n",
      { mode: 0o755 },
    );
    await writeFile(path.join(appDir, "resources", "update-trusted-keys.json"), "{}\n");
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
