import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, rm, stat, unlink, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { FuseState } from "@electron/fuses";
import { ELECTRON_FUSE_POLICY } from "./electron-fuse-policy.mjs";
import {
  parseLinuxArtifactVerifierArguments,
  verifyLinuxReleaseArtifact,
} from "./verify-linux-release-artifact.mjs";

const expectedFuses = Object.freeze({
  RunAsNode: false,
  EnableCookieEncryption: true,
  EnableNodeOptionsEnvironmentVariable: false,
  EnableNodeCliInspectArguments: false,
  EnableEmbeddedAsarIntegrityValidation: true,
  OnlyLoadAppFromAsar: true,
  LoadBrowserProcessSpecificV8Snapshot: false,
  GrantFileProtocolExtraPrivileges: false,
  WasmTrapHandlers: true,
});

function sha256(contents) {
  return createHash("sha256").update(contents).digest("hex");
}

function sha1(contents) {
  return createHash("sha1").update(contents).digest("hex");
}

function staticElfFixture(architecture = "x64", suffix = "") {
  const bytes = Buffer.alloc(64 + 56 + Buffer.byteLength(suffix));
  bytes.set([0x7f, 0x45, 0x4c, 0x46, 2, 1, 1], 0);
  bytes.writeUInt16LE(2, 16);
  bytes.writeUInt16LE(architecture === "x64" ? 62 : 183, 18);
  bytes.writeUInt32LE(1, 20);
  bytes.writeBigUInt64LE(0x40_0040n, 24);
  bytes.writeBigUInt64LE(64n, 32);
  bytes.writeUInt16LE(64, 52);
  bytes.writeUInt16LE(56, 54);
  bytes.writeUInt16LE(1, 56);
  bytes.writeUInt32LE(1, 64);
  bytes.writeUInt32LE(5, 68);
  bytes.writeBigUInt64LE(0n, 72);
  bytes.writeBigUInt64LE(0x40_0000n, 80);
  bytes.writeBigUInt64LE(0x40_0000n, 88);
  bytes.writeBigUInt64LE(BigInt(bytes.length), 96);
  bytes.writeBigUInt64LE(BigInt(bytes.length), 104);
  bytes.writeBigUInt64LE(0x1000n, 112);
  bytes.write(suffix, 120);
  return bytes;
}

function hardenedFuseWire() {
  const state = [];
  for (const { option, enabled } of ELECTRON_FUSE_POLICY) {
    state[option] = enabled ? FuseState.ENABLE : FuseState.DISABLE;
  }
  return state;
}

