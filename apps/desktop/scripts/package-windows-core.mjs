import { cp, mkdtemp, mkdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { packager } from "@electron/packager";
import { Arch, build, Platform } from "electron-builder";

import {
  assertUnsignedPortableExecutable,
  createUnsignedCoreWindowsInstallerConfiguration,
  parseReleaseArguments,
  readUnsignedCoreWindowsReleaseEnvironment,
  sha256File,
  validateCoreWindowsInputs,
  verifyPackagedCoreWindowsLayout,
} from "./release-utils.mjs";
import {
  assertReleaseBuildExists,
  electronPackagerMetadata,
  hardenElectronExecutable,
  preparePackagingSource,
  readVerifiedFuseState,
} from "./package-windows.mjs";

const desktopRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const repositoryRoot = path.resolve(desktopRoot, "../..");
const productName = "Grok Desktop";

async function main() {
  if (process.platform !== "win32") {
    throw new Error("Windows packages must be assembled on a Windows release worker");
  }
  const releaseArguments = parseReleaseArguments(process.argv.slice(2));
  if (releaseArguments.architecture !== "x64") {
    throw new Error("the public core package currently supports Windows x64 only");
  }
  const environment = readUnsignedCoreWindowsReleaseEnvironment(process.env);
  const packageMetadata = JSON.parse(await readFile(path.join(desktopRoot, "package.json"), "utf8"));
  const stageRoot = path.resolve(releaseArguments.stage ??
    path.join(repositoryRoot, "out", "release-inputs", "windows-core", "x64"));
  const outputRoot = path.resolve(releaseArguments.out ??
    path.join(repositoryRoot, "out", "release", "windows", releaseArguments.channel, "x64"));
  const inputs = await validateCoreWindowsInputs(stageRoot, { architecture: "x64" });
  await assertReleaseBuildExists();

  const temporaryRoot = await mkdtemp(path.join(os.tmpdir(), "grok-desktop-core-release-"));
  try {
    const sourceRoot = path.join(temporaryRoot, "source");
    await preparePackagingSource(sourceRoot, packageMetadata, environment.updateTrustedKeysJSON);
    await rm(outputRoot, { recursive: true, force: true });
    await mkdir(outputRoot, { recursive: true });
    const packagedDirectories = await packager({
      dir: sourceRoot,
      out: path.join(outputRoot, "unpacked"),
      overwrite: true,
      platform: "win32",
      arch: "x64",
      name: productName,
      executableName: productName,
      appVersion: packageMetadata.version,
      buildVersion: packageMetadata.version,
      ...electronPackagerMetadata(packageMetadata),
      appCopyright: "Copyright (c) 2026 Grok Insider",
      asar: true,
      prune: true,
      derefSymlinks: false,
      icon: path.join(desktopRoot, "release", "windows", "assets", "icon.ico"),
      extraResource: [
        path.join(sourceRoot, "update-trusted-keys.json"),
        path.join(desktopRoot, "assets", "tray"),
        path.join(inputs.canonicalRoot, "bin"),
      ],
    });
    if (packagedDirectories.length !== 1) {
      throw new Error("Electron Packager returned an unexpected target set");
    }
    const appDirectory = packagedDirectories[0];
    const executable = path.join(appDirectory, `${productName}.exe`);
    await verifyPackagedCoreWindowsLayout(appDirectory, inputs, "x64");
    await hardenElectronExecutable(executable);
    await verifyPackagedCoreWindowsLayout(appDirectory, inputs, "x64");

    const packageName = `GrokDesktop-${releaseArguments.channel}-x64.exe`;
    const builderOutput = path.join(temporaryRoot, "nsis");
    const builderConfiguration = createUnsignedCoreWindowsInstallerConfiguration({
      artifactName: packageName,
      electronVersion: electronPackagerMetadata(packageMetadata).electronVersion,
      iconPath: path.join(desktopRoot, "release", "windows", "assets", "icon.ico"),
      includePath: path.join(desktopRoot, "release", "windows", "nsis-installer.nsh"),
      outputDirectory: builderOutput,
    });
    const artifacts = await build({
      targets: Platform.WINDOWS.createTarget("nsis", Arch.x64),
      projectDir: desktopRoot,
      prepackaged: appDirectory,
      config: builderConfiguration,
      publish: "never",
    });
    const nsisPackage = path.join(builderOutput, packageName);
    if (!artifacts.some((artifact) => path.resolve(artifact) === path.resolve(nsisPackage))) {
      throw new Error("Electron Builder did not return the canonical NSIS artifact");
    }
    const sourceMetadata = await stat(nsisPackage);
    if (!sourceMetadata.isFile() || sourceMetadata.size < 1) {
      throw new Error("Electron Builder returned an invalid NSIS artifact");
    }
    await assertUnsignedPortableExecutable(nsisPackage);
    const sourceSha256 = await sha256File(nsisPackage);
    await verifyPackagedCoreWindowsLayout(appDirectory, inputs, "x64");
    const finalPackage = path.join(outputRoot, packageName);
    await cp(nsisPackage, finalPackage, { errorOnExist: true });
    const metadata = await stat(finalPackage);
    const artifactSha256 = await sha256File(finalPackage);
    if (!metadata.isFile() || metadata.size !== sourceMetadata.size || artifactSha256 !== sourceSha256) {
      throw new Error("copied NSIS artifact does not match the builder output");
    }
    const signing = await assertUnsignedPortableExecutable(finalPackage);
    const releaseRecord = {
      schemaVersion: 5,
      product: "grok-desktop",
      capabilityProfile: "core-host-tools-beta",
      deferredCapabilities: ["isolated-work", "media", "browser-automation", "scheduled-work"],
      version: packageMetadata.version,
      channel: releaseArguments.channel,
      architecture: "x64",
      format: "nsis",
      codeSigning: signing.codeSigning,
      applicationId: "com.grokinsider.grokdesktop",
      installScope: "per-user",
      minimumWindowsVersion: "10.0.22000.0",
      maxTestedWindowsVersion: environment.maxTestedVersion,
      protocolSchemes: ["grok-desktop"],
      officialGrokComponent: {
        version: inputs.manifest.version,
        sourceUrl: inputs.manifest.sourceUrl,
        sha256: inputs.manifest.sha256,
        size: inputs.manifest.size,
        trustBinding: inputs.manifest.binding,
        authenticodePolicy: "preserve-vendor-signature-do-not-resign",
        provenanceEvidenceId: environment.acpProvenanceEvidenceID,
        redistributionEvidenceId: environment.acpRedistributionEvidenceID,
      },
      artifact: {
        file: packageName, size: metadata.size, sha256: artifactSha256,
      },
      fuses: await readVerifiedFuseState(executable),
    };
    await writeFile(
      path.join(outputRoot, "windows-package.json"),
      `${JSON.stringify(releaseRecord, null, 2)}\n`,
      { encoding: "utf8", mode: 0o600, flag: "wx" },
    );
  } finally {
    await rm(temporaryRoot, { recursive: true, force: true });
  }
}

await main();
