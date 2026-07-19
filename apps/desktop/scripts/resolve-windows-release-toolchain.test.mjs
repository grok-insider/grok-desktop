import assert from "node:assert/strict";
import { mkdtemp, mkdir, writeFile, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import {
  buildMsvcLayout,
  formatGithubEnvAssignments,
  selectNewestMsvcVersion,
  selectNewestWindowsSdkVersion,
} from "./resolve-windows-release-toolchain.mjs";

test("selects the newest MSVC toolset version", () => {
  assert.equal(
    selectNewestMsvcVersion(["14.38.33130", "14.44.35207", "14.40.33807"]),
    "14.44.35207",
  );
  assert.throws(() => selectNewestMsvcVersion([]), /no MSVC/);
  assert.throws(() => selectNewestMsvcVersion(["14.44"]), /invalid/);
});

test("selects the newest Windows SDK version", () => {
  assert.equal(
    selectNewestWindowsSdkVersion(["10.0.22621.0", "10.0.26100.0", "10.0.19041.0"]),
    "10.0.26100.0",
  );
  assert.throws(() => selectNewestWindowsSdkVersion([]), /no Windows SDK/);
  assert.throws(() => selectNewestWindowsSdkVersion(["10.0.26100"]), /invalid/);
});

test("builds a complete MSVC toolchain layout for x64", () => {
  const layout = buildMsvcLayout({
    architecture: "x64",
    visualStudioRoot: "C:\\BuildTools",
    msvcVersion: "14.44.35207",
    windowsSdkRoot: "C:\\Windows Kits\\10",
    windowsSdkVersion: "10.0.26100.0",
    systemRoot: "C:\\Windows",
  });
  assert.equal(
    layout.linkerPath,
    "C:\\BuildTools\\VC\\Tools\\MSVC\\14.44.35207\\bin\\Hostx64\\x64\\link.exe",
  );
  assert.equal(layout.toolchainEnvironment.visualCppInstallRoot, "C:\\BuildTools\\VC");
  assert.deepEqual(layout.toolchainEnvironment.executablePaths, [
    "C:\\BuildTools\\VC\\Tools\\MSVC\\14.44.35207\\bin\\Hostx64\\x64",
    "C:\\Windows\\System32",
  ]);
  assert.ok(layout.toolchainEnvironment.includePaths.some((entry) => entry.endsWith("\\ucrt")));
  assert.ok(layout.toolchainEnvironment.libraryPaths.some((entry) => entry.includes("\\um\\x64")));
});

test("materialize path can include an explicit perl directory for OpenSSL vendor builds", async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), "grok-toolchain-perl-"));
  try {
    const vs = path.join(root, "VS");
    const msvcBin = path.join(vs, "VC", "Tools", "MSVC", "14.44.35207", "bin", "Hostx64", "x64");
    const sdk = path.join(root, "Kits", "10");
    const sdkVersion = "10.0.26100.0";
    const perlBin = path.join(root, "Strawberry", "perl", "bin");
    for (const directory of [
      msvcBin,
      path.join(vs, "VC", "Tools", "MSVC", "14.44.35207", "include"),
      path.join(vs, "VC", "Tools", "MSVC", "14.44.35207", "lib", "x64"),
      path.join(sdk, "Include", sdkVersion, "ucrt"),
      path.join(sdk, "Include", sdkVersion, "um"),
      path.join(sdk, "Include", sdkVersion, "shared"),
      path.join(sdk, "Lib", sdkVersion, "ucrt", "x64"),
      path.join(sdk, "Lib", sdkVersion, "um", "x64"),
      path.join(root, "bin"),
      path.join(root, "System32"),
      perlBin,
    ]) {
      await mkdir(directory, { recursive: true });
    }
    await writeFile(path.join(msvcBin, "link.exe"), "");
    await writeFile(path.join(root, "bin", "cargo.exe"), "");
    await writeFile(path.join(root, "bin", "rustc.exe"), "");
    await writeFile(path.join(perlBin, "perl.exe"), "");

    const { resolveWindowsReleaseToolchain } = await import("./resolve-windows-release-toolchain.mjs");
    const resolved = await resolveWindowsReleaseToolchain({
      allowNonWindows: true,
      skipCargoHydration: true,
      architecture: "x64",
      cargoPath: path.join(root, "bin", "cargo.exe"),
      rustcPath: path.join(root, "bin", "rustc.exe"),
      visualStudioRoot: vs,
      msvcVersion: "14.44.35207",
      windowsSdkRoot: sdk,
      windowsSdkVersion: sdkVersion,
      systemRoot: root,
      temporaryRoot: path.join(root, "tmp"),
      cargoCache: path.join(root, "cargo-cache"),
      perlDirectory: perlBin,
    });
    assert.equal(resolved.toolchainEnvironment.executablePaths[0], perlBin);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("formats GITHUB_ENV assignments with a JSON heredoc", () => {
  const text = formatGithubEnvAssignments({
    cargoPath: "C:\\Rust\\bin\\cargo.exe",
    rustcPath: "C:\\Rust\\bin\\rustc.exe",
    linkerPath: "C:\\BuildTools\\VC\\Tools\\MSVC\\14.44.35207\\bin\\Hostx64\\x64\\link.exe",
    cargoCache: "C:\\Temp\\cargo-cache",
    toolchainEnvironmentJSON: "{\"systemRoot\":\"C:\\\\Windows\"}",
  });
  assert.match(text, /^GROK_WINDOWS_CARGO_PATH=C:\\Rust\\bin\\cargo\.exe$/m);
  assert.match(text, /GROK_WINDOWS_TOOLCHAIN_ENV_JSON<<GROK_TOOLCHAIN_EOF/);
  assert.match(text, /"systemRoot":"C:\\\\Windows"/);
  assert.match(text, /GROK_TOOLCHAIN_EOF\n$/);
});

test("rejects empty toolchain exports", () => {
  assert.throws(
    () => formatGithubEnvAssignments({
      cargoPath: "",
      rustcPath: "C:\\Rust\\bin\\rustc.exe",
      linkerPath: "C:\\link.exe",
      cargoCache: "C:\\cache",
      toolchainEnvironmentJSON: "{}",
    }),
    /GROK_WINDOWS_CARGO_PATH/,
  );
});

test("can materialize a skip-hydration cache directory layout for local unit use", async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), "grok-toolchain-test-"));
  try {
    const vs = path.join(root, "VS");
    const msvcBin = path.join(vs, "VC", "Tools", "MSVC", "14.44.35207", "bin", "Hostx64", "x64");
    const sdk = path.join(root, "Kits", "10");
    const sdkVersion = "10.0.26100.0";
    for (const directory of [
      msvcBin,
      path.join(vs, "VC", "Tools", "MSVC", "14.44.35207", "include"),
      path.join(vs, "VC", "Tools", "MSVC", "14.44.35207", "lib", "x64"),
      path.join(sdk, "Include", sdkVersion, "ucrt"),
      path.join(sdk, "Include", sdkVersion, "um"),
      path.join(sdk, "Include", sdkVersion, "shared"),
      path.join(sdk, "Lib", sdkVersion, "ucrt", "x64"),
      path.join(sdk, "Lib", sdkVersion, "um", "x64"),
      path.join(root, "bin"),
      path.join(root, "System32"),
    ]) {
      await mkdir(directory, { recursive: true });
    }
    await writeFile(path.join(msvcBin, "link.exe"), "");
    await writeFile(path.join(root, "bin", "cargo.exe"), "");
    await writeFile(path.join(root, "bin", "rustc.exe"), "");

    const { resolveWindowsReleaseToolchain } = await import("./resolve-windows-release-toolchain.mjs");
    const resolved = await resolveWindowsReleaseToolchain({
      allowNonWindows: true,
      skipCargoHydration: true,
      architecture: "x64",
      cargoPath: path.join(root, "bin", "cargo.exe"),
      rustcPath: path.join(root, "bin", "rustc.exe"),
      visualStudioRoot: vs,
      msvcVersion: "14.44.35207",
      windowsSdkRoot: sdk,
      windowsSdkVersion: sdkVersion,
      systemRoot: root,
      temporaryRoot: path.join(root, "tmp"),
      cargoCache: path.join(root, "cargo-cache"),
    });
    assert.equal(resolved.linkerPath, path.join(msvcBin, "link.exe"));
    assert.match(resolved.toolchainEnvironmentJSON, /visualCppInstallRoot/);
    assert.equal(resolved.cargoCache, path.join(root, "cargo-cache"));
    assert.ok(resolved.toolchainEnvironment.executablePaths.includes(path.join(root, "System32")));
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
