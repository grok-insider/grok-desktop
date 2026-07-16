import assert from "node:assert/strict";
import test from "node:test";
import { FuseState, FuseV1Options, FuseVersion } from "@electron/fuses";
import {
  ELECTRON_FUSE_POLICY,
  assertHardenedElectronFuseState,
  electronFuseConfiguration,
  hardenElectronExecutable,
  readVerifiedFuseState,
  readableElectronFuseState,
  readableFuseState,
} from "./electron-fuse-policy.mjs";
import {
  hardenElectronExecutable as windowsHardenElectronExecutable,
  readVerifiedFuseState as windowsReadVerifiedFuseState,
  readableFuseState as windowsReadableFuseState,
} from "./package-windows.mjs";

function hardenedFuseWire() {
  const state = [];
  for (const { option, enabled } of ELECTRON_FUSE_POLICY) {
    state[option] = enabled ? FuseState.ENABLE : FuseState.DISABLE;
  }
  return state;
}

test("defines the complete cross-platform Electron fuse policy", () => {
  assert.deepEqual(electronFuseConfiguration(), {
    version: FuseVersion.V1,
    strictlyRequireAllFuses: true,
    [FuseV1Options.RunAsNode]: false,
    [FuseV1Options.EnableCookieEncryption]: true,
    [FuseV1Options.EnableNodeOptionsEnvironmentVariable]: false,
    [FuseV1Options.EnableNodeCliInspectArguments]: false,
    [FuseV1Options.EnableEmbeddedAsarIntegrityValidation]: true,
    [FuseV1Options.OnlyLoadAppFromAsar]: true,
    [FuseV1Options.LoadBrowserProcessSpecificV8Snapshot]: false,
    [FuseV1Options.GrantFileProtocolExtraPrivileges]: false,
    [FuseV1Options.WasmTrapHandlers]: true,
  });
});

test("hardens an Electron executable and verifies the resulting wire", async () => {
  const calls = [];
  await hardenElectronExecutable("/tmp/Grok Desktop", {
    flipFuses: async (executable, configuration) => {
      calls.push({ executable, configuration });
    },
    getCurrentFuseWire: async (executable) => {
      calls.push({ executable });
      return hardenedFuseWire();
    },
  });

  assert.deepEqual(calls, [
    { executable: "/tmp/Grok Desktop", configuration: electronFuseConfiguration() },
    { executable: "/tmp/Grok Desktop" },
  ]);
});

test("rejects a packaged executable whose fuse state differs from policy", () => {
  const state = hardenedFuseWire();
  state[FuseV1Options.RunAsNode] = FuseState.ENABLE;
  assert.throws(
    () => assertHardenedElectronFuseState(state),
    /packaged Electron fuse verification failed for RunAsNode/,
  );
});

test("records the verified fuse wire with stable readable names", async () => {
  const state = hardenedFuseWire();
  const expected = {
    RunAsNode: false,
    EnableCookieEncryption: true,
    EnableNodeOptionsEnvironmentVariable: false,
    EnableNodeCliInspectArguments: false,
    EnableEmbeddedAsarIntegrityValidation: true,
    OnlyLoadAppFromAsar: true,
    LoadBrowserProcessSpecificV8Snapshot: false,
    GrantFileProtocolExtraPrivileges: false,
    WasmTrapHandlers: true,
  };
  assert.deepEqual(readableElectronFuseState(state), expected);
  assert.deepEqual(await readableFuseState("/tmp/Grok Desktop", async () => state), expected);
});

test("refuses to record fuse metadata when the executable no longer matches policy", async () => {
  const state = hardenedFuseWire();
  state[FuseV1Options.EnableNodeOptionsEnvironmentVariable] = FuseState.ENABLE;

  await assert.rejects(
    readVerifiedFuseState("/tmp/Grok Desktop", async () => state),
    /packaged Electron fuse verification failed for EnableNodeOptionsEnvironmentVariable/,
  );
  assert.equal(
    (await readableFuseState("/tmp/Grok Desktop", async () => state))
      .EnableNodeOptionsEnvironmentVariable,
    true,
  );
});

test("preserves the Windows packager fuse helper exports", () => {
  assert.equal(windowsHardenElectronExecutable, hardenElectronExecutable);
  assert.equal(windowsReadableFuseState, readableFuseState);
  assert.equal(windowsReadVerifiedFuseState, readVerifiedFuseState);
});
