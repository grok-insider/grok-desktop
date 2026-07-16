import {
  flipFuses,
  FuseState,
  FuseV1Options,
  FuseVersion,
  getCurrentFuseWire,
} from "@electron/fuses";

export const ELECTRON_FUSE_POLICY = Object.freeze([
  Object.freeze({ option: FuseV1Options.RunAsNode, enabled: false }),
  Object.freeze({ option: FuseV1Options.EnableCookieEncryption, enabled: true }),
  Object.freeze({ option: FuseV1Options.EnableNodeOptionsEnvironmentVariable, enabled: false }),
  Object.freeze({ option: FuseV1Options.EnableNodeCliInspectArguments, enabled: false }),
  Object.freeze({ option: FuseV1Options.EnableEmbeddedAsarIntegrityValidation, enabled: true }),
  Object.freeze({ option: FuseV1Options.OnlyLoadAppFromAsar, enabled: true }),
  Object.freeze({ option: FuseV1Options.LoadBrowserProcessSpecificV8Snapshot, enabled: false }),
  Object.freeze({ option: FuseV1Options.GrantFileProtocolExtraPrivileges, enabled: false }),
  Object.freeze({ option: FuseV1Options.WasmTrapHandlers, enabled: true }),
]);

const defaultFuseOperations = Object.freeze({ flipFuses, getCurrentFuseWire });

export function electronFuseConfiguration() {
  const configuration = {
    version: FuseVersion.V1,
    strictlyRequireAllFuses: true,
  };
  for (const { option, enabled } of ELECTRON_FUSE_POLICY) {
    configuration[option] = enabled;
  }
  return configuration;
}

export function assertHardenedElectronFuseState(state) {
  for (const { option, enabled } of ELECTRON_FUSE_POLICY) {
    const expected = enabled ? FuseState.ENABLE : FuseState.DISABLE;
    if (state[option] !== expected) {
      throw new Error(
        `packaged Electron fuse verification failed for ${FuseV1Options[option]}`,
      );
    }
  }
}

export function readableElectronFuseState(state) {
  return Object.fromEntries(
    Object.entries(FuseV1Options)
      .filter(([name]) => Number.isNaN(Number(name)))
      .map(([name, option]) => [name, state[option] === FuseState.ENABLE]),
  );
}

export async function hardenElectronExecutable(executable, operations = defaultFuseOperations) {
  await operations.flipFuses(executable, electronFuseConfiguration());
  const state = await operations.getCurrentFuseWire(executable);
  assertHardenedElectronFuseState(state);
}

export async function readableFuseState(executable, readCurrentFuseWire = getCurrentFuseWire) {
  return readableElectronFuseState(await readCurrentFuseWire(executable));
}

export async function readVerifiedFuseState(executable, readCurrentFuseWire = getCurrentFuseWire) {
  const state = await readCurrentFuseWire(executable);
  assertHardenedElectronFuseState(state);
  return readableElectronFuseState(state);
}