async function createFixture(t, options = {}) {
  const root = await mkdtemp(path.join(os.tmpdir(), "grok-linux-artifact-test-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const appImagePath = path.join(root, "GrokDesktop-beta-x64.AppImage");
  const zsyncPath = `${appImagePath}.zsync`;
  const recordPath = path.join(root, "linux-package.json");
  const appImageBytes = Buffer.from("fixture AppImage bytes");
  const daemonBytes = staticElfFixture("x64", "fixture daemon bytes");
  const hostToolsHelperBytes = staticElfFixture("x64", "fixture Host Tools helper bytes");
  const updateToolBytes = Buffer.from("fixture update tool bytes");
  const componentBytes = Buffer.from("fixture official Grok component bytes");
  await writeFile(appImagePath, appImageBytes, { mode: 0o755 });
  const zsyncBytes = options.zsyncBytes ?? Buffer.concat([
    Buffer.from(
      `zsync: 0.6.2\nFilename: ${path.basename(appImagePath)}\n`
      + `MTime: Thu, 16 Jul 2026 16:15:45 +0000\nBlocksize: 2048\n`
      + `Length: ${appImageBytes.length}\nHash-Lengths: 1,2,4\n`
      + `URL: ${path.basename(appImagePath)}\nSHA-1: ${sha1(appImageBytes)}\n\n`,
    ),
    Buffer.from("fixture rolling checksums"),
  ]);
  await writeFile(zsyncPath, zsyncBytes, { mode: 0o644 });

  let acp = { staged: false };
  let acpPinPath;
  let pinBytes;
  if (options.withAcp) {
    const pin = {
      schema: "grok.official-component-pin/v1",
      name: "grok-build",
      publisher: "xAI",
      version: "0.2.99",
      os: "linux",
      architecture: "x86_64",
      executable: "bin/grok",
      sourceUrl: "https://x.ai/cli/grok-0.2.99-linux-x86_64",
      sha256: sha256(componentBytes),
      size: componentBytes.length,
    };
    pinBytes = Buffer.from(`${JSON.stringify(pin)}\n`);
    acpPinPath = path.join(root, "linux-x64.json");
    await writeFile(acpPinPath, options.externalPinBytes ?? pinBytes);
    acp = {
      staged: true,
      version: pin.version,
      sha256: pin.sha256,
      trustBinding: `grok-acp-pinned-manifest-v1:${sha256(pinBytes)}`,
      sourceUrl: pin.sourceUrl,
    };
  }

  const record = {
    schemaVersion: 2,
    product: "grok-desktop",
    platform: "linux",
    version: "0.0.10",
    architecture: "x64",
    appDirectory: "/build/Grok Desktop-linux-x64",
    executable: "/build/Grok Desktop-linux-x64/grok-desktop",
    appImage: "/build/GrokDesktop-beta-x64.AppImage",
    fuses: { ...expectedFuses },
    appImageSha256: sha256(appImageBytes),
    zsync: {
      filename: path.basename(zsyncPath),
      size: zsyncBytes.length,
      sha256: sha256(zsyncBytes),
    },
    updateToolSha256: sha256(updateToolBytes),
    daemonSha256: sha256(daemonBytes),
    hostToolsHelperSha256: sha256(hostToolsHelperBytes),
    daemonSource: "/build/target/release/grok-daemon",
    acp,
    vmService: { staged: false },
    isolation: "not_embedded",
    notes: "Fixture Limited Mode package.",
    builtAtUnixMs: 1_800_000_000_000,
    host: { platform: "linux", arch: "x64", release: "test-kernel" },
  };
  Object.assign(record, options.recordOverrides);
  await writeFile(recordPath, `${JSON.stringify(record, null, 2)}\n`);

  let extractionDirectory;
  let extractedElectron;
  const operations = {
    extractAppImage: async ({ extractionDirectory: destination }) => {
      extractionDirectory = destination;
      const binRoot = path.join(destination, "squashfs-root", "usr", "bin");
      await mkdir(path.join(binRoot, "resources", "bin"), { recursive: true });
      extractedElectron = path.join(binRoot, "grok-desktop");
      await writeFile(extractedElectron, Buffer.from("fixture Electron bytes"), { mode: 0o755 });
      await writeFile(
        path.join(binRoot, "resources", "bin", "grok-daemon"),
        options.embeddedDaemonBytes ?? daemonBytes,
        { mode: 0o755 },
      );
      await writeFile(
        path.join(binRoot, "resources", "bin", "grok-host-tools-mcp"),
        options.embeddedHostToolsHelperBytes ?? hostToolsHelperBytes,
        { mode: 0o755 },
      );
      await writeFile(
        path.join(binRoot, "resources", "bin", "appimageupdatetool.AppImage"),
        options.embeddedUpdateToolBytes ?? updateToolBytes,
        { mode: 0o755 },
      );
      if (options.withAcp) {
        const componentRoot = path.join(binRoot, "resources", "bin", "components", "grok-acp");
        await mkdir(path.join(componentRoot, "bin"), { recursive: true });
        await writeFile(
          path.join(componentRoot, "bin", "grok"),
          options.embeddedComponentBytes ?? componentBytes,
          { mode: 0o755 },
        );
        await writeFile(
          path.join(componentRoot, "pinned-component.json"),
          options.embeddedPinBytes ?? pinBytes,
        );
      }
      return path.join(destination, "squashfs-root");
    },
    readFuseWire: async (executable) => {
      assert.equal(executable, extractedElectron);
      return options.fuseWire ?? hardenedFuseWire();
    },
  };
  return {
    appImagePath,
    zsyncPath,
    recordPath,
    acpPinPath,
    hostToolsHelperSha256: sha256(hostToolsHelperBytes),
    operations,
    get extractionDirectory() { return extractionDirectory; },
  };
}

test("verifies exact AppImage, fuse, native helper, and pinned ACP evidence", async (t) => {
  const fixture = await createFixture(t, { withAcp: true });
  const result = await verifyLinuxReleaseArtifact({
    appImagePath: fixture.appImagePath,
    recordPath: fixture.recordPath,
    acpPinPath: fixture.acpPinPath,
  }, fixture.operations);

  assert.equal(result.appImage, "GrokDesktop-beta-x64.AppImage");
  assert.deepEqual(result.fuses, expectedFuses);
  assert.equal(result.hostToolsHelperSha256, fixture.hostToolsHelperSha256);
  assert.equal(result.zsync.filename, "GrokDesktop-beta-x64.AppImage.zsync");
  assert.deepEqual(result.acp, {
    staged: true,
    kind: "pinned",
    sha256: result.acp.sha256,
    version: "0.2.99",
  });
  await assert.rejects(stat(fixture.extractionDirectory), { code: "ENOENT" });
});

test("rejects AppImage bytes that differ from the package record before extraction", async (t) => {
  const fixture = await createFixture(t);
  await writeFile(fixture.appImagePath, Buffer.from("different AppImage bytes"), { mode: 0o755 });
  let extracted = false;
  await assert.rejects(
    verifyLinuxReleaseArtifact({
      appImagePath: fixture.appImagePath,
      recordPath: fixture.recordPath,
    }, {
      ...fixture.operations,
      extractAppImage: async () => { extracted = true; },
    }),
    /AppImage differs from Linux package metadata/,
  );
  assert.equal(extracted, false);
});

test("rejects missing, modified, or malformed zsync metadata before extraction", async (t) => {
  await t.test("missing file", async (subtest) => {
    const fixture = await createFixture(subtest);
    await unlink(fixture.zsyncPath);
    await assert.rejects(
      verifyLinuxReleaseArtifact({
        appImagePath: fixture.appImagePath,
        recordPath: fixture.recordPath,
      }, fixture.operations),
      /AppImage zsync metadata is not a bounded regular file/,
    );
  });
  await t.test("modified bytes", async (subtest) => {
    const fixture = await createFixture(subtest);
    await writeFile(fixture.zsyncPath, Buffer.from("modified zsync bytes"));
    await assert.rejects(
      verifyLinuxReleaseArtifact({
        appImagePath: fixture.appImagePath,
        recordPath: fixture.recordPath,
      }, fixture.operations),
      /zsync bytes differ from Linux package metadata/,
    );
  });
  await t.test("malformed header", async (subtest) => {
    const fixture = await createFixture(subtest, {
      zsyncBytes: Buffer.from("not a bounded zsync header"),
    });
    await assert.rejects(
      verifyLinuxReleaseArtifact({
        appImagePath: fixture.appImagePath,
        recordPath: fixture.recordPath,
      }, fixture.operations),
      /zsync header is missing or unbounded/,
    );
  });
});

test("rejects an extracted Electron executable whose raw fuse wire is not hardened", async (t) => {
  const wire = hardenedFuseWire();
  wire[ELECTRON_FUSE_POLICY[0].option] = FuseState.ENABLE;
  const fixture = await createFixture(t, { fuseWire: wire });
  await assert.rejects(
    verifyLinuxReleaseArtifact({
      appImagePath: fixture.appImagePath,
      recordPath: fixture.recordPath,
    }, fixture.operations),
    /fuse verification failed for RunAsNode/,
  );
});

test("rejects mismatched embedded daemon, update tool, and ACP component bytes", async (t) => {
  for (const [name, options, pattern] of [
    ["daemon", { embeddedDaemonBytes: staticElfFixture("x64", "tampered daemon") }, /embedded daemon differs/],
    [
      "Host Tools helper",
      { embeddedHostToolsHelperBytes: staticElfFixture("x64", "tampered Host Tools helper") },
      /embedded Host Tools helper differs/,
    ],
    ["update tool", { embeddedUpdateToolBytes: Buffer.from("tampered updater") }, /embedded update tool differs/],
    ["ACP component", { withAcp: true, embeddedComponentBytes: Buffer.from("tampered ACP") }, /embedded ACP component differs/],
  ]) {
    await t.test(name, async (subtest) => {
      const fixture = await createFixture(subtest, options);
      await assert.rejects(
        verifyLinuxReleaseArtifact({
          appImagePath: fixture.appImagePath,
          recordPath: fixture.recordPath,
          acpPinPath: fixture.acpPinPath,
        }, fixture.operations),
        pattern,
      );
    });
  }
});

test("requires schema v2 fuse attestation and explicit verifier inputs", async (t) => {
  const fixture = await createFixture(t, { recordOverrides: { schemaVersion: 1 } });
  await assert.rejects(
    verifyLinuxReleaseArtifact({
      appImagePath: fixture.appImagePath,
      recordPath: fixture.recordPath,
    }, fixture.operations),
    /record identity is unsupported/,
  );
  assert.throws(() => parseLinuxArtifactVerifierArguments([]), /are required/);
  assert.throws(
    () => parseLinuxArtifactVerifierArguments(["--appimage", "a", "--appimage", "b", "--record", "c"]),
    /was repeated/,
  );
  const parsed = parseLinuxArtifactVerifierArguments([
    "--appimage", fixture.appImagePath,
    "--record", fixture.recordPath,
  ]);
  assert.equal(parsed.appImagePath, fixture.appImagePath);
  assert.equal(parsed.recordPath, fixture.recordPath);
});
